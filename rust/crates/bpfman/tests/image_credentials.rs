//! Credential-file discovery through real CLI processes and loopback registries.
#![allow(clippy::expect_used)]
#[path = "../../../tests/support/oci.rs"]
mod fixture;
#[path = "../../../tests/support/publish_registry.rs"]
mod registry;
#[path = "../../../tests/support/signed_image.rs"]
mod signed_image;

use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;
const SECRET: &str = "file-password:with:colons";

fn encoded() -> String {
    STANDARD.encode(format!("file-user:{SECRET}"))
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman"));
    command
        .arg("--runtime-dir")
        .arg(root.join("runtime"))
        .arg("--image-cache-dir")
        .arg(root.join("cache"))
        .arg("--config")
        .arg(root.join("config.toml"))
        .env_remove("BPFMAN_CONFIG")
        .env_remove("BPFMAN_REGISTRY_AUTH")
        .env_remove("REGISTRY_AUTH_FILE")
        .env_remove("SOURCE_DATE_EPOCH")
        .env("HOME", root.join("home"))
        .env("XDG_RUNTIME_DIR", root.join("xdg-runtime"))
        .env("DOCKER_CONFIG", root.join("docker"))
        .env("SUDO_USER", "ignored-sudo-user")
        .env("XDG_CONFIG_HOME", root.join("ignored-config"));
    command
}

fn setup() -> std::result::Result<tempfile::TempDir, Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    std::fs::write(
        root.path().join("config.toml"),
        "[signing]\ntrust_source = 'bundled'\n",
    )?;
    Ok(root)
}

fn auth_file(root: &Path, host: &str, path: &str) -> std::io::Result<PathBuf> {
    let file = root.join(path);
    std::fs::create_dir_all(file.parent().expect("parent"))?;
    std::fs::write(
        &file,
        json!({"auths":{host:{"auth":encoded()}}}).to_string(),
    )?;
    Ok(file)
}

fn success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn configure(server: &registry::Registry, bearer: bool) {
    assert!(!server.cancelled.load(std::sync::atomic::Ordering::Relaxed));
    let mut state = server.state.lock().expect("state");
    state.required_auth = Some(format!("Basic {}", encoded()));
    state.bearer = bearer;
}

#[test]
fn every_file_location_authenticates_build_inspect_verify_and_load() -> Result {
    let object =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../e2e/testdata/bpf/xdp_pass.bpf.o");
    for bearer in [false, true] {
        for location in ["explicit", "runtime", "podman", "docker", "docker-default"] {
            let temp = setup()?;
            let root = temp.path();
            let server = registry::Registry::new();
            configure(&server, bearer);
            let host = server.reference.split('/').next().ok_or("host")?;
            let path = match location {
                "explicit" => "explicit/auth.json",
                "runtime" => "xdg-runtime/containers/auth.json",
                "podman" => "home/.config/containers/auth.json",
                "docker" => "docker/config.json",
                _ => "home/.docker/config.json",
            };
            let file = auth_file(root, host, path)?;
            let before = std::fs::read(&file)?;
            let process = || {
                let mut process = command(root);
                if location == "explicit" {
                    process.env("REGISTRY_AUTH_FILE", &file);
                }
                if location == "docker-default" {
                    process.env_remove("DOCKER_CONFIG");
                }
                process
            };
            let trace = root.join("trace.json");
            success(
                process()
                    .arg("--trace-file")
                    .arg(&trace)
                    .args(["image", "build", &server.reference])
                    .arg(&object)
                    .env("RUST_LOG", "trace,ureq=trace,ureq_proto=trace")
                    .output()?,
            );
            let inspected: serde_json::Value = serde_json::from_slice(&success(
                process()
                    .args(["image", "inspect", &server.reference])
                    .output()?,
            ))?;
            assert_eq!(inspected["programs"]["pass"], "xdp");
            success(
                process()
                    .args(["image", "inspect", &server.reference, "--registry-auth", ""])
                    .output()?,
            );
            assert!(
                String::from_utf8(success(
                    process()
                        .args(["image", "verify", &server.reference])
                        .output()?
                ))?
                .contains("unsigned image accepted")
            );
            let output = process()
                .args([
                    "program",
                    "load",
                    "image",
                    &server.reference,
                    "--programs",
                    "xdp:missing_program",
                ])
                .output()?;
            assert!(!output.status.success());
            assert!(
                String::from_utf8_lossy(&output.stderr).contains("missing_program"),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let trace = std::fs::read_to_string(trace)?;
            for secret in [SECRET, &encoded(), "publish-token"] {
                assert!(!trace.contains(secret), "credential trace leak");
                assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
            }
            assert_eq!(
                std::fs::read(&file)?,
                before,
                "credential files are read-only"
            );
            assert!(!root.join("runtime").exists());
            assert!(!root.join("cache").exists());
        }
    }
    Ok(())
}

#[test]
fn explicit_auth_wins_and_wrong_explicit_auth_never_falls_back() -> Result {
    for bearer in [false, true] {
        let temp = setup()?;
        let root = temp.path();
        let server = registry::Registry::new();
        configure(&server, bearer);
        let host = server.reference.split('/').next().ok_or("host")?;
        let file = auth_file(root, host, "auth.json")?;
        for via_env in [false, true] {
            let mut process = command(root);
            process
                .args(["image", "inspect", &server.reference])
                .env("REGISTRY_AUTH_FILE", &file);
            if via_env {
                process.env("BPFMAN_REGISTRY_AUTH", STANDARD.encode("wrong:password"));
            } else {
                process.args(["--registry-auth", &STANDARD.encode("wrong:password")]);
            }
            assert!(!process.output()?.status.success());
        }
        std::fs::write(&file, "malformed-credential-file-secret")?;
        // Neither valid explicit source needs to parse the malformed file.
        let object =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../e2e/testdata/bpf/xdp_pass.bpf.o");
        success(
            command(root)
                .env("REGISTRY_AUTH_FILE", &file)
                .env("BPFMAN_REGISTRY_AUTH", encoded())
                .args(["image", "build", &server.reference])
                .arg(object)
                .output()?,
        );
        for via_env in [false, true] {
            let mut process = command(root);
            process
                .args(["image", "inspect", &server.reference])
                .env("REGISTRY_AUTH_FILE", &file);
            if via_env {
                process.env("BPFMAN_REGISTRY_AUTH", encoded());
            } else {
                process
                    .args(["--registry-auth", &encoded()])
                    .env("BPFMAN_REGISTRY_AUTH", "bad-env");
            }
            success(process.output()?);
        }
    }
    Ok(())
}

#[test]
fn file_credentials_verify_real_signatures_before_load_admission() -> Result {
    for bearer in [false, true] {
        let temp = setup()?;
        let root = temp.path();
        let captured = signed_image::registry();
        assert!(captured.reference.starts_with("127.0.0.1:"));
        assert_eq!(
            captured.requests.load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        let server = registry::Registry::new();
        configure(&server, bearer);
        let host = server.reference.split('/').next().ok_or("host")?;
        let file = auth_file(root, host, "home/.config/containers/auth.json")?;
        let mut state = server.state.lock().expect("state");
        for (path, (_, body)) in captured.responses.lock().expect("routes").iter() {
            if let Some(selector) = path.strip_prefix("/v2/test/manifests/") {
                state.manifests.insert(selector.into(), body.clone());
            }
            if let Some(digest) = path.strip_prefix("/v2/test/blobs/") {
                state.blobs.insert(digest.into(), body.clone());
            }
        }
        drop(state);
        let output = success(
            command(root)
                .args([
                    "image",
                    "verify",
                    &server.reference,
                    "--allow-unsigned=false",
                    "--certificate-identity",
                    signed_image::SUBJECT,
                    "--certificate-oidc-issuer",
                    signed_image::ISSUER,
                ])
                .output()?,
        );
        assert!(String::from_utf8(output)?.contains("signature verified"));
        let output = command(root)
            .args([
                "program",
                "load",
                "image",
                &server.reference,
                "--programs",
                "xdp:missing_program",
            ])
            .output()?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("missing_program"));
        assert_eq!(
            std::fs::read_dir(file.parent().ok_or("parent")?)?.count(),
            1
        );
        assert!(!root.join("runtime").exists());
        assert!(!root.join("cache").exists());
    }
    Ok(())
}

#[test]
fn malformed_or_foreign_files_do_not_affect_anonymous_requests_or_never_cache_misses() -> Result {
    let temp = setup()?;
    let root = temp.path();
    let server = registry::Registry::new();
    let file = root.join("auth.json");
    std::fs::write(&file, "malformed-credential-secret")?;
    // A missing anonymous manifest is a registry error, without reading auth.
    let output = command(root)
        .env("REGISTRY_AUTH_FILE", &file)
        .args(["image", "inspect", &server.reference])
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains("manifest not found"), "{stderr}");
    let output = command(root)
        .env("REGISTRY_AUTH_FILE", &file)
        .args([
            "program",
            "load",
            "image",
            &server.reference,
            "--programs",
            "xdp:pass",
            "--pull-policy",
            "Never",
        ])
        .output()?;
    assert!(String::from_utf8_lossy(&output.stderr).contains("pull policy is Never"));
    configure(&server, false);
    let output = command(root)
        .env("REGISTRY_AUTH_FILE", &file)
        .args(["image", "inspect", &server.reference])
        .output()?;
    assert!(String::from_utf8_lossy(&output.stderr).contains("credential file JSON"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("malformed-credential-secret"));
    std::fs::write(
        &file,
        json!({"auths":{"unrelated.test":{"auth":encoded()}}}).to_string(),
    )?;
    let output = command(root)
        .env("REGISTRY_AUTH_FILE", &file)
        .args(["image", "inspect", &server.reference])
        .output()?;
    assert!(!output.status.success());
    assert!(
        server
            .state
            .lock()
            .expect("state")
            .authorizations
            .iter()
            .all(|(_, auth)| auth.is_none())
    );
    assert!(!root.join("runtime").exists());
    assert!(!root.join("cache").exists());
    Ok(())
}
