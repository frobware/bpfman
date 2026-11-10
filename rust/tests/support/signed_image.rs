//! Fixed, unmodified public release bytes and Cosign evidence. No external I/O.
#![allow(clippy::expect_used)]

pub(super) const DIGEST: &str =
    "sha256:89728a806e0cccfaf6491e560a2624dc8f4b070547196fd9e9ce64c44afdf01b";
pub(super) const SUBJECT: &str =
    "https://github.com/bpfman/bpfman/.github/workflows/image-build.yml@refs/tags/v0.5.6";
pub(super) const ISSUER: &str = "https://token.actions.githubusercontent.com";

pub(super) fn registry() -> super::fixture::Registry {
    let registry =
        super::fixture::Registry::captured(include_str!("../fixtures/signed-xdp-v0.5.6.json"));
    let responses = registry.responses.lock().expect("routes");
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
