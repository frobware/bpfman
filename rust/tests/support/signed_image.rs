//! Fixed, unmodified public release bytes and Cosign evidence. No external I/O.
#![allow(clippy::expect_used)]
use base64::{Engine, engine::general_purpose::STANDARD};

pub(super) const DIGEST: &str =
    "sha256:89728a806e0cccfaf6491e560a2624dc8f4b070547196fd9e9ce64c44afdf01b";
pub(super) const SUBJECT: &str =
    "https://github.com/bpfman/bpfman/.github/workflows/image-build.yml@refs/tags/v0.5.6";
pub(super) const ISSUER: &str = "https://token.actions.githubusercontent.com";

pub(super) fn registry() -> super::fixture::Registry {
    let mut registry = super::fixture::Registry::new(Vec::new(), false);
    let captured: serde_json::Value =
        serde_json::from_str(include_str!("../fixtures/signed-xdp-v0.5.6.json"))
            .expect("fixed signature fixture");
    let routes = captured["routes"].as_object().expect("routes");
    let top: serde_json::Value = serde_json::from_slice(
        &STANDARD
            .decode(routes["/manifests/latest"].as_str().expect("index"))
            .expect("index bytes"),
    )
    .expect("index");
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "powerpc64" if cfg!(target_endian = "little") => "ppc64le",
        other => other,
    };
    registry.digest = top["manifests"]
        .as_array()
        .expect("platforms")
        .iter()
        .find(|child| child["platform"]["architecture"] == architecture)
        .expect("host child")["digest"]
        .as_str()
        .expect("digest")
        .into();
    registry
        .bearer
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let mut responses = registry.responses.lock().expect("routes");
    responses.clear();
    for (path, encoded) in routes {
        responses.insert(
            format!("/v2/test{path}"),
            (
                200,
                STANDARD
                    .decode(encoded.as_str().expect("base64"))
                    .expect("fixture bytes"),
            ),
        );
    }
    assert!(
        responses.contains_key(&signature_path()),
        "fixture contains the signature of its pinned index"
    );
    drop(responses);
    registry
}

pub(super) fn signature_path() -> String {
    format!("/v2/test/manifests/{}.sig", DIGEST.replace(':', "-"))
}
