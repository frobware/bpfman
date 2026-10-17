#![allow(clippy::expect_used)]
use super::test_registry as registry;

use super::*;
use crate::{Bytecode, Publication, Reference, SigningPolicy, inspect, publish, pull};
use std::{collections::BTreeMap, sync::atomic::Ordering, time::Duration};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn file_credentials(reference: &Reference, file: &std::path::Path) -> Credentials {
    Credentials {
        source: Source::Files {
            registry: reference.registry.clone(),
            paths: vec![file.to_owned()],
        },
    }
}

fn publication() -> Publication {
    let mut bytes = vec![0; 64];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    let word = |v: u16| {
        if cfg!(target_endian = "little") {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    };
    bytes[16..18].copy_from_slice(&word(1));
    bytes[18..20].copy_from_slice(&word(247));
    Publication::Image(
        Bytecode::new(std::env::consts::ARCH, "bytecode.o", bytes).expect("bytecode"),
    )
}

#[test]
fn resolved_file_credentials_survive_file_removal_and_explicit_upload_cleanup_retry() -> Result {
    for bearer in [false, true] {
        let temp = tempfile::tempdir()?;
        let server = registry::Registry::new();
        let reference = Reference::parse(&server.reference)?;
        let file = temp.path().join("auth.json");
        let secret = STANDARD.encode("file-user:retry-secret");
        std::fs::write(
            &file,
            serde_json::json!({"auths":{&reference.registry:{"auth":secret}}}).to_string(),
        )?;
        let mut state = server.state.lock().expect("state");
        state.bearer = bearer;
        state.required_auth = Some(format!("Basic {secret}"));
        state.cancel_on = Some("POST /v2/test/blobs/uploads/".into());
        state
            .faults
            .insert("DELETE /v2/test/blobs/uploads/*".into(), 503);
        drop(state);
        let credentials = file_credentials(&reference, &file);
        let mut error = publish(
            &reference,
            Some(&credentials),
            publication(),
            &BTreeMap::new(),
            0,
            &server.cancelled,
        )
        .expect_err("cancelled");
        assert!(error.is_cancelled());
        assert!(error.has_pending_cleanup());
        assert!(!format!("{error:?} {error}").contains("retry-secret"));
        // This path was created by this test in its owned temporary directory.
        #[allow(clippy::disallowed_methods)]
        std::fs::remove_file(&file)?;
        assert!(error.retry_upload_cleanup().is_err());
        assert!(error.has_pending_cleanup());
        server.state.lock().expect("state").faults.clear();
        error.retry_upload_cleanup()?;
        assert!(!error.has_pending_cleanup());
        let state = server.state.lock().expect("state");
        assert!(state.uploads.is_empty());
        assert!(state.manifests.is_empty());
        assert_eq!(
            state
                .requests
                .iter()
                .filter(|r| r.starts_with("DELETE "))
                .count(),
            6
        );
    }
    Ok(())
}

#[test]
fn registry_token_is_sent_only_to_registry_and_cache_hits_never_reopen_files() -> Result {
    let temp = tempfile::tempdir()?;
    let server = registry::Registry::new();
    server.state.lock().expect("state").bearer = true;
    let reference = Reference::parse(&server.reference)?;
    let file = temp.path().join("auth.json");
    std::fs::write(
        &file,
        serde_json::json!({"auths":{&reference.registry:{"registrytoken":"publish-token"}}})
            .to_string(),
    )?;
    let credentials = file_credentials(&reference, &file);
    publish(
        &reference,
        Some(&credentials),
        publication(),
        &BTreeMap::new(),
        0,
        &server.cancelled,
    )?;
    inspect(&reference, Some(&credentials), &server.cancelled)?;
    let cache = bpfman_fs::RuntimeLayout::try_from(temp.path().join("cache"))?;
    pull(
        &reference,
        Some(&credentials),
        &SigningPolicy::disabled(),
        bpfman_model::ImagePullPolicy::Always,
        &cache,
        Duration::ZERO,
        &server.cancelled,
        |_| Ok::<_, std::convert::Infallible>(()),
    )?;
    std::fs::write(&file, "invalid-file-secret")?;
    let count = server.state.lock().expect("state").requests.len();
    for policy in [
        bpfman_model::ImagePullPolicy::Never,
        bpfman_model::ImagePullPolicy::IfNotPresent,
    ] {
        pull(
            &reference,
            Some(&credentials),
            &SigningPolicy::disabled(),
            policy,
            &cache,
            Duration::ZERO,
            &server.cancelled,
            |_| Ok::<_, std::convert::Infallible>(()),
        )?;
    }
    assert_eq!(server.state.lock().expect("state").requests.len(), count);
    assert!(
        !server
            .state
            .lock()
            .expect("state")
            .requests
            .iter()
            .any(|r| r.starts_with("GET /token"))
    );
    for entry in std::fs::read_dir(cache.root().join("images-v1"))? {
        assert!(!std::fs::read_to_string(entry?.path())?.contains("publish-token"));
    }
    assert!(!server.cancelled.load(Ordering::Relaxed));
    Ok(())
}

#[test]
fn redirected_challenges_cannot_trigger_file_lookup_or_receive_credentials() -> Result {
    let temp = tempfile::tempdir()?;
    let receiver = registry::Registry::new();
    receiver.state.lock().expect("state").required_auth = Some("Basic ignored".into());
    let server = registry::Registry::new();
    let address = receiver.reference.split('/').next().ok_or("host")?;
    server.state.lock().expect("state").redirects.insert(
        "GET /v2/test/manifests/latest".into(),
        format!("http://{address}/sink"),
    );
    let reference = Reference::parse(&server.reference)?;
    let file = temp.path().join("auth.json");
    std::fs::write(&file, "invalid-file-secret")?;
    let error = inspect(
        &reference,
        Some(&file_credentials(&reference, &file)),
        &server.cancelled,
    )
    .expect_err("foreign challenge");
    assert!(error.to_string().contains("redirected registry response"));
    assert!(
        receiver
            .state
            .lock()
            .expect("state")
            .authorizations
            .iter()
            .all(|(_, value)| value.is_none())
    );
    Ok(())
}
