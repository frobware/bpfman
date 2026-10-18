//! Build information is independent of runtime state, caches and kernel privileges.

use std::io::{self, Write};

pub(super) fn write(out: &mut impl Write) -> io::Result<()> {
    let version = env!("BPFMAN_BUILD_VERSION");
    writeln!(
        out,
        "Version:     {}",
        if version.is_empty() {
            "(devel)"
        } else {
            version
        }
    )?;
    for (label, value) in [
        ("Git commit", env!("BPFMAN_BUILD_GIT_COMMIT")),
        ("Git branch", env!("BPFMAN_BUILD_GIT_BRANCH")),
        ("Git state", env!("BPFMAN_BUILD_GIT_STATE")),
        ("Build date", env!("BPFMAN_BUILD_BUILD_DATE")),
        ("Rust version", env!("BPFMAN_BUILD_COMPILER")),
        ("Platform", env!("BPFMAN_BUILD_PLATFORM")),
    ] {
        writeln!(
            out,
            "{label:13}{separator}{value}",
            label = format!("{label}:"),
            separator = if label == "Rust version" { " " } else { "" }
        )?;
    }
    let image = env!("BPFMAN_BUILD_IMAGE_REF");
    let identity = env!("BPFMAN_BUILD_SIGNER_IDENTITY");
    let issuer = env!("BPFMAN_BUILD_OIDC_ISSUER");
    if !image.is_empty() && !identity.is_empty() && !issuer.is_empty() {
        writeln!(
            out,
            "Attestation: cosign verify {image}:latest --certificate-identity '{identity}' --certificate-oidc-issuer {issuer}"
        )?;
    }
    Ok(())
}
