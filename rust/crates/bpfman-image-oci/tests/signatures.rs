//! Real signed release, loopback HTTP and confined cache admission contracts.
#![allow(clippy::expect_used)]
#[path = "../../../tests/support/oci.rs"]
mod fixture;
#[path = "../../../tests/support/signed_image.rs"]
mod signed_image;

use bpfman_image_oci::{Reference, SigningPolicy, TrustedIdentity, pull};
use bpfman_model::ImagePullPolicy::{Always, IfNotPresent, Never};
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn strict() -> SigningPolicy {
    SigningPolicy::verify(false, vec![]).expect("signing policy")
}

fn trusted(subject: &str) -> SigningPolicy {
    SigningPolicy::verify(
        false,
        vec![TrustedIdentity::new(subject, "", signed_image::ISSUER, "").expect("identity")],
    )
    .expect("policy")
}

fn resolve(
    server: &fixture::Registry,
    signing: &SigningPolicy,
    policy: bpfman_model::ImagePullPolicy,
    layout: &bpfman_fs::RuntimeLayout,
) -> std::result::Result<bpfman_image_oci::Image, bpfman_image_oci::Error> {
    pull(
        &Reference::parse(&server.reference)?,
        None,
        signing,
        policy,
        layout,
        Duration::ZERO,
        &AtomicBool::new(false),
        |_| Ok::<_, std::convert::Infallible>(()),
    )
}

#[test]
fn signed_index_and_policy_changes_reverify_original_digest_without_resolving_tag() -> Result {
    let server = signed_image::registry();
    let temp = tempfile::tempdir()?;
    let layout = bpfman_fs::RuntimeLayout::try_from(temp.path().join("cache"))?;
    let policy = strict();
    let image = resolve(&server, &policy, Always, &layout)?;
    assert_eq!(image.digest(), server.digest);
    assert_ne!(
        image.digest(),
        signed_image::DIGEST,
        "fixture signs the index, not its platform child"
    );
    let count = server.requests.load(Ordering::Relaxed);
    for mode in [Never, IfNotPresent] {
        assert_eq!(
            resolve(&server, &policy, mode, &layout)?.bytes(),
            image.bytes()
        );
    }
    assert_eq!(server.requests.load(Ordering::Relaxed), count);
    let trusted = trusted(signed_image::SUBJECT);
    assert!(resolve(&server, &trusted, Never, &layout).is_err());
    assert_eq!(server.requests.load(Ordering::Relaxed), count);
    server
        .responses
        .lock()
        .expect("routes")
        .insert("/v2/test/manifests/latest".into(), (503, Vec::new()));
    assert_eq!(
        resolve(&server, &trusted, IfNotPresent, &layout)?.bytes(),
        image.bytes()
    );
    let count = server.requests.load(Ordering::Relaxed);
    assert_eq!(
        resolve(&server, &trusted, Never, &layout)?.bytes(),
        image.bytes()
    );
    assert_eq!(server.requests.load(Ordering::Relaxed), count);
    let wrong = trusted_policy_wrong();
    assert!(resolve(&server, &wrong, IfNotPresent, &layout).is_err());
    assert_eq!(
        resolve(&server, &trusted, Never, &layout)?.bytes(),
        image.bytes()
    );
    assert!(resolve(&server, &trusted, Always, &layout).is_err());
    assert_eq!(
        resolve(&server, &trusted, Never, &layout)?.bytes(),
        image.bytes()
    );
    Ok(())
}

fn trusted_policy_wrong() -> SigningPolicy {
    trusted("https://example.invalid/untrusted")
}

#[test]
fn policy_revalidation_preserves_a_concurrently_replaced_cache_generation() -> Result {
    let server = signed_image::registry();
    let temp = tempfile::tempdir()?;
    let layout = bpfman_fs::RuntimeLayout::try_from(temp.path().join("cache"))?;
    resolve(&server, &SigningPolicy::disabled(), Always, &layout)?;
    let permissive = SigningPolicy::verify(true, vec![])?;
    let (arrived, arrival) = std::sync::mpsc::sync_channel(1);
    let (resume, resumed) = std::sync::mpsc::sync_channel(1);
    std::thread::scope(|threads| {
        let server = &server;
        let layout = &layout;
        let worker = threads.spawn(move || {
            let first = std::cell::Cell::new(true);
            pull(
                &Reference::parse(&server.reference).expect("reference"),
                None,
                &strict(),
                IfNotPresent,
                layout,
                Duration::ZERO,
                &AtomicBool::new(false),
                |_| {
                    if first.replace(false) {
                        arrived.send(()).expect("announce captured cache");
                        resumed
                            .recv_timeout(Duration::from_secs(5))
                            .expect("resume admission");
                    }
                    Ok::<_, std::convert::Infallible>(())
                },
            )
        });
        arrival
            .recv_timeout(Duration::from_secs(5))
            .expect("captured cache");
        resolve(server, &permissive, Always, layout).expect("concurrent publication");
        resume.send(()).expect("resume");
        let error = worker
            .join()
            .expect("reader")
            .expect_err("changed generation");
        assert!(
            error
                .to_string()
                .contains("changed during signature verification")
        );
    });
    resolve(&server, &permissive, Never, &layout)?;
    assert!(resolve(&server, &strict(), Never, &layout).is_err());
    Ok(())
}

#[test]
fn older_disabled_cache_entries_require_a_fresh_verified_pull() -> Result {
    let server = signed_image::registry();
    let temp = tempfile::tempdir()?;
    let layout = bpfman_fs::RuntimeLayout::try_from(temp.path().join("cache"))?;
    let disabled = SigningPolicy::disabled();
    resolve(&server, &disabled, Always, &layout)?;
    let path = std::fs::read_dir(layout.root().join("images-v1"))?
        .next()
        .expect("cache entry")?
        .path();
    let mut envelope: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    envelope
        .as_object_mut()
        .expect("envelope")
        .remove("verified_digest");
    std::fs::write(&path, serde_json::to_vec(&envelope)?)?;
    let count = server.requests.load(Ordering::Relaxed);
    resolve(&server, &disabled, Never, &layout)?;
    assert!(resolve(&server, &strict(), Never, &layout).is_err());
    assert_eq!(server.requests.load(Ordering::Relaxed), count);
    let old = server
        .responses
        .lock()
        .expect("routes")
        .insert("/v2/test/manifests/latest".into(), (503, Vec::new()))
        .expect("index");
    assert!(resolve(&server, &strict(), IfNotPresent, &layout).is_err());
    resolve(&server, &disabled, Never, &layout)?;
    server
        .responses
        .lock()
        .expect("routes")
        .insert("/v2/test/manifests/latest".into(), old);
    resolve(&server, &strict(), IfNotPresent, &layout)?;
    resolve(&server, &strict(), Never, &layout)?;
    Ok(())
}

#[test]
fn unsigned_admission_only_accepts_structured_missing_signatures() -> Result {
    let server = signed_image::registry();
    let temp = tempfile::tempdir()?;
    let permissive = SigningPolicy::verify(true, vec![])?;
    for (status, body, allowed) in [
        (
            404,
            serde_json::to_vec(&json!({"errors":[{"code":"MANIFEST_UNKNOWN"}]}))?,
            true,
        ),
        (404, b"not a registry response".to_vec(), false),
        (401, Vec::new(), false),
        (403, Vec::new(), false),
        (503, Vec::new(), false),
        (200, b"{}".to_vec(), false),
    ] {
        server
            .responses
            .lock()
            .expect("routes")
            .insert(signed_image::signature_path(), (status, body));
        let layout = bpfman_fs::RuntimeLayout::try_from(
            temp.path().join(format!("cache-{status}-{}", allowed)),
        )?;
        assert!(resolve(&server, &strict(), Always, &layout).is_err());
        assert!(!layout.root().exists());
        assert_eq!(
            resolve(&server, &permissive, Always, &layout).is_ok(),
            allowed
        );
        assert_eq!(layout.root().exists(), allowed);
    }
    Ok(())
}

#[test]
fn signatures_bind_the_index_and_a_later_valid_signature_can_admit_it() -> Result {
    let server = signed_image::registry();
    let temp = tempfile::tempdir()?;
    let layout = bpfman_fs::RuntimeLayout::try_from(temp.path().join("cache"))?;
    let mut routes = server.responses.lock().expect("routes");
    let signature = &mut routes
        .get_mut(&signed_image::signature_path())
        .expect("signatures")
        .1;
    let mut manifest: Value = serde_json::from_slice(signature)?;
    manifest["layers"][0]["annotations"]["dev.cosignproject.cosign/signature"] =
        json!("YWR2ZXJzYXJ5");
    *signature = serde_json::to_vec(&manifest)?;
    drop(routes);
    resolve(&server, &strict(), Always, &layout)?;

    let mut routes = server.responses.lock().expect("routes");
    let signature = routes[&signed_image::signature_path()].clone();
    let index = &mut routes
        .get_mut("/v2/test/manifests/latest")
        .expect("index")
        .1;
    let mut value: Value = serde_json::from_slice(index)?;
    value["annotations"] = json!({"test": "different index, same platform child"});
    *index = serde_json::to_vec(&value)?;
    let changed_digest = fixture::hash(index);
    routes.insert(
        format!(
            "/v2/test/manifests/{}.sig",
            changed_digest.replace(':', "-")
        ),
        signature,
    );
    drop(routes);
    // A signature for the original index cannot authorize this different index,
    // even though both select exactly the same platform manifest and bytecode.
    assert!(
        resolve(
            &server,
            &SigningPolicy::verify(true, vec![])?,
            Always,
            &layout
        )
        .is_err()
    );
    resolve(&server, &strict(), Never, &layout)?;
    Ok(())
}

#[test]
fn tampered_signatures_claims_certificates_and_log_evidence_never_become_unsigned() -> Result {
    let temp = tempfile::tempdir()?;
    let permissive = SigningPolicy::verify(true, vec![])?;
    for mutation in 0..7 {
        let server = signed_image::registry();
        let mut routes = server.responses.lock().expect("routes");
        let (_, body) = routes
            .get_mut(&signed_image::signature_path())
            .expect("signature manifest");
        let mut manifest: Value = serde_json::from_slice(body)?;
        for layer in manifest["layers"].as_array_mut().expect("layers") {
            let annotations = &mut layer["annotations"];
            match mutation {
                0 => annotations["dev.cosignproject.cosign/signature"] = json!("YWR2ZXJzYXJ5"),
                1 => annotations["dev.sigstore.cosign/certificate"] = json!("not a certificate"),
                2..=5 => {
                    let mut bundle: Value = serde_json::from_str(
                        annotations["dev.sigstore.cosign/bundle"]
                            .as_str()
                            .expect("bundle"),
                    )?;
                    match mutation {
                        2 => bundle["SignedEntryTimestamp"] = json!("YWR2ZXJzYXJ5"),
                        3 => bundle["Payload"]["integratedTime"] = json!(1),
                        4 => bundle["Payload"]["logID"] = json!("00".repeat(32)),
                        _ => bundle["Payload"]["body"] = json!("e30="),
                    }
                    annotations["dev.sigstore.cosign/bundle"] =
                        json!(serde_json::to_string(&bundle)?);
                }
                _ => {
                    // Replacing the payload and descriptor cannot replace its
                    // independently signed claim about the original index.
                    layer["digest"] = json!(signed_image::DIGEST);
                    layer["size"] = json!(1);
                }
            }
        }
        *body = serde_json::to_vec(&manifest)?;
        drop(routes);
        let layout =
            bpfman_fs::RuntimeLayout::try_from(temp.path().join(format!("cache-{mutation}")))?;
        assert!(resolve(&server, &permissive, Always, &layout).is_err());
        assert!(!layout.root().exists());
    }
    Ok(())
}
