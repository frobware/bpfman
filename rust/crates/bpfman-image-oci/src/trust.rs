//! Synchronous TUF orchestration. All signature/metadata authentication is
//! delegated to sigstore-tuf's transport-free TrustedMetadataSet.
use crate::{Error, registry::check};
use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use sigstore_tuf::{Metadata, Root, TargetFile, TrustedMetadataSet};
use std::{sync::atomic::AtomicBool, time::Duration};

const BOOTSTRAP: &[u8] = include_bytes!("tuf-root.json");
const REPOSITORY: &str = "https://tuf-repo-cdn.sigstore.dev";
const TARGET: &str = "trusted_root.json";
const ROOT_LIMIT: usize = 512_000;
const TIMESTAMP_LIMIT: usize = 16_384;
const METADATA_LIMIT: usize = 5_000_000;
const TARGET_LIMIT: usize = 4 * 1024 * 1024;
const ROTATIONS: usize = 32;
const REFRESH_SECONDS: i64 = 24 * 60 * 60;

#[derive(Clone, Debug)]
pub(super) struct TrustCache {
    pub(super) layout: RuntimeLayout,
    pub(super) timeout: Duration,
}

/// One atomic generation includes authenticated version floors, even when a
/// refresh failed after accepting newer metadata. Such an incomplete generation
/// cannot authorize target use. Timestamp and snapshot floors are authenticated
/// independently so rotating only timestamp keys cannot erase snapshot history.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    version: u32,
    roots: Vec<String>,
    timestamp: Option<String>,
    snapshot: Option<String>,
    targets: Option<String>,
    trusted_root: Option<String>,
    refreshed_at: Option<i64>,
}

struct Session {
    saved: Snapshot,
    trusted: TrustedMetadataSet,
    snapshot_floor: Option<sigstore_tuf::Snapshot>,
}

fn tuf<T>(result: sigstore_tuf::Result<T>) -> Result<T, Error> {
    result.map_err(|e| Error::caused("verify Sigstore TUF metadata", e))
}

fn text(bytes: Vec<u8>) -> Result<String, Error> {
    String::from_utf8(bytes).map_err(|e| Error::caused("decode Sigstore TUF UTF-8", e))
}

impl Session {
    fn load(saved: Option<Snapshot>, bootstrap: &[u8]) -> Result<Self, Error> {
        let saved = saved.unwrap_or(Snapshot {
            version: 1,
            ..Snapshot::default()
        });
        if saved.version != 1 || saved.roots.len() > 512 {
            return Err(Error::invalid("unsupported Sigstore TUF cache generation"));
        }
        let mut trusted = tuf(TrustedMetadataSet::from_root(bootstrap))?;
        let pinned_version = trusted.root().version;
        for bytes in &saved.roots {
            if bytes.len() > ROOT_LIMIT {
                return Err(Error::invalid("cached TUF root exceeds size limit"));
            }
            let root = tuf(Metadata::<Root>::from_slice(bytes.as_bytes()))?;
            // A reviewed bootstrap upgrade supersedes earlier cached history.
            if root.signed().version > pinned_version {
                tuf(trusted.update_root(bytes.as_bytes()))?;
            }
        }
        let mut session = Self {
            saved,
            trusted,
            snapshot_floor: None,
        };
        session.seed()?;
        Ok(session)
    }

    fn seed(&mut self) -> Result<(), Error> {
        // Expired authenticated metadata establishes floors only. Snapshot
        // signatures independently authenticate its floor under the current
        // snapshot authority, even when timestamp authority has rotated. It
        // cannot authorize a target without current timestamp pinning below.
        if let Some(timestamp) = &self.saved.timestamp {
            match self
                .trusted
                .update_timestamp(timestamp.as_bytes(), jiff::Timestamp::MIN)
            {
                Ok(()) | Err(sigstore_tuf::Error::EqualVersion { .. }) => {}
                Err(error) => return tuf(Err(error)),
            }
        }
        self.snapshot_floor = None;
        if let Some(snapshot) = &self.saved.snapshot {
            let metadata = tuf(Metadata::<sigstore_tuf::Snapshot>::from_slice(
                snapshot.as_bytes(),
            ))?;
            let authority = self
                .trusted
                .root()
                .role("snapshot")
                .ok_or_else(|| Error::invalid("TUF root has no snapshot role"))?;
            tuf(metadata.verify_threshold(&self.trusted.root().keys, authority, "snapshot"))?;
            let floor = metadata.into_signed();
            if self
                .trusted
                .timestamp()
                .and_then(|ts| ts.snapshot_meta())
                .map(|pin| pin.version)
                == Some(floor.version)
            {
                tuf(self
                    .trusted
                    .update_snapshot(snapshot.as_bytes(), jiff::Timestamp::MIN))?;
            }
            self.snapshot_floor = Some(floor);
        }
        Ok(())
    }

    fn check_snapshot_floor(&self) -> Result<(), Error> {
        let Some(floor) = &self.snapshot_floor else {
            return Ok(());
        };
        let snapshot = self
            .trusted
            .snapshot()
            .ok_or_else(|| Error::invalid("missing authenticated TUF snapshot"))?;
        if snapshot.version < floor.version
            || floor.meta.iter().any(|(name, pin)| {
                snapshot
                    .meta
                    .get(name)
                    .is_none_or(|new| new.version < pin.version)
            })
        {
            return Err(Error::invalid(
                "Sigstore TUF snapshot rollback or target metadata removal",
            ));
        }
        Ok(())
    }

    fn current(&mut self, now: jiff::Timestamp) -> Result<String, Error> {
        tuf(self.trusted.check_root_expired(now))?;
        tuf(self.trusted.check_timestamp_expired(now))?;
        tuf(self.trusted.check_snapshot_expired(now))?;
        let timestamp = self
            .trusted
            .timestamp()
            .ok_or_else(|| Error::invalid("missing cached TUF timestamp"))?;
        let snapshot = self
            .trusted
            .snapshot()
            .ok_or_else(|| Error::invalid("missing cached TUF snapshot"))?;
        if timestamp.snapshot_meta().map(|pin| pin.version) != Some(snapshot.version) {
            return Err(Error::invalid("incomplete Sigstore TUF refresh"));
        }
        let targets = self
            .saved
            .targets
            .as_ref()
            .ok_or_else(|| Error::invalid("missing cached TUF targets"))?;
        let targets = tuf(self.trusted.update_targets(targets.as_bytes(), now))?;
        let target = targets
            .target(TARGET)
            .ok_or_else(|| Error::invalid("Sigstore TUF target is not in top-level targets"))?;
        let bytes = self
            .saved
            .trusted_root
            .as_ref()
            .ok_or_else(|| Error::invalid("missing cached Sigstore trust root"))?;
        verify_target(bytes.as_bytes(), target)?;
        validate_root(bytes)?;
        Ok(bytes.clone())
    }

    fn fresh(&self, now: jiff::Timestamp) -> bool {
        self.saved.refreshed_at.is_some_and(|last| {
            let age = now.as_second().checked_sub(last);
            age.is_some_and(|age| (0..REFRESH_SECONDS).contains(&age))
        })
    }

    fn refresh(
        &mut self,
        fetch: &mut impl FnMut(&str, usize) -> Result<Option<Vec<u8>>, Error>,
        now: jiff::Timestamp,
        flag: &AtomicBool,
        mut persist: impl FnMut(&Snapshot) -> Result<(), Error>,
    ) -> Result<String, Error> {
        check(flag)?;
        let before = self.trusted.root().clone();
        let mut ended = false;
        for _ in 0..ROTATIONS {
            let next = self
                .trusted
                .root()
                .version
                .checked_add(1)
                .ok_or_else(|| Error::invalid("TUF root version overflow"))?;
            let Some(bytes) = fetch(&format!("{next}.root.json"), ROOT_LIMIT)? else {
                ended = true;
                break;
            };
            check(flag)?;
            tuf(self.trusted.update_root(&bytes))?;
            self.saved.roots.push(text(bytes)?);
            if self.saved.roots.len() > 512 {
                return Err(Error::invalid(
                    "Sigstore TUF root history limit exceeded; upgrade the bundled bootstrap",
                ));
            }
            // Root progress must survive later failures and process cancellation.
            // Clear lower roles only when their signing authority changed.
            let timestamp_same = same_authority(&before, self.trusted.root(), "timestamp");
            let snapshot_same = same_authority(&before, self.trusted.root(), "snapshot");
            if !timestamp_same {
                self.saved.timestamp = None;
            }
            if !snapshot_same {
                self.saved.snapshot = None;
            }
            self.saved.refreshed_at = None;
            persist(&self.saved)?;
        }
        if !ended {
            return Err(Error::invalid("Sigstore TUF root rotation limit exceeded"));
        }
        tuf(self.trusted.check_root_expired(now))?;
        if before.version != self.trusted.root().version {
            self.seed()?;
        }
        let timestamp = required(fetch, "timestamp.json", TIMESTAMP_LIMIT)?;
        check(flag)?;
        match self.trusted.update_timestamp(&timestamp, now) {
            Ok(()) => {
                self.saved.timestamp = Some(text(timestamp)?);
                self.saved.refreshed_at = None;
                persist(&self.saved)?;
            }
            Err(sigstore_tuf::Error::EqualVersion { .. }) => {}
            Err(error) => return tuf(Err(error)),
        }
        // Equal-version timestamp replay must not bypass expiry.
        tuf(self.trusted.check_timestamp_expired(now))?;
        let pin = self
            .trusted
            .timestamp()
            .and_then(|t| t.snapshot_meta())
            .ok_or_else(|| Error::invalid("TUF timestamp does not pin snapshot"))?;
        let name = if self.trusted.root().consistent_snapshot {
            format!("{}.snapshot.json", pin.version)
        } else {
            "snapshot.json".into()
        };
        let bytes = required(fetch, &name, METADATA_LIMIT)?;
        check(flag)?;
        tuf(self.trusted.update_snapshot(&bytes, now))?;
        self.check_snapshot_floor()?;
        self.snapshot_floor = self.trusted.snapshot().cloned();
        self.saved.snapshot = Some(text(bytes)?);
        persist(&self.saved)?;
        let pin = self
            .trusted
            .snapshot()
            .and_then(|s| s.meta.get("targets.json"))
            .ok_or_else(|| Error::invalid("TUF snapshot does not pin targets"))?;
        let name = if self.trusted.root().consistent_snapshot {
            format!("{}.targets.json", pin.version)
        } else {
            "targets.json".into()
        };
        let bytes = required(fetch, &name, METADATA_LIMIT)?;
        check(flag)?;
        let targets = tuf(self.trusted.update_targets(&bytes, now))?;
        let target = targets
            .target(TARGET)
            .cloned()
            .ok_or_else(|| Error::invalid("Sigstore TUF target is not in top-level targets"))?;
        self.saved.targets = Some(text(bytes)?);
        persist(&self.saved)?;
        // Production currently publishes trusted_root.json directly in the top
        // level. Do not search unbounded/unneeded delegated repositories.
        let digest = target
            .hashes
            .get("sha256")
            .ok_or_else(|| Error::invalid("Sigstore TUF target requires SHA256"))?;
        crate::reference::validate_digest(&format!("sha256:{digest}"))?;
        let path = if self.trusted.root().consistent_snapshot {
            format!("targets/{digest}.{TARGET}")
        } else {
            format!("targets/{TARGET}")
        };
        if target.length > TARGET_LIMIT as u64 {
            return Err(Error::invalid("Sigstore trust root exceeds size limit"));
        }
        let bytes = required(fetch, &path, target.length as usize)?;
        check(flag)?;
        verify_target(&bytes, &target)?;
        let bytes = text(bytes)?;
        validate_root(&bytes)?;
        self.saved.trusted_root = Some(bytes.clone());
        self.saved.refreshed_at = Some(now.as_second());
        persist(&self.saved)?;
        Ok(bytes)
    }
}

fn same_authority(old: &Root, new: &Root, role: &str) -> bool {
    let (Some(old_role), Some(new_role)) = (old.role(role), new.role(role)) else {
        return false;
    };
    let old_ids: std::collections::BTreeSet<_> = old_role.keyids.iter().collect();
    let new_ids: std::collections::BTreeSet<_> = new_role.keyids.iter().collect();
    old_role.threshold == new_role.threshold
        && old_ids == new_ids
        && old_ids
            .iter()
            .all(|id| old.keys.get(*id) == new.keys.get(*id))
}

fn validate_root(bytes: &str) -> Result<(), Error> {
    let root = sigstore_verify::trust_root::TrustedRoot::from_json(bytes)
        .map_err(|e| Error::caused("decode TUF-authenticated Sigstore trust root", e))?;
    sigstore_verify::Verifier::new(&root)
        .map_err(|e| Error::caused("prepare TUF-authenticated Sigstore trust root", e))?;
    Ok(())
}

fn verify_target(bytes: &[u8], target: &TargetFile) -> Result<(), Error> {
    if bytes.len() as u64 != target.length || bytes.len() > TARGET_LIMIT {
        return Err(Error::invalid("Sigstore TUF target length mismatch"));
    }
    let mut checked = false;
    for (algorithm, expected) in &target.hashes {
        let actual = match algorithm.as_str() {
            "sha256" => format!("{:x}", Sha256::digest(bytes)),
            "sha512" => format!("{:x}", Sha512::digest(bytes)),
            _ => continue,
        };
        if !actual.eq_ignore_ascii_case(expected) {
            return Err(Error::invalid("Sigstore TUF target digest mismatch"));
        }
        checked = true;
    }
    if !checked {
        return Err(Error::invalid(
            "Sigstore TUF target has no supported digest",
        ));
    }
    Ok(())
}

fn required(
    fetch: &mut impl FnMut(&str, usize) -> Result<Option<Vec<u8>>, Error>,
    name: &str,
    limit: usize,
) -> Result<Vec<u8>, Error> {
    fetch(name, limit)?.ok_or_else(|| Error::invalid(format!("Sigstore TUF file missing: {name}")))
}

fn read(directory: &RuntimeDirectory) -> Result<Option<Snapshot>, Error> {
    directory
        .read_trust_cache()
        .map_err(|e| Error::caused("read Sigstore TUF cache", e))?
        .map(|bytes| {
            serde_json::from_slice(&bytes)
                .map_err(|e| Error::caused("decode Sigstore TUF cache", e))
        })
        .transpose()
}

fn persist(
    writer: &RuntimeWriter<'_>,
    snapshot: &Snapshot,
    flag: &AtomicBool,
) -> Result<(), Error> {
    let bytes =
        serde_json::to_vec(snapshot).map_err(|e| Error::caused("encode Sigstore TUF cache", e))?;
    publish_snapshot(writer, &bytes, || {
        flag.load(std::sync::atomic::Ordering::Relaxed)
    })
}

fn publish_snapshot(
    writer: &RuntimeWriter<'_>,
    bytes: &[u8],
    mut cancelled: impl FnMut() -> bool,
) -> Result<(), Error> {
    if cancelled() {
        return Err(Error::cancelled());
    }
    let stage = writer
        .stage_trust_cache(bytes)
        .map_err(|e| Error::caused("stage Sigstore TUF cache", e))?;
    if cancelled() {
        let error = Error::cancelled();
        return Err(match writer.remove_trust_cache_stage(stage) {
            Ok(()) => error,
            Err(failure) => error.trust_cleanup_failed(failure.cause, failure.remaining),
        });
    }
    writer.publish_trust_cache(stage).map_err(|failure| {
        let error = Error::caused("publish Sigstore TUF cache", failure.cause);
        match failure.remaining {
            None => error,
            Some(stage) => match writer.remove_trust_cache_stage(stage) {
                Ok(()) => error,
                Err(failure) => error.trust_cleanup_failed(failure.cause, failure.remaining),
            },
        }
    })
}

pub(super) fn resolve(
    cache: &TrustCache,
    online: bool,
    flag: &AtomicBool,
) -> Result<String, Error> {
    resolve_at(
        cache,
        online,
        flag,
        BOOTSTRAP,
        REPOSITORY,
        jiff::Timestamp::now(),
    )
}

fn resolve_at(
    cache: &TrustCache,
    online: bool,
    flag: &AtomicBool,
    bootstrap: &[u8],
    repository: &str,
    now: jiff::Timestamp,
) -> Result<String, Error> {
    check(flag)?;
    if let Some(directory) = RuntimeDirectory::open_existing(cache.layout.clone())
        .map_err(|e| Error::caused("open Sigstore TUF cache", e))?
    {
        let mut session = Session::load(read(&directory)?, bootstrap)?;
        match session.current(now) {
            Ok(root) if !online || session.fresh(now) => return Ok(root),
            Err(error) if !online => return Err(error),
            _ => {}
        }
    }
    if !online {
        return Err(Error::invalid(
            "no valid Sigstore TUF cache and pull policy is Never",
        ));
    }
    check(flag)?;
    let directory = RuntimeDirectory::open_or_create(cache.layout.clone())
        .map_err(|e| Error::caused("prepare Sigstore TUF cache", e))?;
    directory
        .with_cache_writer(
            bpfman_lock::AcquireOptions {
                timeout: cache.timeout,
                cancelled: Some(flag),
            },
            |writer| {
                check(flag)?;
                // Readers remain lock-free; refreshers serialize and re-read after
                // acquisition so an older download can never overwrite a newer floor.
                let mut session = Session::load(read(writer.directory())?, bootstrap)?;
                if let Ok(root) = session.current(now) {
                    if session.fresh(now) {
                        return Ok(root);
                    }
                }
                let agent = crate::transport::agent(
                    ureq::Agent::config_builder()
                        .http_status_as_error(false)
                        .max_redirects(0)
                        .timeout_global(Some(Duration::from_secs(30)))
                        .build(),
                );
                session.refresh(
                    &mut |name, limit| fetch(&agent, repository, name, limit, flag),
                    now,
                    flag,
                    |snapshot| persist(&writer, snapshot, flag),
                )
            },
        )
        .map_err(|e| {
            if e.kind() == bpfman_fs::ErrorKind::Cancelled {
                Error::cancelled()
            } else {
                Error::caused("acquire Sigstore TUF writer", e)
            }
        })?
}

fn fetch(
    agent: &ureq::Agent,
    repository: &str,
    name: &str,
    limit: usize,
    flag: &AtomicBool,
) -> Result<Option<Vec<u8>>, Error> {
    check(flag)?;
    let response = agent
        .get(format!("{repository}/{name}"))
        .call()
        .map_err(|e| Error::caused("fetch Sigstore TUF file", e));
    check(flag)?;
    let mut response = response?;
    if response.status().as_u16() == 404 {
        return Ok(None);
    }
    if response.status().as_u16() != 200 {
        return Err(Error::invalid(format!(
            "Sigstore TUF HTTP status {}",
            response.status().as_u16()
        )));
    }
    use std::io::Read;
    let mut reader = response.body_mut().as_reader().take(limit as u64 + 1);
    let mut bytes = Vec::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        check(flag)?;
        let count = reader
            .read(&mut buffer)
            .map_err(|e| Error::caused("read Sigstore TUF file", e));
        check(flag)?;
        let count = count?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        if bytes.len() > limit {
            return Err(Error::invalid("Sigstore TUF download exceeds size limit"));
        }
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests;
