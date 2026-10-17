#![allow(clippy::expect_used)]
use super::*;
use serde_json::{Value, json};
use sigstore_crypto::KeyPair;
use std::{collections::BTreeMap, sync::atomic::Ordering};

use crate::fixture;

type Result = std::result::Result<(), Box<dyn std::error::Error>>;
const FUTURE: &str = "2099-01-01T00:00:00Z";
const PAST: &str = "2020-01-01T00:00:00Z";

fn now() -> jiff::Timestamp {
    "2026-10-17T12:00:00Z".parse().expect("time")
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

struct Signer {
    pair: KeyPair,
    id: String,
    key: Value,
}

impl Signer {
    fn new() -> Self {
        let pair = KeyPair::generate_ecdsa_p256().expect("key pair");
        let key = json!({"keytype":"ecdsa", "scheme":"ecdsa-sha2-nistp256", "keyval":{"public":pair.public_key_der().expect("public key").to_pem()}});
        let parsed: sigstore_tuf::Key = serde_json::from_value(key.clone()).expect("TUF key");
        Self {
            pair,
            id: parsed.key_id().expect("key ID"),
            key,
        }
    }

    fn signature(&self, signed: &Value) -> Value {
        let bytes = sigstore_tuf::canonical_json::to_canonical_bytes(signed).expect("canonical");
        json!({"keyid": self.id, "sig": hex(self.pair.sign(&bytes).expect("signature").as_bytes())})
    }

    fn sign(&self, signed: Value) -> Vec<u8> {
        serde_json::to_vec(&json!({"signatures":[self.signature(&signed)], "signed":signed}))
            .expect("metadata")
    }

    fn root_value(&self, version: u64) -> Value {
        let roles: BTreeMap<_, _> = ["root", "timestamp", "snapshot", "targets"]
            .into_iter()
            .map(|role| (role, json!({"keyids":[self.id], "threshold":1})))
            .collect();
        json!({"_type":"root", "spec_version":"1.0.0", "version":version, "expires":FUTURE, "consistent_snapshot":true, "keys":{&self.id: self.key}, "roles":roles})
    }
}

fn pin(bytes: &[u8], version: u64) -> Value {
    json!({"version":version, "length":bytes.len(), "hashes":{"sha256":hex(&Sha256::digest(bytes))}})
}

struct Repo {
    signer: Signer,
    bootstrap: Vec<u8>,
    files: BTreeMap<String, Vec<u8>>,
}

impl Repo {
    fn new() -> Self {
        let signer = Signer::new();
        let bootstrap = signer.sign(signer.root_value(1));
        let mut repo = Self {
            signer,
            bootstrap,
            files: BTreeMap::new(),
        };
        repo.release(
            1,
            FUTURE,
            sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT.as_bytes(),
        );
        repo
    }

    fn release(&mut self, version: u64, expiry: &str, material: &[u8]) {
        self.release_expiries(version, [expiry; 3], material);
    }

    fn release_expiries(&mut self, version: u64, expiry: [&str; 3], material: &[u8]) {
        let digest = hex(&Sha256::digest(material));
        let targets = self.signer.sign(json!({"_type":"targets", "spec_version":"1.0.0", "version":version, "expires":expiry[2], "targets":{TARGET:{"length":material.len(), "hashes":{"sha256":digest}}}}));
        let snapshot = self.signer.sign(json!({"_type":"snapshot", "spec_version":"1.0.0", "version":version, "expires":expiry[1], "meta":{"targets.json":pin(&targets,version)}}));
        let timestamp = self.signer.sign(json!({"_type":"timestamp", "spec_version":"1.0.0", "version":version, "expires":expiry[0], "meta":{"snapshot.json":pin(&snapshot,version)}}));
        self.files.insert("timestamp.json".into(), timestamp);
        self.files
            .insert(format!("{version}.snapshot.json"), snapshot);
        self.files
            .insert(format!("{version}.targets.json"), targets);
        self.files
            .insert(format!("targets/{digest}.{TARGET}"), material.into());
    }

    fn fetch(&self, name: &str, limit: usize) -> std::result::Result<Option<Vec<u8>>, Error> {
        let result = self.files.get(name).cloned();
        assert!(result.as_ref().is_none_or(|bytes| bytes.len() <= limit));
        Ok(result)
    }

    fn session(&self) -> Session {
        Session::load(None, &self.bootstrap).expect("bootstrap")
    }
}

fn refresh(session: &mut Session, repo: &Repo) -> std::result::Result<String, Error> {
    session.refresh(
        &mut |name, limit| repo.fetch(name, limit),
        now(),
        &AtomicBool::new(false),
        |_| Ok(()),
    )
}

#[test]
fn valid_metadata_is_authenticated_again_on_every_offline_read() -> Result {
    let repo = Repo::new();
    let mut session = repo.session();
    let material = refresh(&mut session, &repo)?;
    assert_eq!(
        material,
        sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT
    );
    let encoded = serde_json::to_vec(&session.saved)?;
    let mut reopened = Session::load(Some(serde_json::from_slice(&encoded)?), &repo.bootstrap)?;
    assert_eq!(reopened.current(now())?, material);
    for role in ["timestamp", "snapshot", "targets", "trusted_root"] {
        let mut corrupt: Value = serde_json::from_slice(&encoded)?;
        if role == "trusted_root" {
            corrupt[role] = json!(format!("{} ", corrupt[role].as_str().expect("target")));
        } else {
            let mut metadata: Value =
                serde_json::from_str(corrupt[role].as_str().expect("metadata"))?;
            metadata["signed"]["version"] = json!(900);
            corrupt[role] = json!(serde_json::to_string(&metadata)?);
        }
        let outcome = Session::load(Some(serde_json::from_value(corrupt)?), &repo.bootstrap)
            .and_then(|mut cached| cached.current(now()));
        assert!(outcome.is_err(), "tampered {role}");
    }
    Ok(())
}

#[test]
fn root_rotation_requires_old_and_new_thresholds_and_survives_reopen() -> Result {
    let mut repo = Repo::new();
    let original = repo.signer.root_value(1);
    let new_signer = Signer::new();
    let signed = new_signer.root_value(2);
    let signed_root = serde_json::to_vec(
        &json!({"signed":signed,"signatures":[repo.signer.signature(&signed),new_signer.signature(&signed)]}),
    )?;
    repo.files.insert("2.root.json".into(), signed_root.clone());
    repo.signer = new_signer;
    repo.release(
        2,
        FUTURE,
        sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT.as_bytes(),
    );
    let mut session = repo.session();
    refresh(&mut session, &repo)?;
    assert_eq!(session.trusted.root().version, 2);
    let mut reopened = Session::load(Some(session.saved.clone()), &repo.bootstrap)?;
    assert!(reopened.current(now()).is_ok());
    // A self-signed replacement without the old threshold is never trusted.
    let mut bad = repo.session();
    repo.files
        .insert("2.root.json".into(), repo.signer.sign(signed.clone()));
    assert!(refresh(&mut bad, &repo).is_err());
    assert_eq!(bad.trusted.root().version, 1);
    // Nor does authorization from the old root replace the new self-threshold.
    let mut metadata: Value = serde_json::from_slice(&signed_root)?;
    metadata["signatures"]
        .as_array_mut()
        .expect("signatures")
        .pop();
    repo.files
        .insert("2.root.json".into(), serde_json::to_vec(&metadata)?);
    assert!(refresh(&mut repo.session(), &repo).is_err());
    assert_eq!(original["version"], 1);
    Ok(())
}

#[test]
fn rollback_floors_survive_expiry_partial_refresh_and_unchanged_key_rotation() -> Result {
    let mut repo = Repo::new();
    repo.release(
        2,
        FUTURE,
        sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT.as_bytes(),
    );
    let mut session = repo.session();
    refresh(&mut session, &repo)?;
    let saved = session.saved.clone();
    let mut snapshots = Vec::new();
    repo.release(
        3,
        FUTURE,
        sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT.as_bytes(),
    );
    repo.files
        .insert("3.snapshot.json".into(), b"invalid".to_vec());
    assert!(
        session
            .refresh(
                &mut |name, limit| repo.fetch(name, limit),
                now(),
                &AtomicBool::new(false),
                |state| {
                    snapshots.push(state.clone());
                    Ok(())
                }
            )
            .is_err()
    );
    let last = snapshots.pop().expect("accepted timestamp persisted");
    assert_eq!(
        Session::load(Some(last.clone()), &repo.bootstrap)?
            .trusted
            .timestamp()
            .expect("timestamp")
            .version,
        3
    );
    // An older timestamp is refused after a process restart, despite the failed snapshot.
    repo.release(
        2,
        FUTURE,
        sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT.as_bytes(),
    );
    let mut reopened = Session::load(Some(last), &repo.bootstrap)?;
    assert!(refresh(&mut reopened, &repo).is_err());
    assert_eq!(reopened.trusted.timestamp().expect("floor").version, 3);
    // Rotating root without changing lower-role keys cannot discard version floors.
    repo.files.insert(
        "2.root.json".into(),
        repo.signer.sign(repo.signer.root_value(2)),
    );
    repo.release(
        1,
        FUTURE,
        sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT.as_bytes(),
    );
    let mut reopened = Session::load(Some(saved), &repo.bootstrap)?;
    assert!(refresh(&mut reopened, &repo).is_err());
    assert_eq!(
        reopened
            .trusted
            .timestamp()
            .expect("preserved floor")
            .version,
        2
    );
    // Fully authenticated expired metadata still supplies anti-rollback floors.
    let mut expired = Repo::new();
    expired.release(
        5,
        PAST,
        sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT.as_bytes(),
    );
    let mut session = expired.session();
    session.refresh(
        &mut |name, limit| expired.fetch(name, limit),
        "2019-01-01T00:00:00Z".parse()?,
        &AtomicBool::new(false),
        |_| Ok(()),
    )?;
    let mut reopened = Session::load(Some(session.saved), &expired.bootstrap)?;
    assert!(reopened.current(now()).is_err());
    expired.release(
        4,
        FUTURE,
        sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT.as_bytes(),
    );
    assert!(refresh(&mut reopened, &expired).is_err());
    // Replaying the same expired timestamp is an error, not an expiry bypass.
    expired.release(
        5,
        PAST,
        sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT.as_bytes(),
    );
    assert!(refresh(&mut reopened, &expired).is_err());
    Ok(())
}

#[test]
fn malformed_expired_and_mismatched_metadata_never_authorizes_a_target() -> Result {
    for failed in [
        "timestamp.json",
        "1.snapshot.json",
        "1.targets.json",
        "target",
    ] {
        let mut repo = Repo::new();
        if failed == "target" {
            let path = repo
                .files
                .keys()
                .find(|key| key.starts_with("targets/"))
                .expect("target")
                .clone();
            repo.files.get_mut(&path).expect("bytes")[0] ^= 1;
        } else {
            let mut metadata: Value = serde_json::from_slice(&repo.files[failed])?;
            metadata["signed"]["expires"] = json!(PAST);
            repo.files
                .insert(failed.into(), serde_json::to_vec(&metadata)?);
        }
        let mut session = repo.session();
        assert!(refresh(&mut session, &repo).is_err(), "{failed}");
        assert!(session.current(now()).is_err());
    }
    let mut repo = Repo::new();
    repo.release(
        1,
        PAST,
        sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT.as_bytes(),
    );
    assert!(refresh(&mut repo.session(), &repo).is_err());
    // Bootstrap root authenticity, not a cached self-signed root, defines trust.
    let mut foreign = repo.session().saved;
    let attacker = Signer::new();
    foreign
        .roots
        .push(text(attacker.sign(attacker.root_value(2)))?);
    assert!(Session::load(Some(foreign), &repo.bootstrap).is_err());
    Ok(())
}

#[test]
fn bounded_root_walk_and_cancellation_do_not_accept_partial_updates() -> Result {
    let mut repo = Repo::new();
    let flag = AtomicBool::new(false);
    let mut session = repo.session();
    let mut persisted = Vec::new();
    let error = session
        .refresh(
            &mut |name, limit| {
                let bytes = repo.fetch(name, limit)?;
                if name == "timestamp.json" {
                    flag.store(true, Ordering::Release);
                }
                Ok(bytes)
            },
            now(),
            &flag,
            |snapshot| {
                persisted.push(snapshot.clone());
                Ok(())
            },
        )
        .expect_err("cancelled");
    assert!(error.is_cancelled());
    assert!(persisted.is_empty());
    assert!(session.saved.trusted_root.is_none());
    for version in 2..=(ROTATIONS as u64 + 1) {
        repo.files.insert(
            format!("{version}.root.json"),
            repo.signer.sign(repo.signer.root_value(version)),
        );
    }
    assert!(
        refresh(&mut repo.session(), &repo)
            .expect_err("rotation bound")
            .to_string()
            .contains("rotation limit")
    );
    Ok(())
}

#[test]
fn http_cache_refresh_is_serialized_and_fresh_readers_need_no_lock_or_network() -> Result {
    let repo = Repo::new();
    let server = fixture::Registry::new(vec![], false);
    let endpoint = format!(
        "http://{}",
        server.reference.split('/').next().expect("host")
    );
    {
        let mut routes = server.responses.lock().expect("routes");
        for (path, bytes) in &repo.files {
            routes.insert(format!("/{path}"), (200, bytes.clone()));
        }
    }
    let temp = tempfile::tempdir()?;
    let cache = TrustCache {
        layout: RuntimeLayout::try_from(temp.path().join("trust"))?,
        timeout: Duration::from_secs(5),
    };
    let flag = AtomicBool::new(false);
    assert!(resolve_at(&cache, false, &flag, &repo.bootstrap, &endpoint, now()).is_err());
    assert!(!cache.layout.root().exists());
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| resolve_at(&cache, true, &flag, &repo.bootstrap, &endpoint, now()))
            })
            .collect();
        for worker in workers {
            worker.join().expect("refresh worker").expect("valid trust");
        }
    });
    assert_eq!(
        server.requests.load(Ordering::Relaxed),
        5,
        "only one five-request refresh"
    );
    let directory = RuntimeDirectory::open_existing(cache.layout.clone())?.expect("cache");
    directory.with_cache_writer(
        bpfman_lock::AcquireOptions {
            timeout: Duration::ZERO,
            cancelled: None,
        },
        |_| {
            assert!(resolve_at(&cache, false, &flag, &repo.bootstrap, &endpoint, now()).is_ok());
            assert!(resolve_at(&cache, true, &flag, &repo.bootstrap, &endpoint, now()).is_ok());
        },
    )?;
    assert!(!cache.layout.database_path().exists());
    let later = jiff::Timestamp::from_second(now().as_second() + REFRESH_SECONDS + 1)?;
    assert!(resolve_at(&cache, false, &flag, &repo.bootstrap, &endpoint, later).is_ok());
    assert_eq!(server.requests.load(Ordering::Relaxed), 5);
    assert!(resolve_at(&cache, true, &flag, &repo.bootstrap, &endpoint, later).is_ok());
    assert_eq!(server.requests.load(Ordering::Relaxed), 10);
    // The corpus fixture exposes these observations; no BPF fixture is required.
    assert!(server.digest.starts_with("sha256:"));
    assert!(!server.bearer.load(Ordering::Relaxed));
    Ok(())
}

#[test]
fn http_errors_redirects_and_size_limits_fail_without_bundled_fallback() -> Result {
    let repo = Repo::new();
    for (status, body) in [
        (401, vec![]),
        (403, vec![]),
        (503, vec![]),
        (302, vec![]),
        (200, vec![b' '; TIMESTAMP_LIMIT + 1]),
    ] {
        let server = fixture::Registry::new(vec![], false);
        let endpoint = format!(
            "http://{}",
            server.reference.split('/').next().expect("host")
        );
        server
            .responses
            .lock()
            .expect("routes")
            .insert("/timestamp.json".into(), (status, body));
        let temp = tempfile::tempdir()?;
        let cache = TrustCache {
            layout: RuntimeLayout::try_from(temp.path().join("trust"))?,
            timeout: Duration::ZERO,
        };
        assert!(
            resolve_at(
                &cache,
                true,
                &AtomicBool::new(false),
                &repo.bootstrap,
                &endpoint,
                now()
            )
            .is_err()
        );
        assert!(
            RuntimeDirectory::open_existing(cache.layout.clone())?
                .expect("cache")
                .read_trust_cache()?
                .is_none()
        );
        assert_eq!(server.requests.load(Ordering::Relaxed), 2);
    }
    Ok(())
}

#[test]
fn staged_cancellation_preserves_previous_generation_and_retains_failed_cleanup() -> Result {
    for replace_parent in [false, true] {
        let temp = tempfile::tempdir()?;
        let layout = RuntimeLayout::try_from(temp.path().join("trust"))?;
        let directory = RuntimeDirectory::open_or_create(layout.clone())?;
        directory.with_cache_writer(
            bpfman_lock::AcquireOptions {
                timeout: Duration::ZERO,
                cancelled: None,
            },
            |writer| {
                publish_snapshot(&writer, b"old complete generation", || false)
                    .expect("old generation");
                let collection = layout.root().join("sigstore-tuf-v1");
                let saved = layout.root().join("saved");
                let calls = std::cell::Cell::new(0);
                let mut error = publish_snapshot(&writer, b"cancelled new generation", || {
                    calls.set(calls.get() + 1);
                    if calls.get() == 2 {
                        if replace_parent {
                            std::fs::rename(&collection, &saved).expect("move collection");
                            std::fs::create_dir(&collection).expect("replace parent");
                        }
                        true
                    } else {
                        false
                    }
                })
                .expect_err("cancelled after stage");
                assert!(error.is_cancelled());
                assert_eq!(error.has_pending_cleanup(), replace_parent);
                if replace_parent {
                    assert!(error.retry_cleanup(&writer).is_err());
                    std::fs::rename(&collection, layout.root().join("foreign"))
                        .expect("save foreign");
                    std::fs::rename(&saved, &collection).expect("restore owned");
                    error.retry_cleanup(&writer).expect("explicit retry");
                }
                assert!(!error.has_pending_cleanup());
                assert_eq!(
                    directory.read_trust_cache().expect("read").expect("old"),
                    b"old complete generation"
                );
                assert_eq!(std::fs::read_dir(collection).expect("entries").count(), 1);
            },
        )?;
    }
    Ok(())
}

#[test]
fn every_role_requires_unexpired_authentic_metadata_and_snapshot_targets_cannot_roll_back() -> Result
{
    for role in ["root", "timestamp", "snapshot", "targets"] {
        let mut repo = Repo::new();
        let mut expiries = [FUTURE; 3];
        match role {
            "root" => {
                let mut root = repo.signer.root_value(1);
                root["expires"] = json!(PAST);
                repo.bootstrap = repo.signer.sign(root);
            }
            "timestamp" => expiries[0] = PAST,
            "snapshot" => expiries[1] = PAST,
            "targets" => expiries[2] = PAST,
            _ => unreachable!(),
        }
        repo.release_expiries(
            1,
            expiries,
            sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT.as_bytes(),
        );
        let mut session = repo.session();
        assert!(refresh(&mut session, &repo).is_err(), "expired {role}");
        assert!(session.current(now()).is_err(), "expired cached {role}");
    }
    let mut repo = Repo::new();
    repo.release(
        2,
        FUTURE,
        sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT.as_bytes(),
    );
    let mut session = repo.session();
    refresh(&mut session, &repo)?;
    // All signatures and cross-role hashes are valid. Only the per-target
    // rollback floor makes this newer timestamp/snapshot reject old targets.
    let targets = repo.files["1.targets.json"].clone();
    let snapshot = repo.signer.sign(json!({"_type":"snapshot", "spec_version":"1.0.0", "version":3, "expires":FUTURE, "meta":{"targets.json":pin(&targets,1)}}));
    let timestamp = repo.signer.sign(json!({"_type":"timestamp", "spec_version":"1.0.0", "version":3, "expires":FUTURE, "meta":{"snapshot.json":pin(&snapshot,3)}}));
    repo.files.insert("timestamp.json".into(), timestamp);
    repo.files.insert("3.snapshot.json".into(), snapshot);
    assert!(
        refresh(&mut session, &repo)
            .expect_err("target rollback")
            .to_string()
            .contains("TUF")
    );
    Ok(())
}

#[test]
fn reordering_root_role_key_ids_does_not_discard_authenticated_floors() -> Result {
    let mut repo = Repo::new();
    let other = Signer::new();
    let mut root = repo.signer.root_value(1);
    root["keys"][&other.id] = other.key.clone();
    for role in ["timestamp", "snapshot"] {
        root["roles"][role]["keyids"] = json!([repo.signer.id, other.id]);
    }
    repo.bootstrap = repo.signer.sign(root.clone());
    repo.release(
        2,
        FUTURE,
        sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT.as_bytes(),
    );
    let mut session = repo.session();
    refresh(&mut session, &repo)?;
    root["version"] = json!(2);
    for role in ["timestamp", "snapshot"] {
        root["roles"][role]["keyids"] = json!([other.id, repo.signer.id]);
    }
    repo.files
        .insert("2.root.json".into(), repo.signer.sign(root));
    repo.release(
        1,
        FUTURE,
        sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT.as_bytes(),
    );
    assert!(refresh(&mut session, &repo).is_err());
    assert_eq!(
        session
            .trusted
            .timestamp()
            .expect("preserved timestamp floor")
            .version,
        2
    );
    Ok(())
}

#[test]
fn timestamp_key_rotation_preserves_snapshot_floors_across_reopened_failed_refresh() -> Result {
    let mut repo = Repo::new();
    repo.release(
        2,
        FUTURE,
        sigstore_verify::trust_root::SIGSTORE_PRODUCTION_TRUSTED_ROOT.as_bytes(),
    );
    let mut session = repo.session();
    refresh(&mut session, &repo)?;
    let new_timestamp = Signer::new();
    let mut root = repo.signer.root_value(2);
    root["keys"][&new_timestamp.id] = new_timestamp.key.clone();
    root["roles"]["timestamp"]["keyids"] = json!([new_timestamp.id]);
    repo.files
        .insert("2.root.json".into(), repo.signer.sign(root));
    // Snapshot version 1 was validly signed before timestamp-key rotation.
    // A fresh timestamp signature cannot reset the observed snapshot floor 2.
    let snapshot = repo.files["1.snapshot.json"].clone();
    let timestamp = new_timestamp.sign(json!({"_type":"timestamp", "spec_version":"1.0.0", "version":1, "expires":FUTURE, "meta":{"snapshot.json":pin(&snapshot,1)}}));
    repo.files.insert("timestamp.json".into(), timestamp);
    let mut states = Vec::new();
    let error = session
        .refresh(
            &mut |name, limit| repo.fetch(name, limit),
            now(),
            &AtomicBool::new(false),
            |snapshot| {
                states.push(snapshot.clone());
                Ok(())
            },
        )
        .expect_err("snapshot rollback");
    assert!(error.to_string().contains("snapshot rollback"));
    let saved = states
        .pop()
        .expect("authenticated new root/timestamp persisted");
    let mut reopened = Session::load(Some(saved), &repo.bootstrap)?;
    assert!(
        refresh(&mut reopened, &repo)
            .expect_err("reopened snapshot rollback")
            .to_string()
            .contains("snapshot rollback")
    );
    assert!(reopened.current(now()).is_err());
    // An authorized snapshot that meets the retained floor completes recovery.
    let snapshot = repo.files["2.snapshot.json"].clone();
    let timestamp = new_timestamp.sign(json!({"_type":"timestamp", "spec_version":"1.0.0", "version":2, "expires":FUTURE, "meta":{"snapshot.json":pin(&snapshot,2)}}));
    repo.files.insert("timestamp.json".into(), timestamp);
    refresh(&mut reopened, &repo)?;
    assert!(reopened.current(now()).is_ok());
    Ok(())
}
