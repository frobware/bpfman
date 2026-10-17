#![allow(clippy::expect_used)]
use super::{Credentials, Source, test_registry as registry, tests::publication};
use crate::{Reference, SigningPolicy, inspect, publish, pull};
use bpfman_model::ImagePullPolicy::{Always, IfNotPresent, Never};
use serde_json::json;
use std::{collections::BTreeMap, time::Duration};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;
const SECRET: &str = "refresh-secret+/: &=?";
const FORM: &str = "grant_type=refresh_token&refresh_token=refresh-secret%2B%2F%3A+%26%3D%3F&service=fixture&client_id=bpfman&scope=repository%3Atest%3A";

fn configure(server: &registry::Registry) {
    let mut state = server.state.lock().expect("state");
    state.bearer = true;
    state.refresh_form = Some(format!("{FORM}pull%2Cpush"));
}

fn file_credentials(
    reference: &Reference,
    path: &std::path::Path,
) -> std::result::Result<Credentials, Box<dyn std::error::Error>> {
    std::fs::write(
        path,
        json!({"auths":{&reference.registry:{"identitytoken":SECRET}}}).to_string(),
    )?;
    Ok(Credentials {
        source: Source::Files {
            registry: reference.registry.clone(),
            paths: vec![path.to_owned()],
        },
    })
}

#[test]
fn identity_tokens_are_bounded_redacted_and_distinct_from_basic_and_access_tokens() -> Result {
    let credentials = Credentials::identity(SECRET.into(), None)?;
    assert_eq!(credentials.refresh(), Some(SECRET));
    assert!(credentials.basic().is_err());
    assert!(credentials.bearer().is_none());
    assert!(!format!("{credentials:?}").contains(SECRET));
    for token in [
        "".into(),
        "secret\n".into(),
        "secret\0".into(),
        "secret\u{7f}".into(),
        "秘密".into(),
        "x".repeat(64 * 1024 + 1),
    ] {
        let error = Credentials::identity(token, None).expect_err("invalid token");
        assert_eq!(error.to_string(), "invalid registry identity token");
        assert!(std::error::Error::source(&error).is_none());
    }
    assert!(Credentials::identity("x".repeat(64 * 1024), None).is_ok());
    Ok(())
}

#[test]
fn refresh_post_authenticates_publication_inspection_and_cache_without_persisting_secrets() -> Result
{
    let temp = tempfile::tempdir()?;
    let server = registry::Registry::new();
    configure(&server);
    let reference = Reference::parse(&server.reference)?;
    let file = temp.path().join("auth.json");
    let credentials = file_credentials(&reference, &file)?;
    publish(
        &reference,
        Some(&credentials),
        publication(),
        &BTreeMap::new(),
        0,
        &server.cancelled,
    )?;
    server.state.lock().expect("state").refresh_form = Some(format!("{FORM}pull"));
    inspect(&reference, Some(&credentials), &server.cancelled)?;
    let cache = bpfman_fs::RuntimeLayout::try_from(temp.path().join("cache"))?;
    pull(
        &reference,
        Some(&credentials),
        &SigningPolicy::disabled(),
        Always,
        &cache,
        Duration::ZERO,
        &server.cancelled,
        |_| Ok::<_, std::convert::Infallible>(()),
    )?;
    std::fs::write(&file, "broken-secret")?;
    let before = server.state.lock().expect("state").requests.len();
    for policy in [Never, IfNotPresent] {
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
    let state = server.state.lock().expect("state");
    assert_eq!(state.requests.len(), before);
    assert_eq!(state.token_forms.len(), 3);
    assert!(
        state
            .token_forms
            .iter()
            .all(|(url, content_type, _)| url == "/token"
                && content_type.as_deref() == Some("application/x-www-form-urlencoded"))
    );
    assert!(
        state
            .requests
            .iter()
            .all(|r| !r.starts_with("GET /token") && !r.contains("refresh-secret"))
    );
    assert!(state.authorizations.iter().all(|(_, auth)| {
        auth.as_deref()
            .is_none_or(|value| value == "Bearer publish-token")
    }));
    drop(state);
    for entry in std::fs::read_dir(cache.root().join("images-v1"))? {
        let bytes = std::fs::read(entry?.path())?;
        let text = String::from_utf8_lossy(&bytes);
        for secret in [SECRET, "publish-token", "refresh-secret%2B"] {
            assert!(!text.contains(secret), "cache leaked credentials");
        }
    }
    Ok(())
}

#[test]
fn refresh_failures_and_cancellation_never_fall_back_or_create_uploads() -> Result {
    for (status, body, cancel) in [
        (400, None, false),
        (401, None, false),
        (503, None, false),
        (200, Some(b"malformed-secret".to_vec()), false),
        (
            200,
            Some(br#"{"token":"distribution-only-secret"}"#.to_vec()),
            false,
        ),
        (200, Some(br#"{"access_token":""}"#.to_vec()), false),
        (200, Some(br#"{"access_token":42}"#.to_vec()), false),
        (200, Some(br#"{"access_token":"secret\n"}"#.to_vec()), false),
        (
            200,
            Some(
                json!({"access_token":"x".repeat(64 * 1024 + 1)})
                    .to_string()
                    .into_bytes(),
            ),
            false,
        ),
        (200, Some(vec![b'x'; 1024 * 1024 + 1]), false),
        (200, None, true),
    ] {
        let temp = tempfile::tempdir()?;
        let server = registry::Registry::new();
        configure(&server);
        let reference = Reference::parse(&server.reference)?;
        let file = temp.path().join("auth.json");
        let credentials = file_credentials(&reference, &file)?;
        let mut state = server.state.lock().expect("state");
        if status != 200 {
            state.faults.insert("POST /token".into(), status);
        }
        state.token_response = body;
        if cancel {
            state.cancel_on = Some("POST /token".into());
        }
        drop(state);
        let error = publish(
            &reference,
            Some(&credentials),
            publication(),
            &BTreeMap::new(),
            0,
            &server.cancelled,
        )
        .expect_err("failed exchange");
        assert_eq!(error.is_cancelled(), cancel);
        assert!(!error.has_pending_cleanup());
        assert!(!format!("{error:?} {error}").contains("secret"));
        assert!(std::error::Error::source(&error).is_none());
        let state = server.state.lock().expect("state");
        assert_eq!(state.token_forms.len(), 1);
        assert!(
            state
                .requests
                .iter()
                .all(|r| r.starts_with("HEAD ") || r == "POST /token")
        );
        assert!(state.uploads.is_empty());
        assert!(state.manifests.is_empty());
    }
    Ok(())
}

#[test]
fn refresh_token_redirects_and_unsafe_realms_never_forward_the_secret() -> Result {
    let receiver = registry::Registry::new();
    let address = receiver.reference.split('/').next().ok_or("host")?;
    for realm in [
        format!("http://{address}/token"),
        "https://user:realm-secret@example.invalid/token".into(),
        "file:///token".into(),
        "https://example.invalid/token#fragment-secret".into(),
    ] {
        let server = registry::Registry::new();
        configure(&server);
        server.state.lock().expect("state").realm = Some(realm);
        let error = inspect(
            &Reference::parse(&server.reference)?,
            Some(&Credentials::identity(SECRET.into(), None)?),
            &server.cancelled,
        )
        .expect_err("realm rejected");
        assert!(!format!("{error:?} {error}").contains("secret"));
        assert!(server.state.lock().expect("state").token_forms.is_empty());
    }
    let server = registry::Registry::new();
    configure(&server);
    server.state.lock().expect("state").refresh_form = Some(format!("{FORM}pull"));
    server
        .state
        .lock()
        .expect("state")
        .redirects
        .insert("POST /token".into(), format!("http://{address}/token"));
    assert!(
        inspect(
            &Reference::parse(&server.reference)?,
            Some(&Credentials::identity(SECRET.into(), None)?),
            &server.cancelled
        )
        .is_err()
    );
    assert_eq!(server.state.lock().expect("state").token_forms.len(), 1);
    assert!(receiver.state.lock().expect("state").requests.is_empty());
    // A foreign response may not choose the refresh-token recipient.
    let server = registry::Registry::new();
    receiver.state.lock().expect("state").bearer = true;
    server.state.lock().expect("state").redirects.insert(
        "GET /v2/test/manifests/latest".into(),
        format!("http://{address}/sink"),
    );
    let error = inspect(
        &Reference::parse(&server.reference)?,
        Some(&Credentials::identity(SECRET.into(), None)?),
        &server.cancelled,
    )
    .expect_err("foreign challenge");
    assert!(error.to_string().contains("redirected registry response"));
    assert!(receiver.state.lock().expect("state").token_forms.is_empty());
    assert!(
        receiver
            .state
            .lock()
            .expect("state")
            .authorizations
            .iter()
            .all(|(_, auth)| auth.is_none())
    );
    Ok(())
}

#[test]
fn refresh_credentials_survive_file_removal_failed_abort_and_failed_exchange_on_retry() -> Result {
    let temp = tempfile::tempdir()?;
    let server = registry::Registry::new();
    configure(&server);
    let reference = Reference::parse(&server.reference)?;
    let file = temp.path().join("auth.json");
    let credentials = file_credentials(&reference, &file)?;
    let mut state = server.state.lock().expect("state");
    state.token_response = Some(json!({"access_token":"publish-token", "token":"ignored-distribution-secret", "refresh_token":"ignored-rotation-secret"}).to_string().into_bytes());
    state.cancel_on = Some("POST /v2/test/blobs/uploads/".into());
    state
        .faults
        .insert("DELETE /v2/test/blobs/uploads/*".into(), 503);
    drop(state);
    let mut error = publish(
        &reference,
        Some(&credentials),
        publication(),
        &BTreeMap::new(),
        0,
        &server.cancelled,
    )
    .expect_err("cancelled with failed abort");
    assert!(error.is_cancelled());
    assert!(error.has_pending_cleanup());
    #[allow(clippy::disallowed_methods)] // This test owns the temporary input file.
    std::fs::remove_file(&file)?;
    server
        .state
        .lock()
        .expect("state")
        .faults
        .insert("POST /token".into(), 503);
    assert!(error.retry_upload_cleanup().is_err());
    assert!(error.has_pending_cleanup());
    server.state.lock().expect("state").faults.clear();
    error.retry_upload_cleanup()?;
    assert!(!error.has_pending_cleanup());
    assert!(!format!("{error:?} {error}").contains("secret"));
    let state = server.state.lock().expect("state");
    assert_eq!(
        state.token_forms.len(),
        4,
        "one initial grant and one per cleanup pass"
    );
    assert!(state.uploads.is_empty());
    assert!(state.manifests.is_empty());
    Ok(())
}
