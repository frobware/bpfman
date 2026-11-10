//! Captured Cosign v3 signatures, synchronous discovery and cache admission.
#![allow(clippy::expect_used)]
#[path = "../../../tests/support/oci.rs"]
mod fixture;

use base64::{Engine, engine::general_purpose::STANDARD};
use bpfman_image_oci::{Reference, SigningPolicy, TrustedIdentity, pull, verify};
use bpfman_model::ImagePullPolicy::{Always, IfNotPresent, Never};
use serde_json::{Value, json};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;
const DIGEST: &str = "sha256:1c363a6bf1455f6a642dc7d8becb3b8b06888f338b848ef70de841a844d6d9fd";
const SUBJECT: &str =
    "https://github.com/bpfman/bpfman/.github/workflows/image-build.yml@refs/heads/main";
const ISSUER: &str = "https://token.actions.githubusercontent.com";

fn endpoint() -> String {
    format!("/v2/test/referrers/{DIGEST}")
}

fn rewrite_bundles(server: &fixture::Registry, change: impl Fn(&mut Value, &mut Value)) {
    let mut routes = server.responses.lock().expect("routes");
    let mut index: Value = serde_json::from_slice(&routes[&endpoint()].1).expect("index");
    for candidate in index["manifests"].as_array_mut().expect("candidates") {
        let mut manifest: Value = serde_json::from_slice(
            &routes[&format!(
                "/v2/test/manifests/{}",
                candidate["digest"].as_str().expect("digest")
            )]
                .1,
        )
        .expect("manifest");
        let blob_path = format!(
            "/v2/test/blobs/{}",
            manifest["layers"][0]["digest"].as_str().expect("digest")
        );
        let mut bundle: Value = serde_json::from_slice(&routes[&blob_path].1).expect("bundle");
        change(&mut manifest, &mut bundle);
        let bytes = serde_json::to_vec(&bundle).expect("bundle bytes");
        manifest["layers"][0]["digest"] = json!(fixture::hash(&bytes));
        manifest["layers"][0]["size"] = json!(bytes.len());
        routes.insert(
            format!("/v2/test/blobs/{}", fixture::hash(&bytes)),
            (200, bytes),
        );
        let bytes = serde_json::to_vec(&manifest).expect("manifest bytes");
        candidate["digest"] = json!(fixture::hash(&bytes));
        candidate["size"] = json!(bytes.len());
        routes.insert(
            format!("/v2/test/manifests/{}", fixture::hash(&bytes)),
            (200, bytes),
        );
    }
    routes.insert(
        endpoint(),
        (200, serde_json::to_vec(&index).expect("index bytes")),
    );
}

fn assert_rejected_without_admission(server: &fixture::Registry) -> Result {
    let temp = tempfile::tempdir()?;
    let reference = Reference::parse(&server.reference)?;
    let layout = bpfman_fs::RuntimeLayout::try_from(temp.path().join("cache"))?;
    // Invalid evidence must remain invalid even when unsigned images are allowed.
    for allow_unsigned in [false, true] {
        assert!(
            pull(
                &reference,
                None,
                &SigningPolicy::verify(allow_unsigned, vec![])?,
                Always,
                &layout,
                Duration::ZERO,
                &server.cancelled,
                |_| Ok::<_, std::convert::Infallible>(())
            )
            .is_err()
        );
        assert!(!layout.root().exists());
    }
    Ok(())
}

fn registry() -> fixture::Registry {
    fixture::Registry::captured(include_str!("../../../tests/fixtures/signed-xdp-v0.3.json"))
}

fn policy(subject: &str) -> SigningPolicy {
    SigningPolicy::verify(
        false,
        vec![TrustedIdentity::new(subject, "", ISSUER, "").expect("identity")],
    )
    .expect("policy")
}

#[test]
fn real_dsse_index_verifies_and_cache_revalidation_preserves_prior_admission() -> Result {
    let server = registry();
    let reference = Reference::parse(&server.reference)?;
    let flag = AtomicBool::new(false);
    let trusted = policy(SUBJECT);
    assert_eq!(
        verify(&reference, None, &trusted, &flag)?,
        bpfman_image_oci::VerificationStatus::Verified
    );
    let temp = tempfile::tempdir()?;
    let layout = bpfman_fs::RuntimeLayout::try_from(temp.path().join("cache"))?;
    let resolve = |signing: &SigningPolicy, mode| {
        pull(
            &reference,
            None,
            signing,
            mode,
            &layout,
            Duration::ZERO,
            &flag,
            |_| Ok::<_, std::convert::Infallible>(()),
        )
    };
    let image = resolve(&trusted, Always)?;
    assert_eq!(image.digest(), server.digest);
    assert_ne!(image.digest(), DIGEST);
    let requests = server.requests.load(Ordering::Relaxed);
    for mode in [Never, IfNotPresent] {
        assert_eq!(resolve(&trusted, mode)?.bytes(), image.bytes());
    }
    assert_eq!(server.requests.load(Ordering::Relaxed), requests);
    let wrong = policy("https://example.invalid/untrusted");
    assert!(resolve(&wrong, Never).is_err());
    assert_eq!(server.requests.load(Ordering::Relaxed), requests);
    assert!(resolve(&wrong, IfNotPresent).is_err());
    assert_eq!(resolve(&trusted, Never)?.bytes(), image.bytes());
    Ok(())
}

#[test]
fn tampered_dsse_certificates_transparency_and_subjects_never_become_unsigned() -> Result {
    for mutation in 0..16 {
        let server = registry();
        rewrite_bundles(&server, |manifest, bundle| match mutation {
            0 => bundle["dsseEnvelope"]["signatures"][0]["sig"] = json!("YWR2ZXJzYXJ5"),
            1 => bundle["verificationMaterial"]["certificate"]["rawBytes"] = json!("YWR2ZXJzYXJ5"),
            2 => {
                bundle["verificationMaterial"]["tlogEntries"][0]["inclusionProof"]["rootHash"] =
                    json!(STANDARD.encode([0; 32]))
            }
            3 => {
                bundle["verificationMaterial"]["tlogEntries"][0]["inclusionProof"]["checkpoint"]["envelope"] =
                    json!("invalid checkpoint")
            }
            4 => {
                bundle["verificationMaterial"]["tlogEntries"][0]["canonicalizedBody"] =
                    json!("e30=")
            }
            5 => bundle["verificationMaterial"]["tlogEntries"][0]["integratedTime"] = json!("1"),
            6..=10 => {
                let mut statement: Value = serde_json::from_slice(
                    &STANDARD
                        .decode(bundle["dsseEnvelope"]["payload"].as_str().expect("payload"))
                        .expect("payload bytes"),
                )
                .expect("statement");
                match mutation {
                    6 => statement["subject"][0]["digest"]["sha256"] = json!("00".repeat(32)),
                    7 => statement["predicateType"] = json!("https://slsa.dev/provenance/v1"),
                    8 => statement["subject"] = json!([]),
                    9 => statement["_type"] = json!("https://in-toto.io/Statement/v0.1"),
                    _ => statement["predicate"] = json!({"changed": true}),
                }
                bundle["dsseEnvelope"]["payload"] = json!(
                    STANDARD.encode(serde_json::to_vec(&statement).expect("statement bytes"))
                );
            }
            11 => bundle["dsseEnvelope"]["payloadType"] = json!("application/json"),
            12 => manifest["subject"]["digest"] = json!("sha256:".to_owned() + &"00".repeat(32)),
            13 => manifest["layers"][0]["mediaType"] = json!("application/octet-stream"),
            14 => {
                manifest["mediaType"] =
                    json!("application/vnd.docker.distribution.manifest.v2+json")
            }
            _ => {
                bundle["mediaType"] = json!("application/vnd.dev.sigstore.bundle+json;version=0.2")
            }
        });
        assert_rejected_without_admission(&server)?;
    }
    Ok(())
}

#[test]
fn later_valid_signature_passes_but_relabeling_signed_subjects_does_not() -> Result {
    let server = registry();
    {
        let mut routes = server.responses.lock().expect("routes");
        let mut index: Value = serde_json::from_slice(&routes[&endpoint()].1)?;
        index["manifests"][0]["size"] = json!(1);
        // Optional index hints are never signer authority.
        for candidate in index["manifests"].as_array_mut().expect("candidates") {
            candidate["annotations"] =
                json!({"dev.sigstore.bundle.predicateType":"untrusted hint"});
        }
        routes.insert(endpoint(), (200, serde_json::to_vec(&index)?));
    }
    assert_eq!(
        verify(
            &Reference::parse(&server.reference)?,
            None,
            &policy(SUBJECT),
            &AtomicBool::new(false)
        )?,
        bpfman_image_oci::VerificationStatus::Verified
    );

    // Change the index while retaining precisely the same platform children.
    let changed = {
        let mut routes = server.responses.lock().expect("routes");
        let mut index: Value = serde_json::from_slice(&routes["/v2/test/manifests/latest"].1)?;
        index["annotations"] = json!({"changed": "same bytecode"});
        let bytes = serde_json::to_vec(&index)?;
        let digest = fixture::hash(&bytes);
        routes.insert("/v2/test/manifests/latest".into(), (200, bytes));
        routes.insert(
            format!("/v2/test/manifests/{}.sig", digest.replace(':', "-")),
            (404, br#"{"errors":[{"code":"MANIFEST_UNKNOWN"}]}"#.to_vec()),
        );
        digest
    };
    rewrite_bundles(&server, |manifest, _| {
        manifest["subject"]["digest"] = json!(changed)
    });
    {
        let mut routes = server.responses.lock().expect("routes");
        let response = routes[&endpoint()].clone();
        routes.insert(format!("/v2/test/referrers/{changed}"), response);
    }
    assert_rejected_without_admission(&server)?;
    Ok(())
}

#[test]
fn referrers_pagination_and_tag_fallback_verify_the_same_signed_index() -> Result {
    for fallback in [false, true] {
        let server = registry();
        {
            let mut routes = server.responses.lock().expect("routes");
            let index = routes[&endpoint()].clone();
            if fallback {
                // The referrers API's bare 404 is defined as unsupported by OCI.
                routes.insert(endpoint(), (404, b"not implemented".to_vec()));
                routes.insert(
                    format!("/v2/test/manifests/{}", DIGEST.replace(':', "-")),
                    index,
                );
            } else {
                routes.insert(format!("{}?next=2", endpoint()), index);
                routes.insert(
                    endpoint(),
                    (
                        200,
                        serde_json::to_vec(&json!({"schemaVersion":2,
                    "mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}))?,
                    ),
                );
                server.headers.lock().expect("headers").insert(
                    endpoint(),
                    format!("Link: <{}?next=2>; rel=\"next\"\r\n", endpoint()),
                );
            }
        }
        assert_eq!(
            verify(
                &Reference::parse(&server.reference)?,
                None,
                &policy(SUBJECT),
                &AtomicBool::new(false)
            )?,
            bpfman_image_oci::VerificationStatus::Verified
        );
    }
    Ok(())
}

#[test]
fn malformed_unavailable_or_unbounded_discovery_fails_closed() -> Result {
    for mutation in 0..13 {
        let server = registry();
        {
            let mut routes = server.responses.lock().expect("routes");
            let mut index: Value = serde_json::from_slice(&routes[&endpoint()].1)?;
            let response = match mutation {
                0 => (503, Vec::new()),
                1 => (403, Vec::new()),
                2 => (401, Vec::new()),
                3 => (200, b"{}".to_vec()),
                4 => {
                    index["manifests"] = json!([null]);
                    (200, serde_json::to_vec(&index)?)
                }
                5 => {
                    index["manifests"] = json!(vec![index["manifests"][0].clone(); 65]);
                    (200, serde_json::to_vec(&index)?)
                }
                6 => (200, vec![b' '; 4 * 1024 * 1024 + 1]),
                7..=11 => {
                    let target = match mutation {
                        7 => "http://example.invalid/v2/test/referrers/foreign".into(),
                        8 => "/v2/foreign/referrers/subject?next=2".into(),
                        9 => format!("{}?next=missing", endpoint()),
                        10 => endpoint(),
                        _ => format!("{}?next=2", endpoint()),
                    };
                    if mutation == 11 {
                        for page in 2..=16 {
                            let path = format!("{}?next={page}", endpoint());
                            routes.insert(path.clone(), (200, serde_json::to_vec(&index)?));
                            server.headers.lock().expect("headers").insert(
                                path,
                                format!(
                                    "Link: <{}?next={}>; rel=\"next\"\r\n",
                                    endpoint(),
                                    page + 1
                                ),
                            );
                        }
                    }
                    server
                        .headers
                        .lock()
                        .expect("headers")
                        .insert(endpoint(), format!("Link: <{target}>; rel=\"next\"\r\n"));
                    (200, serde_json::to_vec(&index)?)
                }
                _ => {
                    // Unsupported API followed by an unrecognized fallback 404
                    // is an observation failure, not confirmed unsigned evidence.
                    (404, Vec::new())
                }
            };
            routes.insert(endpoint(), response);
        }
        assert_rejected_without_admission(&server)?;
    }
    Ok(())
}

#[test]
fn cancellation_at_discovery_manifest_and_bundle_reads_prevents_cache_creation() -> Result {
    for stage in 0..3 {
        let server = registry();
        let path = {
            let routes = server.responses.lock().expect("routes");
            let index: Value = serde_json::from_slice(&routes[&endpoint()].1)?;
            let manifest_path = format!(
                "/v2/test/manifests/{}",
                index["manifests"][0]["digest"].as_str().expect("digest")
            );
            let manifest: Value = serde_json::from_slice(&routes[&manifest_path].1)?;
            match stage {
                0 => endpoint(),
                1 => manifest_path,
                _ => format!(
                    "/v2/test/blobs/{}",
                    manifest["layers"][0]["digest"].as_str().expect("digest")
                ),
            }
        };
        *server.cancel_on.lock().expect("cancel request") = Some(path);
        let temp = tempfile::tempdir()?;
        let layout = bpfman_fs::RuntimeLayout::try_from(temp.path().join("cache"))?;
        let error = pull(
            &Reference::parse(&server.reference)?,
            None,
            &policy(SUBJECT),
            Always,
            &layout,
            Duration::ZERO,
            &server.cancelled,
            |_| Ok::<_, std::convert::Infallible>(()),
        )
        .expect_err("cancelled");
        assert!(error.is_cancelled());
        assert!(!layout.root().exists());
    }
    Ok(())
}
