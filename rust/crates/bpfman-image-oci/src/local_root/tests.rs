#![allow(clippy::expect_used)]
use super::*;
use crate::{SigningPolicy, VerificationStatus, signed_image};
use bpfman_model::ImagePullPolicy::{Always, IfNotPresent, Never};
use sigstore_verify::trust_root::{
    SIGSTORE_PRODUCTION_TRUSTED_ROOT as PRODUCTION, SIGSTORE_STAGING_TRUSTED_ROOT as STAGING,
};
use std::{io, sync::atomic::Ordering, time::Duration};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn local_selection_is_deferred_and_last_selector_wins_without_fallback() -> Result {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("root.json");
    let cache = bpfman_fs::RuntimeLayout::try_from(temp.path().join("trust"))?;
    // Any TUF access would fail rather than using production network services.
    std::fs::write(cache.root(), "foreign")?;
    let server = signed_image::registry();
    let reference = crate::Reference::parse(&server.reference)?;
    let flag = AtomicBool::new(false);
    let policy = SigningPolicy::verify(false, vec![])?
        .with_trust_cache(cache.clone(), Duration::ZERO)
        .with_trusted_root_file(root.clone());
    assert!(!root.exists(), "constructors do no I/O");
    let verify = |policy: &SigningPolicy| crate::verify(&reference, None, policy, &flag);
    assert!(verify(&policy).is_err());
    assert_eq!(server.requests.load(Ordering::Relaxed), 0);

    std::fs::write(&root, PRODUCTION)?;
    assert_eq!(verify(&policy)?, VerificationStatus::Verified);
    let before = server.requests.load(Ordering::Relaxed);
    assert!(
        verify(
            &policy
                .clone()
                .with_trust_cache(cache.clone(), Duration::ZERO)
        )
        .is_err()
    );
    assert_eq!(server.requests.load(Ordering::Relaxed), before);
    std::fs::write(&root, "invalid operator root")?;
    assert!(verify(&policy).is_err());
    assert_eq!(server.requests.load(Ordering::Relaxed), before);
    assert_eq!(std::fs::read_to_string(&root)?, "invalid operator root");
    assert_eq!(std::fs::read_to_string(cache.root())?, "foreign");

    let disabled = SigningPolicy::disabled().with_trusted_root_file(root);
    assert_eq!(verify(&disabled)?, VerificationStatus::Disabled);
    flag.store(true, Ordering::Relaxed);
    assert!(verify(&policy).expect_err("cancelled").is_cancelled());
    Ok(())
}

#[test]
fn invalid_roots_fail_before_registry_io_and_cannot_expose_input() -> Result {
    use std::error::Error as _;
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("root.json");
    let server = signed_image::registry();
    let reference = crate::Reference::parse(&server.reference)?;
    let policy = SigningPolicy::verify(true, vec![])?.with_trusted_root_file(path.clone());
    let flag = AtomicBool::new(false);
    let mut bad_key: serde_json::Value = serde_json::from_str(PRODUCTION)?;
    bad_key["tlogs"][0]["publicKey"]["rawBytes"] = "operator-input-do-not-echo".into();
    let mut bad_media: serde_json::Value = serde_json::from_str(PRODUCTION)?;
    bad_media["mediaType"] = "operator-input-do-not-echo".into();
    let mut invalid_cert: serde_json::Value = serde_json::from_str(PRODUCTION)?;
    invalid_cert["certificateAuthorities"][0]["certChain"]["certificates"][0]["rawBytes"] =
        "AA==".into();
    let mut invalid_window: serde_json::Value = serde_json::from_str(PRODUCTION)?;
    invalid_window["certificateAuthorities"][0]["validFor"] = serde_json::json!({
        "start": "2099-01-01T00:00:00Z", "end": "2020-01-01T00:00:00Z"
    });
    for bytes in [
        vec![],
        b"not JSON operator-input-do-not-echo".to_vec(),
        b"{}".to_vec(),
        serde_json::to_vec(&serde_json::json!({"mediaType": MEDIA_TYPE}))?,
        vec![0xff],
        serde_json::to_vec(&bad_key)?,
        serde_json::to_vec(&bad_media)?,
        serde_json::to_vec(&invalid_cert)?,
        serde_json::to_vec(&invalid_window)?,
        vec![b' '; LIMIT + 1],
    ] {
        std::fs::write(&path, &bytes)?;
        let error = crate::verify(&reference, None, &policy, &flag).expect_err("invalid root");
        assert!(error.to_string().contains("Sigstore trust root"), "{error}");
        assert!(!format!("{error:?}").contains("operator-input-do-not-echo"));
        assert!(error.source().is_none());
        assert!(!error.has_pending_cleanup());
        assert_eq!(std::fs::read(&path)?, bytes);
    }
    assert_eq!(server.requests.load(Ordering::Relaxed), 0);
    assert_eq!(std::fs::read_dir(temp.path())?.count(), 1);
    Ok(())
}

#[test]
fn root_reader_bounds_bytes_resumes_interruption_and_observes_cancellation() -> Result {
    struct Reader<'a> {
        bytes: &'a [u8],
        consumed: usize,
        interrupt: bool,
        cancel: Option<&'a AtomicBool>,
    }
    impl Read for Reader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if std::mem::take(&mut self.interrupt) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let count = self.bytes.read(buffer)?;
            self.consumed += count;
            if let Some(flag) = self.cancel {
                flag.store(true, Ordering::Relaxed);
            }
            Ok(count)
        }
    }
    let flag = AtomicBool::new(false);
    let mut reader = Reader {
        bytes: PRODUCTION.as_bytes(),
        consumed: 0,
        interrupt: true,
        cancel: None,
    };
    assert_eq!(read(&mut reader, &flag)?, PRODUCTION);
    assert_eq!(reader.consumed, PRODUCTION.len());
    let bytes = vec![b' '; LIMIT + 64 * 1024];
    let mut reader = Reader {
        bytes: &bytes,
        consumed: 0,
        interrupt: false,
        cancel: None,
    };
    assert!(read(&mut reader, &flag).is_err());
    assert_eq!(reader.consumed, LIMIT + 1);
    // Exactly the limit is accepted by the reader, then rejected as invalid JSON.
    assert_eq!(read(&bytes[..LIMIT], &flag)?.len(), LIMIT);
    let mut padded = PRODUCTION.to_owned();
    padded.extend(std::iter::repeat_n(' ', LIMIT - PRODUCTION.len()));
    assert_eq!(read(padded.as_bytes(), &flag)?.len(), LIMIT);
    validate(&padded, &flag)?;
    let mut reader = Reader {
        bytes: PRODUCTION.as_bytes(),
        consumed: 0,
        interrupt: false,
        cancel: Some(&flag),
    };
    assert!(
        read(&mut reader, &flag)
            .expect_err("cancel during read")
            .is_cancelled()
    );
    assert!(reader.consumed > 0);
    assert!(
        validate(PRODUCTION, &flag)
            .expect_err("cancel before parse")
            .is_cancelled()
    );
    Ok(())
}

#[test]
fn file_roots_bind_reopened_cache_and_failed_revalidation_preserves_admission() -> Result {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("root.json");
    std::fs::write(&path, PRODUCTION)?;
    let server = signed_image::registry();
    let reference = crate::Reference::parse(&server.reference)?;
    let layout = bpfman_fs::RuntimeLayout::try_from(temp.path().join("images"))?;
    let flag = AtomicBool::new(false);
    let policy = || {
        Ok::<_, crate::Error>(
            SigningPolicy::verify(false, vec![])?.with_trusted_root_file(path.clone()),
        )
    };
    let pull = |mode| {
        crate::pull(
            &reference,
            None,
            &policy()?,
            mode,
            &layout,
            Duration::ZERO,
            &flag,
            |_| Ok::<_, std::convert::Infallible>(()),
        )
    };
    let original = pull(Always)?;
    let before = server.requests.load(Ordering::Relaxed);
    assert_eq!(pull(Never)?.bytes(), original.bytes());
    assert_eq!(server.requests.load(Ordering::Relaxed), before);

    // Valid staging material does not trust this production signature. Failure
    // cannot fall back or rewrite the previously admitted cache generation.
    std::fs::write(&path, STAGING)?;
    assert!(pull(Never).is_err());
    assert_eq!(server.requests.load(Ordering::Relaxed), before);
    assert!(pull(IfNotPresent).is_err());
    std::fs::write(&path, PRODUCTION)?;
    let before = server.requests.load(Ordering::Relaxed);
    assert_eq!(pull(Never)?.bytes(), original.bytes());
    assert_eq!(server.requests.load(Ordering::Relaxed), before);

    // Even formatting changes bind different bytes. Reopening the same path
    // must see the new policy, and successful revalidation replaces admission.
    std::fs::write(&path, format!("{PRODUCTION}\n"))?;
    assert!(pull(Never).is_err());
    assert_eq!(server.requests.load(Ordering::Relaxed), before);
    assert_eq!(pull(IfNotPresent)?.bytes(), original.bytes());
    assert!(server.requests.load(Ordering::Relaxed) > before);
    let before = server.requests.load(Ordering::Relaxed);
    assert_eq!(pull(Never)?.bytes(), original.bytes());
    assert_eq!(server.requests.load(Ordering::Relaxed), before);
    std::fs::write(&path, PRODUCTION)?;
    assert!(pull(Never).is_err());
    assert_eq!(server.requests.load(Ordering::Relaxed), before);
    assert!(!temp.path().join("runtime").exists());
    Ok(())
}

#[test]
fn each_operation_captures_one_root_generation_and_path_is_not_policy_identity() -> Result {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("root.json");
    let alias = temp.path().join("alias.json");
    std::fs::write(&path, PRODUCTION)?;
    std::fs::write(&alias, PRODUCTION)?;
    let flag = AtomicBool::new(false);
    let original = SigningPolicy::verify(false, vec![])?.with_trusted_root_file(path.clone());
    let same = SigningPolicy::verify(false, vec![])?.with_trusted_root_file(alias);
    assert_eq!(
        original.resolve(false, &flag)?.id(),
        same.resolve(false, &flag)?.id()
    );
    let server = signed_image::registry();
    let reference = crate::Reference::parse(&server.reference)?;
    let layout = bpfman_fs::RuntimeLayout::try_from(temp.path().join("images"))?;
    let image = crate::pull(
        &reference,
        None,
        &original,
        Always,
        &layout,
        Duration::ZERO,
        &flag,
        |_| {
            // Mutation after trust resolution cannot change admission mid-operation.
            std::fs::write(&path, STAGING)
        },
    )?;
    let before = server.requests.load(Ordering::Relaxed);
    let pull = |policy| {
        crate::pull(
            &reference,
            None,
            policy,
            Never,
            &layout,
            Duration::ZERO,
            &flag,
            |_| Ok::<_, std::convert::Infallible>(()),
        )
    };
    assert!(pull(&original).is_err());
    assert_eq!(pull(&same)?.bytes(), image.bytes());
    assert_eq!(server.requests.load(Ordering::Relaxed), before);
    Ok(())
}
