//! Publishing owns upload sessions, while blobs and manifests belong to registry GC.
#![allow(clippy::expect_used)]
#[path = "../../../tests/support/publish_registry.rs"]
mod fixture;
use bpfman_image_oci::{Bytecode, Publication, Reference, inspect, publish, pull};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::atomic::Ordering, time::Duration};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn elf() -> Vec<u8> {
    let mut bytes = vec![0; 64];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16] = 1;
    bytes[18] = 247;
    bytes
}

fn input(arch: &str) -> Bytecode {
    Bytecode::new(arch, "bytecode.o", elf()).expect("input")
}
fn labels() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("io.ebpf.programs".into(), "{\"pass\":\"xdp\"}".into()),
        ("io.ebpf.maps".into(), "{}".into()),
    ])
}

#[test]
fn images_indexes_reproducibility_and_consumption() -> Result {
    for index in [false, true] {
        let server = fixture::Registry::new();
        server.state.lock().expect("state").bearer = true;
        let reference = Reference::parse(&server.reference)?;
        let temp = tempfile::tempdir()?;
        let cache = bpfman_fs::RuntimeLayout::try_from(temp.path().join("cache"))?;
        let publication = || {
            if index {
                Publication::Index(vec![input("amd64"), input("arm64")])
            } else {
                Publication::Image(input("amd64"))
            }
        };
        let pinned = publish(&reference, publication(), &labels(), 123, &server.cancelled)?;
        assert!(pinned.contains("@sha256:"));
        assert_eq!(
            pinned,
            publish(&reference, publication(), &labels(), 123, &server.cancelled)?
        );
        let inspection = inspect(&reference, &server.cancelled)?;
        let child = if index {
            &inspection["manifests"][0]
        } else {
            &inspection
        };
        assert_eq!(child["programs"]["pass"], "xdp");
        let image = pull(
            &reference,
            bpfman_model::ImagePullPolicy::Always,
            &cache,
            Duration::ZERO,
            &server.cancelled,
            |_| Ok::<_, std::convert::Infallible>(()),
        )?;
        assert_eq!(image.bytes(), elf());
        let state = server.state.lock().expect("state");
        assert!(state.uploads.is_empty());
        assert!(state.requests.iter().any(|r| r.starts_with("GET /token?")
            && (r.contains("pull%2Cpush") || r.contains("pull,push"))));
        let manifest: Value = serde_json::from_slice(&state.manifests[image.digest()])?;
        let config: Value = serde_json::from_slice(
            &state.blobs[manifest["config"]["digest"].as_str().expect("digest")],
        )?;
        assert_eq!(config["created"], "1970-01-01T00:02:03Z");
        let layer = &state.blobs[manifest["layers"][0]["digest"].as_str().expect("layer")];
        assert_eq!(config["rootfs"]["diff_ids"], json!([fixture::hash(layer)]));
        let mut archive = tar::Archive::new(layer.as_slice());
        let entries = archive
            .entries()?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path()?.to_str(), Some("bytecode.o"));
        assert_eq!(entries[0].header().mtime()?, 123);
        assert_eq!(entries[0].header().mode()?, 0o644);
    }
    Ok(())
}

#[test]
fn failures_and_cancellation_preserve_tag_and_abort_sessions() -> Result {
    for (fault, cancel) in [
        ("HEAD /v2/test/blobs/*", false),
        ("POST /v2/test/blobs/uploads/", false),
        ("PUT /v2/test/blobs/uploads/*", false),
        ("PUT /v2/test/manifests/latest", false),
        ("POST /v2/test/blobs/uploads/", true),
    ] {
        let server = fixture::Registry::new();
        let reference = Reference::parse(&server.reference)?;
        {
            let mut state = server.state.lock().expect("state");
            state
                .manifests
                .insert("latest".into(), b"previous tag".to_vec());
            if cancel {
                state.cancel_on = Some(fault.into());
            } else {
                state.faults.insert(fault.into(), 503);
            }
        }
        let error = publish(
            &reference,
            Publication::Image(input("amd64")),
            &labels(),
            0,
            &server.cancelled,
        )
        .expect_err("failure");
        assert_eq!(error.is_cancelled(), cancel, "{error}");
        assert!(!error.has_pending_cleanup());
        let state = server.state.lock().expect("state");
        assert!(state.uploads.is_empty(), "{fault}");
        assert_eq!(state.manifests["latest"], b"previous tag");
    }
    Ok(())
}

#[test]
fn failed_abort_retains_receipt_for_explicit_retry() -> Result {
    let server = fixture::Registry::new();
    let reference = Reference::parse(&server.reference)?;
    {
        let mut state = server.state.lock().expect("state");
        state.cancel_on = Some("POST /v2/test/blobs/uploads/".into());
        state.location = Some("/v2/test/blobs/uploads/2?_state=secret-upload-state".into());
        state
            .faults
            .insert("DELETE /v2/test/blobs/uploads/*".into(), 503);
    }
    let mut error = publish(
        &reference,
        Publication::Image(input("amd64")),
        &labels(),
        0,
        &server.cancelled,
    )
    .expect_err("cancelled");
    assert!(error.is_cancelled());
    assert!(error.has_pending_cleanup());
    assert!(!format!("{error:?}").contains("secret-upload-state"));
    assert_eq!(server.state.lock().expect("state").uploads.len(), 1);
    assert!(error.retry_upload_cleanup().is_err());
    assert!(error.has_pending_cleanup());
    server.state.lock().expect("state").faults.clear();
    error.retry_upload_cleanup()?;
    assert!(!error.has_pending_cleanup());
    assert!(error.is_cancelled());
    assert!(server.state.lock().expect("state").uploads.is_empty());
    Ok(())
}

#[test]
fn cancellation_admission_digest_refusal_and_final_commit() -> Result {
    let server = fixture::Registry::new();
    let reference = Reference::parse(&server.reference)?;
    server.cancelled.store(true, Ordering::Relaxed);
    assert!(
        publish(
            &reference,
            Publication::Image(input("amd64")),
            &labels(),
            0,
            &server.cancelled
        )
        .expect_err("cancelled")
        .is_cancelled()
    );
    assert!(server.state.lock().expect("state").requests.is_empty());
    server.cancelled.store(false, Ordering::Relaxed);
    let mismatched = Reference::parse(&format!(
        "{}@{}",
        server
            .reference
            .split(':')
            .take(2)
            .collect::<Vec<_>>()
            .join(":"),
        fixture::hash(b"wrong")
    ))?;
    assert!(
        publish(
            &mismatched,
            Publication::Image(input("amd64")),
            &labels(),
            0,
            &server.cancelled
        )
        .is_err()
    );
    assert!(server.state.lock().expect("state").requests.is_empty());
    server.state.lock().expect("state").cancel_on = Some("PUT /v2/test/manifests/latest".into());
    assert!(
        publish(
            &reference,
            Publication::Image(input("amd64")),
            &labels(),
            0,
            &server.cancelled
        )
        .is_ok()
    );
    assert!(server.cancelled.load(Ordering::Relaxed));
    assert!(
        server
            .state
            .lock()
            .expect("state")
            .manifests
            .contains_key("latest")
    );
    Ok(())
}

#[test]
fn upload_locations_never_escape_the_repository() -> Result {
    let foreign = fixture::Registry::new();
    for location in [
        format!(
            "http://{}/v2/test/blobs/uploads/foreign",
            foreign.reference.split('/').next().expect("host")
        ),
        "/v2/other/blobs/uploads/foreign".into(),
        "/v2/test/blobs/uploads/../escape".into(),
        "/v2/test/blobs/uploads/%2e%2e/escape".into(),
        "//foreign.invalid/upload".into(),
    ] {
        let server = fixture::Registry::new();
        server.state.lock().expect("state").location = Some(location);
        assert!(
            publish(
                &Reference::parse(&server.reference)?,
                Publication::Image(input("amd64")),
                &labels(),
                0,
                &server.cancelled
            )
            .is_err()
        );
        assert!(foreign.state.lock().expect("state").requests.is_empty());
        assert!(
            !server
                .state
                .lock()
                .expect("state")
                .requests
                .iter()
                .any(|r| r.starts_with("PUT ") || r.starts_with("DELETE "))
        );
    }
    Ok(())
}
