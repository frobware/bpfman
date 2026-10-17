//! Real HTTP, confined cache files and cooperative writer locking.
#[path = "../../../tests/support/oci.rs"]
mod fixture;

use bpfman_image_oci::{Reference, inspect, pull as resolve};
use bpfman_model::ImagePullPolicy::{Always, IfNotPresent, Never};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn pull(
    reference: &Reference,
    policy: bpfman_model::ImagePullPolicy,
    layout: &bpfman_fs::RuntimeLayout,
    timeout: Duration,
    flag: &AtomicBool,
) -> std::result::Result<bpfman_image_oci::Image, bpfman_image_oci::Error> {
    resolve(reference, None, policy, layout, timeout, flag, |_| {
        Ok::<(), std::convert::Infallible>(())
    })
}

fn elf() -> Vec<u8> {
    let mut bytes = vec![0; 64];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[5] = if cfg!(target_endian = "little") { 1 } else { 2 };
    let encode = |v: u16| {
        if cfg!(target_endian = "little") {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    };
    bytes[16..18].copy_from_slice(&encode(1));
    bytes[18..20].copy_from_slice(&encode(247));
    bytes
}

#[test]
fn policies_reference_forms_index_and_atomic_cache_generations() -> Result {
    for index in [false, true] {
        let temporary = tempfile::tempdir()?;
        let layout = bpfman_fs::RuntimeLayout::try_from(temporary.path().join("cache"))?;
        let server = fixture::Registry::new(elf(), index);
        server.bearer.store(true, Ordering::Relaxed);
        let reference = Reference::parse(&server.reference)?;
        let flag = AtomicBool::new(false);
        assert!(pull(&reference, Never, &layout, Duration::ZERO, &flag).is_err());
        assert_eq!(server.requests.load(Ordering::Relaxed), 0);
        assert!(!layout.root().exists());
        let image = pull(&reference, IfNotPresent, &layout, Duration::ZERO, &flag)?;
        assert_eq!(image.digest(), server.digest);
        assert_eq!(image.bytes(), elf());
        let cache = bpfman_fs::RuntimeDirectory::open_existing(layout.clone())?.expect("cache");
        cache.with_cache_writer(
            bpfman_lock::AcquireOptions {
                timeout: Duration::ZERO,
                cancelled: None,
            },
            |_| {
                assert!(
                    pull(&reference, Never, &layout, Duration::ZERO, &flag).is_ok(),
                    "cached readers bypass the writer lock"
                );
            },
        )?;
        let count = server.requests.load(Ordering::Relaxed);
        for spelling in [
            server.reference.clone(),
            format!("{}:latest", server.reference),
        ] {
            let reference = Reference::parse(&spelling)?;
            for policy in [Never, IfNotPresent] {
                assert_eq!(
                    pull(&reference, policy, &layout, Duration::ZERO, &flag)?.bytes(),
                    elf()
                );
            }
        }
        assert_eq!(server.requests.load(Ordering::Relaxed), count);
        let inspected = inspect(&reference, None, &flag)?;
        let pinned = Reference::parse(&format!(
            "{}@{}",
            server.reference,
            inspected["digest"].as_str().expect("digest")
        ))?;
        assert_eq!(
            pull(&pinned, Always, &layout, Duration::ZERO, &flag)?.digest(),
            server.digest
        );
        assert_eq!(inspected["reference"], reference.normalized());
        let manifest = if index {
            assert_eq!(inspected["manifests"][0]["digest"], server.digest);
            &inspected["manifests"][0]
        } else {
            &inspected
        };
        assert_eq!(manifest["programs"]["pass"], "xdp");
        assert_eq!(
            manifest["layers"][0]["media_type"],
            "application/octet-stream"
        );
        assert!(manifest["layers"][0].get("mediaType").is_none());

        // Failed refresh preserves a complete previous envelope.
        let previous = server
            .responses
            .lock()
            .expect("routes")
            .insert("/v2/test/manifests/latest".into(), (503, Vec::new()))
            .expect("previous manifest");
        assert!(pull(&reference, Always, &layout, Duration::ZERO, &flag).is_err());
        assert_eq!(
            pull(&reference, Never, &layout, Duration::ZERO, &flag)?.digest(),
            server.digest
        );
        assert!(
            std::fs::read_dir(layout.root().join("images-v1"))?.all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with('.'))
        );
        // Tampering must fail before attempting a network fallback.
        for entry in std::fs::read_dir(layout.root().join("images-v1"))? {
            std::fs::write(entry?.path(), b"{}").expect("corrupt envelope");
        }
        let count = server.requests.load(Ordering::Relaxed);
        for reference in [&pinned, &reference] {
            for policy in [Never, IfNotPresent] {
                assert!(pull(reference, policy, &layout, Duration::ZERO, &flag).is_err());
            }
        }
        assert_eq!(server.requests.load(Ordering::Relaxed), count);
        server
            .responses
            .lock()
            .expect("routes")
            .insert("/v2/test/manifests/latest".into(), previous);
        assert_eq!(
            pull(&reference, Always, &layout, Duration::ZERO, &flag)?.bytes(),
            elf()
        );
        assert_eq!(
            pull(&reference, Never, &layout, Duration::ZERO, &flag)?.bytes(),
            elf()
        );
    }
    Ok(())
}

#[test]
fn bad_content_and_cancelled_admission_create_no_cache() -> Result {
    let temporary = tempfile::tempdir()?;
    let layout = bpfman_fs::RuntimeLayout::try_from(temporary.path().join("cache"))?;
    let server = fixture::Registry::new(elf(), false);
    let reference = Reference::parse(&server.reference)?;
    let flag = AtomicBool::new(true);
    assert!(
        pull(&reference, Always, &layout, Duration::ZERO, &flag)
            .expect_err("cancelled")
            .is_cancelled()
    );
    assert_eq!(server.requests.load(Ordering::Relaxed), 0);
    flag.store(false, Ordering::Release);
    let layer_path = format!("/v2/test/blobs/{}", fixture::hash(&elf()));
    server
        .responses
        .lock()
        .expect("routes")
        .insert(layer_path, (200, vec![1; 64]));
    assert!(pull(&reference, Always, &layout, Duration::ZERO, &flag).is_err());
    assert!(!layout.root().exists());
    Ok(())
}

#[test]
fn full_validator_rejection_precedes_cache_publication() -> Result {
    let temp = tempfile::tempdir()?;
    let layout = bpfman_fs::RuntimeLayout::try_from(temp.path().join("cache"))?;
    let server = fixture::Registry::new(elf(), false);
    let reference = Reference::parse(&server.reference)?;
    let flag = AtomicBool::new(false);
    assert!(
        resolve(
            &reference,
            None,
            Always,
            &layout,
            Duration::ZERO,
            &flag,
            |_| Err::<(), _>(std::io::Error::other("ELF rejected"))
        )
        .is_err()
    );
    assert!(!layout.root().exists());
    pull(&reference, Always, &layout, Duration::ZERO, &flag)?;
    let requests = server.requests.load(Ordering::Relaxed);
    assert!(
        resolve(
            &reference,
            None,
            Never,
            &layout,
            Duration::ZERO,
            &flag,
            |_| Err::<(), _>(std::io::Error::other("selection rejected"))
        )
        .is_err()
    );
    assert_eq!(server.requests.load(Ordering::Relaxed), requests);
    assert!(pull(&reference, Never, &layout, Duration::ZERO, &flag).is_ok());
    Ok(())
}
