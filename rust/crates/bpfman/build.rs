//! Capture build metadata without making repository or clock access part of the CLI.

use std::{env, process::Command};

fn main() {
    for field in [
        "VERSION",
        "GIT_COMMIT",
        "GIT_BRANCH",
        "GIT_STATE",
        "BUILD_DATE",
        "IMAGE_REF",
        "SIGNER_IDENTITY",
        "OIDC_ISSUER",
    ] {
        let name = format!("BPFMAN_BUILD_{field}");
        println!("cargo:rerun-if-env-changed={name}");
        let value = env::var(&name).unwrap_or_default();
        println!("cargo:rustc-env={name}={value}");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RUSTC");
    let compiler = env::var_os("RUSTC")
        .and_then(|rustc| {
            Command::new(rustc)
                .arg("--version")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
        })
        .unwrap_or_default();
    println!("cargo:rustc-env=BPFMAN_BUILD_COMPILER={}", compiler.trim());
    let os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let arch = match arch.as_str() {
        "x86_64" => "amd64",
        "x86" => "386",
        "aarch64" => "arm64",
        "loongarch64" => "loong64",
        other => other,
    };
    println!("cargo:rustc-env=BPFMAN_BUILD_PLATFORM={os}/{arch}");
}
