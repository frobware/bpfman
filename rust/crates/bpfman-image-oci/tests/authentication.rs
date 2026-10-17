//! Authenticated distribution traffic, credential confinement and retained cleanup.
#![allow(clippy::expect_used)]
#[path = "../../../tests/support/publish_registry.rs"]
mod fixture;
use base64::{Engine, engine::general_purpose::STANDARD};
use bpfman_image_oci::{Bytecode, Credentials, Publication, Reference, inspect, publish, pull};
use bpfman_model::ImagePullPolicy::{Always, IfNotPresent, Never};
use std::{collections::BTreeMap, sync::atomic::Ordering, time::Duration};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;
const USER: &str = "private-user";
const PASSWORD: &str = "private-password:with:colons";

fn credentials() -> Credentials {
    Credentials::new(USER, PASSWORD).expect("credentials")
}

fn configure(server: &fixture::Registry, bearer: bool) {
    let mut state = server.state.lock().expect("state");
    state.bearer = bearer;
    state.required_auth = Some(format!(
        "Basic {}",
        STANDARD.encode(format!("{USER}:{PASSWORD}"))
    ));
}

fn publication() -> Publication {
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
    Publication::Image(Bytecode::new(std::env::consts::ARCH, "program.o", bytes).expect("input"))
}

#[test]
fn basic_and_bearer_publication_inspection_and_cache_policies() -> Result {
    for bearer in [false, true] {
        let server = fixture::Registry::new();
        configure(&server, bearer);
        let reference = Reference::parse(&server.reference)?;
        let credentials = credentials();
        let wrong = Credentials::new(USER, "wrong-password")?;
        let temp = tempfile::tempdir()?;
        let cache = bpfman_fs::RuntimeLayout::try_from(temp.path().join("cache"))?;
        for auth in [None, Some(&wrong)] {
            assert!(
                publish(
                    &reference,
                    auth,
                    publication(),
                    &BTreeMap::new(),
                    0,
                    &server.cancelled
                )
                .is_err()
            );
            assert!(server.state.lock().expect("state").uploads.is_empty());
            assert!(server.state.lock().expect("state").manifests.is_empty());
        }
        publish(
            &reference,
            Some(&credentials),
            publication(),
            &BTreeMap::new(),
            0,
            &server.cancelled,
        )?;
        inspect(&reference, Some(&credentials), &server.cancelled)?;
        for auth in [None, Some(&wrong)] {
            assert!(inspect(&reference, auth, &server.cancelled).is_err());
            assert!(
                pull(
                    &reference,
                    auth,
                    Always,
                    &cache,
                    Duration::ZERO,
                    &server.cancelled,
                    |_| Ok::<_, std::convert::Infallible>(())
                )
                .is_err()
            );
            assert!(!cache.root().exists());
        }
        let image = pull(
            &reference,
            Some(&credentials),
            Always,
            &cache,
            Duration::ZERO,
            &server.cancelled,
            |_| Ok::<_, std::convert::Infallible>(()),
        )?;
        for auth in [None, Some(&wrong), Some(&credentials)] {
            for policy in [Never, IfNotPresent] {
                let count = server.state.lock().expect("state").requests.len();
                let cached = pull(
                    &reference,
                    auth,
                    policy,
                    &cache,
                    Duration::ZERO,
                    &server.cancelled,
                    |_| Ok::<_, std::convert::Infallible>(()),
                )?;
                assert_eq!(cached.bytes(), image.bytes());
                assert_eq!(server.state.lock().expect("state").requests.len(), count);
            }
        }
        assert!(
            pull(
                &reference,
                Some(&wrong),
                Always,
                &cache,
                Duration::ZERO,
                &server.cancelled,
                |_| Ok::<_, std::convert::Infallible>(())
            )
            .is_err()
        );
        assert_eq!(
            pull(
                &reference,
                None,
                Never,
                &cache,
                Duration::ZERO,
                &server.cancelled,
                |_| Ok::<_, std::convert::Infallible>(())
            )?
            .digest(),
            image.digest()
        );
        for entry in std::fs::read_dir(cache.root().join("images-v1"))? {
            let bytes = std::fs::read(entry?.path())?;
            let text = String::from_utf8_lossy(&bytes);
            for secret in [
                USER,
                PASSWORD,
                "publish-token",
                &STANDARD.encode(format!("{USER}:{PASSWORD}")),
            ] {
                assert!(!text.contains(secret), "cache leaked credentials");
            }
        }
        let state = server.state.lock().expect("state");
        if bearer {
            assert!(state.requests.iter().any(|r| r.starts_with("GET /token?")
                && (r.contains("pull%2Cpush") || r.contains("pull,push"))));
            assert!(
                state
                    .authorizations
                    .iter()
                    .any(|(key, value)| key == "GET /token" && value == &state.required_auth)
            );
            assert!(
                state
                    .authorizations
                    .iter()
                    .filter(|(key, _)| key != "GET /token")
                    .all(|(_, value)| value.as_deref().is_none_or(|v| !v.starts_with("Basic ")))
            );
        }
    }
    Ok(())
}

#[test]
fn authenticated_abort_and_explicit_retry_retain_redacted_credentials() -> Result {
    for bearer in [false, true] {
        let server = fixture::Registry::new();
        configure(&server, bearer);
        let reference = Reference::parse(&server.reference)?;
        {
            let mut state = server.state.lock().expect("state");
            state.cancel_on = Some("POST /v2/test/blobs/uploads/".into());
            state
                .faults
                .insert("DELETE /v2/test/blobs/uploads/*".into(), 503);
        }
        let mut error = publish(
            &reference,
            Some(&credentials()),
            publication(),
            &BTreeMap::new(),
            0,
            &server.cancelled,
        )
        .expect_err("cancelled");
        assert!(error.is_cancelled());
        assert!(error.has_pending_cleanup());
        let diagnostic = format!("{error:?} {error}");
        for secret in [USER, PASSWORD, "publish-token"] {
            assert!(!diagnostic.contains(secret));
        }
        assert!(error.retry_upload_cleanup().is_err());
        assert!(error.has_pending_cleanup());
        server.state.lock().expect("state").faults.clear();
        error.retry_upload_cleanup()?;
        assert!(!error.has_pending_cleanup());
        assert!(error.is_cancelled());
        let state = server.state.lock().expect("state");
        assert!(state.uploads.is_empty());
        assert!(state.manifests.is_empty());
        assert_eq!(
            state
                .requests
                .iter()
                .filter(|r| r.starts_with("DELETE "))
                .count(),
            6,
            "each of three cleanup passes challenges once then authenticates"
        );
    }
    Ok(())
}

#[test]
fn token_failure_and_cancellation_precede_registry_mutation() -> Result {
    for cancel in [false, true] {
        let server = fixture::Registry::new();
        configure(&server, true);
        {
            let mut state = server.state.lock().expect("state");
            if cancel {
                state.cancel_on = Some("GET /token".into());
            } else {
                state.faults.insert("GET /token".into(), 503);
            }
        }
        let error = publish(
            &Reference::parse(&server.reference)?,
            Some(&credentials()),
            publication(),
            &BTreeMap::new(),
            0,
            &server.cancelled,
        )
        .expect_err("token failure");
        assert_eq!(error.is_cancelled(), cancel);
        assert!(!error.has_pending_cleanup());
        let state = server.state.lock().expect("state");
        assert!(state.uploads.is_empty());
        assert!(state.manifests.is_empty());
        assert!(
            !state
                .requests
                .iter()
                .any(|r| r.starts_with("POST ") || r.starts_with("PUT "))
        );
    }
    Ok(())
}

#[test]
fn redirects_strip_registry_authorization_and_never_forward_token_credentials() -> Result {
    let receiver = fixture::Registry::new();
    let destination = format!(
        "http://{}/sink",
        receiver.reference.split('/').next().expect("host")
    );
    for bearer in [false, true] {
        let server = fixture::Registry::new();
        configure(&server, bearer);
        server
            .state
            .lock()
            .expect("state")
            .redirects
            .insert("GET /v2/test/manifests/latest".into(), destination.clone());
        assert!(
            inspect(
                &Reference::parse(&server.reference)?,
                Some(&credentials()),
                &server.cancelled
            )
            .is_err()
        );
        let state = receiver.state.lock().expect("state");
        assert_eq!(
            state.authorizations.last(),
            Some(&("GET /sink".into(), None))
        );
    }
    // A redirected 401 must not be allowed to choose a credential-bearing
    // token endpoint. This also refuses a foreign Basic challenge before retry.
    {
        configure(&receiver, false);
        let server = fixture::Registry::new();
        server
            .state
            .lock()
            .expect("state")
            .redirects
            .insert("GET /v2/test/manifests/latest".into(), destination.clone());
        let error = inspect(
            &Reference::parse(&server.reference)?,
            Some(&credentials()),
            &server.cancelled,
        )
        .expect_err("foreign challenge");
        assert!(error.to_string().contains("redirected registry response"));
        assert_eq!(server.state.lock().expect("state").requests.len(), 1);
        assert!(
            receiver
                .state
                .lock()
                .expect("state")
                .authorizations
                .iter()
                .all(|(_, auth)| auth.is_none())
        );
    }
    let count = receiver.state.lock().expect("state").requests.len();
    for (key, bearer) in [
        ("GET /token", true),
        ("POST /v2/test/blobs/uploads/", false),
    ] {
        let server = fixture::Registry::new();
        configure(&server, bearer);
        // Token redirects cannot even be followed without auth. Publishing
        // also refuses redirects when acquiring an upload session.
        server
            .state
            .lock()
            .expect("state")
            .redirects
            .insert(key.into(), destination.clone());
        assert!(
            publish(
                &Reference::parse(&server.reference)?,
                Some(&credentials()),
                publication(),
                &BTreeMap::new(),
                0,
                &server.cancelled
            )
            .is_err()
        );
        assert_eq!(receiver.state.lock().expect("state").requests.len(), count);
    }
    Ok(())
}

#[test]
fn insecure_foreign_or_userinfo_token_realms_fail_before_credentials_leave_registry() -> Result {
    let receiver = fixture::Registry::new();
    let address = receiver.reference.split('/').next().expect("host");
    for realm in [
        format!("http://{address}/token"),
        format!("https://{USER}:{PASSWORD}@example.invalid/token"),
        "file:///token".into(),
        "https://example.invalid/token#fragment".into(),
    ] {
        let server = fixture::Registry::new();
        configure(&server, true);
        server.state.lock().expect("state").realm = Some(realm);
        let error = inspect(
            &Reference::parse(&server.reference)?,
            Some(&credentials()),
            &server.cancelled,
        )
        .expect_err("realm refusal");
        for secret in [USER, PASSWORD] {
            assert!(!format!("{error:?} {error}").contains(secret));
        }
        assert_eq!(server.state.lock().expect("state").requests.len(), 1);
        assert!(receiver.state.lock().expect("state").requests.is_empty());
    }
    assert!(!receiver.cancelled.load(Ordering::Relaxed));
    Ok(())
}
