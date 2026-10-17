use crate::{
    Error, Reference,
    registry::{self, BYTECODE_LIMIT, Registry, check, hash},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use bpfman_fs::{ImageCacheKey, RuntimeDirectory, RuntimeLayout};
use bpfman_model::ImagePullPolicy;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Component, sync::atomic::AtomicBool, time::Duration};

/// Validated image bytes and resolved manifest identity, without credentials.
#[derive(Debug)]
pub struct Image {
    bytes: Vec<u8>,
    digest: String,
}

impl Image {
    /// Exact captured BPF ELF bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    /// Selected single-platform manifest digest.
    pub fn digest(&self) -> &str {
        &self.digest
    }
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u32,
    reference: String,
    platform: String,
    digest: String,
    bytecode_digest: String,
    bytecode: String,
    // Admission evidence must never imply signature verification.
    policy: String,
}

const POLICY: &str = "verification-disabled-v1";

fn platform() -> String {
    format!("linux/{}", registry::host_arch())
}

fn key(reference: &Reference) -> ImageCacheKey {
    ImageCacheKey::new(Sha256::digest(format!("{}\n{}", reference.normalized(), platform())).into())
}

fn cached<E: std::error::Error + Send + Sync + 'static>(
    cache: &RuntimeDirectory,
    reference: &Reference,
    validate: &impl Fn(&[u8]) -> Result<(), E>,
) -> Result<Option<Image>, Error> {
    let Some(bytes) = cache
        .read_image_cache(key(reference))
        .map_err(|e| Error::caused("read image cache", e))?
    else {
        return Ok(None);
    };
    let entry: Envelope = serde_json::from_slice(&bytes).map_err(|e| {
        Error::caused(
            "decode image cache; use Always to refresh corrupt content",
            e,
        )
    })?;
    if entry.version != 1
        || entry.reference != reference.normalized()
        || entry.platform != platform()
        || entry.policy != POLICY
    {
        return Err(Error::invalid(
            "image cache admission metadata mismatch; use Always to refresh",
        ));
    }
    crate::reference::validate_digest(&entry.digest)?;
    let bytes = STANDARD
        .decode(entry.bytecode)
        .map_err(|e| Error::caused("decode cached bytecode", e))?;
    registry::verify(&bytes, &entry.bytecode_digest)?;
    if reference.selector.starts_with("sha256:") {
        // Index references resolve to a platform child. The complete normalized
        // request in the envelope binds that resolution to its original digest.
        crate::reference::validate_digest(&reference.selector)?;
    }
    validate_elf(&bytes)?;
    validate(&bytes).map_err(|e| Error::caused("validate cached bytecode", e))?;
    Ok(Some(Image {
        bytes,
        digest: entry.digest,
    }))
}

/// Resolve public bytecode with explicit cache policy and caller cancellation.
/// Registry and ELF validation finish before cache creation. Cache publication
/// has its own writer authority, separate from the program lifecycle lock.
/// Corrupt cache entries fail closed for cached policies; Always refreshes them.
/// The caller's side-effect-free validator performs full ELF/selection admission
/// before publication and on cache hits. It may run again when a concurrent
/// IfNotPresent writer wins publication.
pub fn pull<E: std::error::Error + Send + Sync + 'static>(
    reference: &Reference,
    policy: ImagePullPolicy,
    cache_layout: &RuntimeLayout,
    lock_timeout: Duration,
    cancelled: &AtomicBool,
    validate: impl Fn(&[u8]) -> Result<(), E>,
) -> Result<Image, Error> {
    check(cancelled)?;
    if policy != ImagePullPolicy::Always {
        if let Some(cache) = RuntimeDirectory::open_existing(cache_layout.clone())
            .map_err(|e| Error::caused("open image cache", e))?
        {
            if let Some(image) = cached(&cache, reference, &validate)? {
                check(cancelled)?;
                return Ok(image);
            }
        }
        if policy == ImagePullPolicy::Never {
            return Err(Error::invalid(
                "image not in cache and pull policy is Never",
            ));
        }
    }
    let mut registry = Registry::new(reference, cancelled);
    let mut manifest = registry.manifest(&reference.selector)?;
    if registry::is_index(&manifest) {
        manifest = registry.child(&registry::select(&manifest)?)?;
    }
    registry::require_image(&manifest)?;
    let _labels = registry.labels(&manifest)?;
    let layer = manifest.value["layers"]
        .as_array()
        .and_then(|v| v.first())
        .ok_or_else(|| Error::invalid("OCI image has no bytecode layer"))?;
    let layer = registry::descriptor(layer)?;
    if !matches!(
        layer.media_type.as_str(),
        "application/vnd.oci.image.layer.v1.tar+gzip"
            | "application/vnd.docker.image.rootfs.diff.tar.gzip"
            | "application/vnd.oci.image.layer.v1.tar"
            | "application/vnd.oci.image.layer.v1+gzip"
            | "application/octet-stream"
    ) {
        return Err(Error::invalid("unsupported bytecode layer media type"));
    }
    let layer = registry.blob(&layer, 32 * 1024 * 1024)?;
    let bytes = extract(&layer, cancelled)?;
    validate_elf(&bytes)?;
    validate(&bytes).map_err(|e| Error::caused("validate pulled bytecode", e))?;
    check(cancelled)?;
    let envelope = Envelope {
        version: 1,
        reference: reference.normalized().into(),
        platform: platform(),
        digest: manifest.digest.clone(),
        bytecode_digest: hash(&bytes),
        bytecode: STANDARD.encode(&bytes),
        policy: POLICY.into(),
    };
    let envelope =
        serde_json::to_vec(&envelope).map_err(|e| Error::caused("encode image cache", e))?;
    let cache = RuntimeDirectory::open_or_create(cache_layout.clone())
        .map_err(|e| Error::caused("prepare image cache", e))?;
    cache
        .with_cache_writer(
            bpfman_lock::AcquireOptions {
                timeout: lock_timeout,
                cancelled: Some(cancelled),
            },
            |writer| {
                check(cancelled)?;
                // A concurrent pull may have populated the cache. Preserve that entry
                // for IfNotPresent after rechecking under the cache lock.
                if policy == ImagePullPolicy::IfNotPresent {
                    if let Some(image) = cached(writer.directory(), reference, &validate)? {
                        return Ok(image);
                    }
                }
                publish_cache(&writer, key(reference), &envelope, || {
                    cancelled.load(std::sync::atomic::Ordering::Relaxed)
                })?;
                Ok(Image {
                    bytes,
                    digest: manifest.digest,
                })
            },
        )
        .map_err(|e| {
            if e.kind() == bpfman_fs::ErrorKind::Cancelled {
                Error::cancelled()
            } else {
                Error::caused("acquire image cache writer", e)
            }
        })?
}

fn publish_cache(
    writer: &bpfman_fs::RuntimeWriter<'_>,
    key: ImageCacheKey,
    envelope: &[u8],
    mut cancelled: impl FnMut() -> bool,
) -> Result<(), Error> {
    if cancelled() {
        return Err(Error::cancelled());
    }
    let stage = writer
        .stage_image_cache(key, envelope)
        .map_err(|e| Error::caused("stage image cache", e))?;
    if cancelled() {
        let error = Error::cancelled();
        return Err(match writer.remove_image_cache_stage(stage) {
            Ok(()) => error,
            Err(failure) => error.cleanup_failed(failure.cause, failure.remaining),
        });
    }
    writer
        .publish_image_cache(stage)
        .map_err(|f| Error::publication(writer, f.cause, f.remaining))
}

fn bounded(mut input: impl Read, limit: usize, flag: &AtomicBool) -> Result<Vec<u8>, Error> {
    let mut result = Vec::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        check(flag)?;
        let n = input
            .read(&mut buffer)
            .map_err(|e| Error::caused("decode bytecode layer", e))?;
        if n == 0 {
            return Ok(result);
        }
        if result.len() + n > limit {
            return Err(Error::invalid("bytecode layer exceeds decompression limit"));
        }
        result.extend_from_slice(&buffer[..n]);
    }
}

fn extract(layer: &[u8], flag: &AtomicBool) -> Result<Vec<u8>, Error> {
    let decompressed;
    let layer = if layer.starts_with(&[0x1f, 0x8b]) {
        decompressed = bounded(flate2::read::GzDecoder::new(layer), 64 * 1024 * 1024, flag)?;
        &decompressed
    } else {
        layer
    };
    if layer.starts_with(b"\x7fELF") {
        if layer.len() > BYTECODE_LIMIT {
            return Err(Error::invalid("bytecode exceeds size limit"));
        }
        return Ok(layer.to_vec());
    }
    let mut archive = tar::Archive::new(layer);
    for entry in archive
        .entries()
        .map_err(|e| Error::caused("read bytecode archive", e))?
    {
        check(flag)?;
        let entry = entry.map_err(|e| Error::caused("read bytecode archive entry", e))?;
        let path = entry
            .path()
            .map_err(|e| Error::caused("read archive path", e))?;
        if path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(Error::invalid("unsafe bytecode archive path"));
        }
        if path.extension().is_some_and(|v| v == "o") {
            if !entry.header().entry_type().is_file() {
                return Err(Error::invalid(
                    "bytecode archive entry must be a regular file",
                ));
            }
            let bytes = bounded(entry, BYTECODE_LIMIT, flag)?;
            validate_elf(&bytes)?;
            return Ok(bytes);
        }
    }
    Err(Error::invalid("no BPF bytecode (.o file) found in image"))
}

fn validate_elf(bytes: &[u8]) -> Result<(), Error> {
    // Full ELF/section interpretation remains at the kernel adapter boundary.
    // This admission check rejects wrong architecture/type/endian before caching.
    let endian = if cfg!(target_endian = "little") { 1 } else { 2 };
    if bytes.len() < 64
        || bytes.len() > BYTECODE_LIMIT
        || !bytes.starts_with(b"\x7fELF")
        || bytes[4] != 2
        || bytes[5] != endian
        || bytes[6] != 1
    {
        return Err(Error::invalid("invalid BPF ELF header or endianness"));
    }
    let word = |offset| {
        if endian == 1 {
            u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
        } else {
            u16::from_be_bytes([bytes[offset], bytes[offset + 1]])
        }
    };
    if word(16) != 1 || word(18) != 247 {
        return Err(Error::invalid(
            "bytecode must be a relocatable eBPF ELF object",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, fs, io::Write};

    fn elf() -> Vec<u8> {
        let mut bytes = vec![0; 64];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[5] = if cfg!(target_endian = "little") { 1 } else { 2 };
        let word = |v: u16| {
            if cfg!(target_endian = "little") {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        bytes[16..18].copy_from_slice(&word(1));
        bytes[18..20].copy_from_slice(&word(247));
        bytes
    }

    #[test]
    fn extracts_raw_gzipped_and_tar_bytecode_without_unpacking_paths() {
        let flag = AtomicBool::new(false);
        let bytes = elf();
        assert_eq!(extract(&bytes, &flag).expect("raw"), bytes);
        let mut archive = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        archive
            .append_data(&mut header, "nested/program.o", bytes.as_slice())
            .expect("tar entry");
        let tar = archive.into_inner().expect("tar");
        assert_eq!(extract(&tar, &flag).expect("tar"), bytes);
        for input in [bytes.as_slice(), tar.as_slice()] {
            let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            gzip.write_all(input).expect("gzip input");
            let compressed = gzip.finish().expect("gzip");
            assert_eq!(extract(&compressed, &flag).expect("gzip extraction"), bytes);
            assert!(extract(&compressed[..compressed.len() - 5], &flag).is_err());
        }
        let mut unsafe_tar = tar.clone();
        unsafe_tar[..100].fill(0);
        unsafe_tar[..10].copy_from_slice(b"../evil.o\0");
        // Recalculate the header checksum after deliberately bypassing tar's
        // safe-path constructor. Extraction must refuse it without filesystem I/O.
        unsafe_tar[148..156].fill(b' ');
        let sum: u32 = unsafe_tar[..512].iter().map(|b| u32::from(*b)).sum();
        unsafe_tar[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
        assert!(extract(&unsafe_tar, &flag).is_err());
        let mut link = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o644);
        header.set_link_name("outside").expect("link target");
        header.set_cksum();
        link.append_data(&mut header, "program.o", std::io::empty())
            .expect("link tar");
        assert!(extract(&link.into_inner().expect("tar"), &flag).is_err());
        assert!(validate_elf(b"ELF").is_err());
        let mut wrong = bytes.clone();
        wrong[18] = 0;
        assert!(validate_elf(&wrong).is_err());
    }

    #[test]
    fn staged_cancellation_cleans_once_and_retains_failed_cleanup_for_retry() {
        for replace_parent in [false, true] {
            let temp = tempfile::tempdir().expect("temporary");
            let layout = RuntimeLayout::try_from(temp.path().join("cache")).expect("layout");
            let cache = RuntimeDirectory::open_or_create(layout.clone()).expect("cache");
            cache
                .with_cache_writer(
                    bpfman_lock::AcquireOptions {
                        timeout: Duration::ZERO,
                        cancelled: None,
                    },
                    |writer| {
                        let calls = Cell::new(0);
                        let collection = layout.root().join("images-v1");
                        let saved = layout.root().join("saved");
                        let mut error = publish_cache(
                            &writer,
                            ImageCacheKey::new([6; 32]),
                            b"envelope",
                            || {
                                calls.set(calls.get() + 1);
                                if calls.get() == 2 {
                                    if replace_parent {
                                        fs::rename(&collection, &saved).expect("move collection");
                                        fs::create_dir(&collection).expect("replacement");
                                    }
                                    true
                                } else {
                                    false
                                }
                            },
                        )
                        .expect_err("cancelled");
                        assert!(error.is_cancelled());
                        assert_eq!(calls.get(), 2);
                        assert_eq!(error.has_pending_cleanup(), replace_parent);
                        if replace_parent {
                            assert!(error.retry_cleanup(&writer).is_err());
                            assert!(error.has_pending_cleanup());
                            fs::rename(&collection, layout.root().join("foreign"))
                                .expect("save replacement");
                            fs::rename(&saved, &collection).expect("restore");
                            error
                                .retry_cleanup(&writer)
                                .expect("explicit cleanup retry");
                            assert!(!error.has_pending_cleanup());
                        }
                        assert_eq!(fs::read_dir(collection).expect("collection").count(), 0);
                    },
                )
                .expect("writer");
        }
    }
}
