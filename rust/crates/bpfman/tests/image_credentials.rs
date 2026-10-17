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
const IDENTITY: &str = "refresh-secret+/: &=?";
const REFRESH_FORM: &str = "grant_type=refresh_token&refresh_token=refresh-secret%2B%2F%3A+%26%3D%3F&service=fixture&client_id=bpfman&scope=repository%3Atest%3A";

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

fn configure_identity(server: &registry::Registry, actions: &str) {
    let mut state = server.state.lock().expect("state");
    state.bearer = true;
    state.required_auth = None;
    state.refresh_form = Some(format!("{REFRESH_FORM}{actions}"));
}

fn identity_command(root: &Path, helper: bool) -> Command {
    let mut process = if helper {
        helper_command(root)
    } else {
        command(root)
    };
    process.env("REGISTRY_AUTH_FILE", root.join("auth.json"));
    if helper {
        process.env(
            "HELPER_RESPONSE",
            json!({"Username":"<token>", "Secret":IDENTITY}).to_string(),
        );
    }
    process
}

fn identity_config(root: &Path, host: &str, helper: bool) -> std::io::Result<()> {
    if helper {
        install_helper(root, "selected")?;
        helper_config(root, host, true)
    } else {
        std::fs::write(
            root.join("auth.json"),
            json!({"auths":{host:{"identitytoken":IDENTITY, "auth":encoded()}}}).to_string(),
        )
    }
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
fn file_and_helper_credentials_verify_real_signatures_before_load_admission() -> Result {
    for (bearer, helper, identity) in [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (true, true, false),
        (true, false, true),
        (true, true, true),
    ] {
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
        if helper {
            install_helper(root, "selected")?;
            helper_config(root, host, true)?;
        }
        if identity {
            identity_config(root, host, helper)?;
            configure_identity(&server, "pull");
        }
        let process = || {
            if identity {
                identity_command(root, helper)
            } else if helper {
                helper_command(root)
            } else {
                command(root)
            }
        };
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
            process()
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
        assert!(String::from_utf8_lossy(&output.stderr).contains("missing_program"));
        assert_eq!(
            std::fs::read_dir(file.parent().ok_or("parent")?)?.count(),
            1
        );
        assert!(!root.join("runtime").exists());
        assert!(!root.join("cache").exists());
        if helper {
            assert_helper_reaped(root)?;
        }
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

const HELPER: &str = r#"#!/bin/sh
[ "$#" = 1 ] && [ "$1" = get ] || exit 11
IFS= read -r registry || :
printf '%s %s %s\n' "${0##*/}" "$1" "$registry" >> "$HELPER_CALLS"
printf '%s' "$$" > "$HELPER_PID"
printf 'helper-stderr-secret' >&2
case "$HELPER_MODE" in
    hang) while :; do :; done ;;
    fail) printf 'helper-failure-secret'; exit 1 ;;
    malformed) printf '{"Username":{"helper-parser-secret":true}}'; exit 0 ;;
    token) printf '{"Username":"<token>","Secret":"helper-identity-secret"}'; exit 0 ;;
    absent) printf 'credentials not found in native keychain'; exit 1 ;;
    empty) printf '{}'; exit 0 ;;
    poison)
        printf 'changed-credential-file-secret' > "$REGISTRY_AUTH_FILE"
        printf '#!/bin/sh\nexit 99\n' > "$0"
        ;;
esac
printf '%s' "$HELPER_RESPONSE"
"#;

fn install_helper(root: &Path, name: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let directory = root.join("helpers");
    std::fs::create_dir_all(&directory)?;
    let file = directory.join(format!("docker-credential-{name}"));
    std::fs::write(&file, HELPER)?;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o700))?;
    Ok(file)
}

fn helper_command(root: &Path) -> Command {
    let mut process = command(root);
    process
        .env("REGISTRY_AUTH_FILE", root.join("auth.json"))
        .env("PATH", root.join("helpers"))
        .env("HELPER_CALLS", root.join("helper.calls"))
        .env("HELPER_PID", root.join("helper.pid"))
        .env("HELPER_MODE", "")
        .env(
            "HELPER_RESPONSE",
            json!({"Username":"file-user", "Secret":SECRET}).to_string(),
        );
    process
}

fn helper_config(root: &Path, host: &str, specific: bool) -> std::io::Result<()> {
    std::fs::write(root.join("auth.json"), if specific {
        // The registry-specific helper must override this missing global helper
        // and a malformed inline entry, without interpreting either one.
        json!({"credHelpers":{host:"selected"}, "credsStore":"missing-global", "auths":{host:false}})
    } else {
        json!({"credsStore":"selected", "auths":{host:false}})
    }.to_string())
}

fn assert_helper_reaped(root: &Path) -> Result {
    let pid: u32 = std::fs::read_to_string(root.join("helper.pid"))?.parse()?;
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "helper must be reaped before CLI returns"
    );
    Ok(())
}

#[test]
fn configured_helpers_authenticate_every_image_command_and_preserve_precedence() -> Result {
    let object =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../e2e/testdata/bpf/xdp_pass.bpf.o");
    for bearer in [false, true] {
        for specific in [false, true] {
            let temp = setup()?;
            let root = temp.path();
            install_helper(root, "selected")?;
            let server = registry::Registry::new();
            configure(&server, bearer);
            let host = server.reference.split('/').next().ok_or("host")?;
            helper_config(root, host, specific)?;
            let before = std::fs::read(root.join("auth.json"))?;
            let trace = root.join("helper.trace");
            let output = helper_command(root)
                .arg("--trace-file")
                .arg(&trace)
                .env("RUST_LOG", "trace,ureq=trace,ureq_proto=trace")
                .args(["image", "build", &server.reference])
                .arg(&object)
                .output()?;
            assert!(!String::from_utf8_lossy(&output.stderr).contains("helper-stderr-secret"));
            success(output);
            assert_helper_reaped(root)?;
            let inspected: serde_json::Value = serde_json::from_slice(&success(
                helper_command(root)
                    .args(["image", "inspect", &server.reference])
                    .output()?,
            ))?;
            assert_eq!(inspected["programs"]["pass"], "xdp");
            assert_helper_reaped(root)?;
            assert!(
                String::from_utf8(success(
                    helper_command(root)
                        .args(["image", "verify", &server.reference])
                        .output()?
                ))?
                .contains("unsigned image accepted")
            );
            assert_helper_reaped(root)?;
            let output = helper_command(root)
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
            assert_helper_reaped(root)?;
            let calls = std::fs::read_to_string(root.join("helper.calls"))?;
            assert!(calls.lines().count() >= 4);
            assert!(
                calls
                    .lines()
                    .all(|line| line == format!("docker-credential-selected get {host}"))
            );
            let trace = std::fs::read_to_string(trace)?;
            for secret in [SECRET, &encoded(), "publish-token", "helper-stderr-secret"] {
                assert!(!trace.contains(secret));
                assert!(!String::from_utf8_lossy(&output.stderr).contains(secret));
            }
            assert_eq!(std::fs::read(root.join("auth.json"))?, before);
            assert!(!root.join("runtime").exists());
            assert!(!root.join("cache").exists());
        }
    }
    Ok(())
}

#[test]
fn helper_failures_empty_results_and_identity_tokens_on_basic_never_fall_back_or_leak() -> Result {
    let temp = setup()?;
    let root = temp.path();
    install_helper(root, "selected")?;
    let server = registry::Registry::new();
    configure(&server, false);
    let host = server.reference.split('/').next().ok_or("host")?;
    let file = root.join("auth.json");
    // Valid inline credentials and a later file must never rescue a helper.
    auth_file(root, host, "home/.config/containers/auth.json")?;
    for helper in ["missing", "selected"] {
        std::fs::write(
            &file,
            json!({"credHelpers":{host:helper}, "auths":{host:{"auth":encoded()}}}).to_string(),
        )?;
        for mode in ["fail", "malformed", "token", "absent", "empty"] {
            let output = helper_command(root)
                .env("HELPER_MODE", mode)
                .args(["image", "inspect", &server.reference])
                .output()?;
            assert!(!output.status.success());
            let stderr = String::from_utf8(output.stderr)?;
            for secret in [
                SECRET,
                "helper-stderr-secret",
                "helper-failure-secret",
                "helper-parser-secret",
                "helper-identity-secret",
            ] {
                assert!(!stderr.contains(secret), "{stderr}");
            }
            if helper == "selected" {
                assert_helper_reaped(root)?;
                if mode == "token" {
                    assert!(stderr.contains("Basic authentication requires username and password"));
                }
                if mode == "malformed" {
                    assert!(stderr.contains("invalid registry credential helper response"));
                }
            }
        }
    }
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

#[test]
fn explicit_credentials_unchallenged_requests_never_and_foreign_challenges_do_not_run_helpers()
-> Result {
    let temp = setup()?;
    let root = temp.path();
    install_helper(root, "selected")?;
    let server = registry::Registry::new();
    let host = server.reference.split('/').next().ok_or("host")?;
    helper_config(root, host, true)?;
    let output = helper_command(root)
        .args(["image", "inspect", &server.reference])
        .output()?;
    assert!(String::from_utf8_lossy(&output.stderr).contains("manifest not found"));
    let output = helper_command(root)
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
    let object =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../e2e/testdata/bpf/xdp_pass.bpf.o");
    success(
        helper_command(root)
            .env("BPFMAN_REGISTRY_AUTH", encoded())
            .args(["image", "build", &server.reference])
            .arg(object)
            .output()?,
    );
    success(
        helper_command(root)
            .args([
                "image",
                "inspect",
                &server.reference,
                "--registry-auth",
                &encoded(),
            ])
            .output()?,
    );
    assert!(
        !helper_command(root)
            .env("BPFMAN_REGISTRY_AUTH", STANDARD.encode("wrong:credentials"))
            .args(["image", "inspect", &server.reference])
            .output()?
            .status
            .success()
    );
    assert!(!root.join("helper.calls").exists());
    let receiver = registry::Registry::new();
    configure(&receiver, false);
    let receiver_host = receiver.reference.split('/').next().ok_or("host")?;
    let mut state = server.state.lock().expect("state");
    state.required_auth = None;
    state.redirects.insert(
        "GET /v2/test/manifests/latest".into(),
        format!("http://{receiver_host}/sink"),
    );
    drop(state);
    let output = helper_command(root)
        .args(["image", "inspect", &server.reference])
        .output()?;
    assert!(String::from_utf8_lossy(&output.stderr).contains("redirected registry response"));
    assert!(!root.join("helper.calls").exists());
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
fn sigint_and_sigterm_during_helper_lookup_reap_the_helper_before_cli_exit() -> Result {
    use std::{
        process::{Child, Stdio},
        time::{Duration, Instant},
    };
    struct Process(Child);
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for signal in ["INT", "TERM"] {
        let temp = setup()?;
        let root = temp.path();
        install_helper(root, "selected")?;
        let server = registry::Registry::new();
        configure(&server, false);
        let host = server.reference.split('/').next().ok_or("host")?;
        helper_config(root, host, true)?;
        let mut process = Process(
            helper_command(root)
                .env("HELPER_MODE", "hang")
                .args(["image", "inspect", &server.reference])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()?,
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::fs::read_to_string(root.join("helper.pid"))
            .unwrap_or_default()
            .is_empty()
        {
            assert!(Instant::now() < deadline, "helper did not start");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            Command::new("kill")
                .arg(format!("-{signal}"))
                .arg(process.0.id().to_string())
                .status()?
                .success()
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = process.0.try_wait()? {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "helper cancellation did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(status.code(), Some(if signal == "INT" { 130 } else { 143 }));
        let mut stderr = String::new();
        std::io::Read::read_to_string(&mut process.0.stderr.take().ok_or("stderr")?, &mut stderr)?;
        assert!(stderr.contains("cancelled"), "{stderr}");
        assert!(!stderr.contains("helper-stderr-secret"));
        assert_helper_reaped(root)?;
        assert!(!root.join("runtime").exists());
        assert!(!root.join("cache").exists());
    }
    Ok(())
}

#[test]
fn upload_abort_retains_helper_credentials_after_config_and_executable_change() -> Result {
    let object =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../e2e/testdata/bpf/xdp_pass.bpf.o");
    for (bearer, identity) in [(false, false), (true, false), (true, true)] {
        let temp = setup()?;
        let root = temp.path();
        install_helper(root, "selected")?;
        let server = registry::Registry::new();
        configure(&server, bearer);
        if identity {
            configure_identity(&server, "pull%2Cpush");
        }
        server
            .state
            .lock()
            .expect("state")
            .faults
            .insert("PUT /v2/test/blobs/uploads/*".into(), 503);
        let host = server.reference.split('/').next().ok_or("host")?;
        helper_config(root, host, true)?;
        let mut process = if identity {
            identity_command(root, true)
        } else {
            helper_command(root)
        };
        let output = process
            .env("HELPER_MODE", "poison")
            .args(["image", "build", &server.reference])
            .arg(&object)
            .output()?;
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(
            stderr.contains("complete OCI upload returned HTTP 503"),
            "{stderr}"
        );
        assert!(!stderr.contains("cleanup failed"), "{stderr}");
        assert!(!stderr.contains("secret"), "{stderr}");
        assert_helper_reaped(root)?;
        assert_eq!(
            std::fs::read_to_string(root.join("helper.calls"))?
                .lines()
                .count(),
            1
        );
        assert_eq!(
            std::fs::read_to_string(root.join("auth.json"))?,
            "changed-credential-file-secret"
        );
        let state = server.state.lock().expect("state");
        assert!(state.uploads.is_empty());
        assert!(state.manifests.is_empty());
        assert!(
            state
                .requests
                .iter()
                .any(|request| request.starts_with("DELETE "))
        );
    }
    Ok(())
}

#[test]
fn admitted_cache_hits_do_not_execute_a_subsequently_broken_helper() -> Result {
    let temp = setup()?;
    let root = temp.path();
    let executable = install_helper(root, "selected")?;
    let server = registry::Registry::new();
    configure(&server, false);
    let host = server.reference.split('/').next().ok_or("host")?;
    helper_config(root, host, true)?;
    let object =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../e2e/testdata/bpf/xdp_pass.bpf.o");
    success(
        helper_command(root)
            .args(["image", "build", &server.reference])
            .arg(object)
            .output()?,
    );
    // A regular-file runtime root rejects mutation after image/cache admission,
    // keeping this a rootless contract while proving a real admitted cache hit.
    std::fs::write(root.join("runtime"), "blocked runtime")?;
    let load = |policy: &str| {
        helper_command(root)
            .args([
                "program",
                "load",
                "image",
                &server.reference,
                "--programs",
                "xdp:pass",
                "--pull-policy",
                policy,
            ])
            .output()
    };
    let output = load("Always")?;
    assert!(!output.status.success());
    assert!(
        root.join("cache/images-v1").is_dir(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let before = std::fs::read(root.join("helper.calls"))?;
    let requests = server.state.lock().expect("state").requests.len();
    std::fs::write(executable, "#!/bin/sh\nexit 99\n")?;
    for policy in ["Never", "IfNotPresent"] {
        let output = load(policy)?;
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(!stderr.contains("credential helper"), "{stderr}");
        assert_eq!(std::fs::read(root.join("helper.calls"))?, before);
        assert_eq!(server.state.lock().expect("state").requests.len(), requests);
    }
    Ok(())
}

#[test]
fn identity_files_and_helpers_authenticate_all_commands_without_leaking_or_modifying_secrets()
-> Result {
    let object =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../e2e/testdata/bpf/xdp_pass.bpf.o");
    for helper in [false, true] {
        let temp = setup()?;
        let root = temp.path();
        let server = registry::Registry::new();
        let host = server.reference.split('/').next().ok_or("host")?;
        identity_config(root, host, helper)?;
        let before = std::fs::read(root.join("auth.json"))?;
        configure_identity(&server, "pull%2Cpush");
        let trace = root.join("identity.trace");
        let output = identity_command(root, helper)
            .env("RUST_LOG", "trace,ureq=trace,ureq_proto=trace")
            .arg("--trace-file")
            .arg(&trace)
            .args(["image", "build", &server.reference])
            .arg(&object)
            .output()?;
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        success(output);
        let trace = std::fs::read_to_string(&trace)?;
        for secret in [
            IDENTITY,
            "refresh-secret%2B",
            "publish-token",
            "helper-stderr-secret",
        ] {
            assert!(!trace.contains(secret), "trace leaked credentials");
            assert!(!stderr.contains(secret), "stderr leaked credentials");
        }
        configure_identity(&server, "pull");
        success(
            identity_command(root, helper)
                .args(["image", "inspect", &server.reference])
                .output()?,
        );
        assert!(
            String::from_utf8(success(
                identity_command(root, helper)
                    .args(["image", "verify", &server.reference])
                    .output()?
            ))?
            .contains("unsigned image accepted")
        );
        let output = identity_command(root, helper)
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
        assert_eq!(std::fs::read(root.join("auth.json"))?, before);
        assert!(!root.join("runtime").exists());
        assert!(!root.join("cache").exists());
        if helper {
            assert_helper_reaped(root)?;
        }
        let state = server.state.lock().expect("state");
        assert_eq!(state.token_forms.len(), 4);
        assert!(state.requests.iter().all(|r| !r.contains("refresh-secret")));
        assert!(state.authorizations.iter().all(|(_, auth)| {
            auth.as_deref()
                .is_none_or(|value| value == "Bearer publish-token")
        }));
        drop(state);
        // Valid explicit auth bypasses even a selected identity-token source;
        // wrong explicit auth never falls back to a refresh grant.
        configure(&server, false);
        for via_env in [false, true] {
            for correct in [false, true] {
                let mut process = identity_command(root, helper);
                process.args(["image", "inspect", &server.reference]);
                let auth = if correct {
                    encoded()
                } else {
                    STANDARD.encode("wrong:credentials")
                };
                if via_env {
                    process.env("BPFMAN_REGISTRY_AUTH", auth);
                } else {
                    process.args(["--registry-auth", &auth]);
                }
                assert_eq!(process.output()?.status.success(), correct);
            }
        }
        assert_eq!(server.state.lock().expect("state").token_forms.len(), 4);
        if !helper {
            // Files can retain username/password alongside an identity token,
            // as Go does, for a registry that issues a Basic challenge.
            success(
                identity_command(root, false)
                    .args(["image", "inspect", &server.reference])
                    .output()?,
            );
        }
    }
    Ok(())
}

#[test]
fn failed_identity_grants_do_not_use_inline_credentials_or_later_files() -> Result {
    for helper in [false, true] {
        let temp = setup()?;
        let root = temp.path();
        let server = registry::Registry::new();
        let host = server.reference.split('/').next().ok_or("host")?;
        identity_config(root, host, helper)?;
        auth_file(root, host, "home/.config/containers/auth.json")?;
        configure_identity(&server, "pull");
        server
            .state
            .lock()
            .expect("state")
            .faults
            .insert("POST /token".into(), 401);
        let output = identity_command(root, helper)
            .args(["image", "inspect", &server.reference])
            .output()?;
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr)?;
        assert!(stderr.contains("registry token request failed"), "{stderr}");
        assert!(!stderr.contains("secret"), "{stderr}");
        let state = server.state.lock().expect("state");
        assert_eq!(state.token_forms.len(), 1);
        assert!(state.authorizations.iter().all(|(_, auth)| auth.is_none()));
        assert!(state.uploads.is_empty());
        assert!(!root.join("runtime").exists());
        assert!(!root.join("cache").exists());
        if helper {
            assert_helper_reaped(root)?;
        }
    }
    Ok(())
}

#[test]
fn identity_cache_hits_skip_token_exchange_and_changed_credentials() -> Result {
    for helper in [false, true] {
        let temp = setup()?;
        let root = temp.path();
        let server = registry::Registry::new();
        let host = server.reference.split('/').next().ok_or("host")?;
        identity_config(root, host, helper)?;
        configure_identity(&server, "pull%2Cpush");
        let object =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../e2e/testdata/bpf/xdp_pass.bpf.o");
        success(
            identity_command(root, helper)
                .args(["image", "build", &server.reference])
                .arg(object)
                .output()?,
        );
        configure_identity(&server, "pull");
        std::fs::write(root.join("runtime"), "blocked runtime")?;
        let load = |policy: &str| {
            identity_command(root, helper)
                .args([
                    "program",
                    "load",
                    "image",
                    &server.reference,
                    "--programs",
                    "xdp:pass",
                    "--pull-policy",
                    policy,
                ])
                .output()
        };
        assert!(!load("Always")?.status.success());
        assert!(root.join("cache/images-v1").is_dir());
        let calls = if helper {
            Some(std::fs::read(root.join("helper.calls"))?)
        } else {
            None
        };
        let requests = server.state.lock().expect("state").requests.len();
        std::fs::write(root.join("auth.json"), "broken-identity-file-secret")?;
        if helper {
            std::fs::write(
                root.join("helpers/docker-credential-selected"),
                "#!/bin/sh\nexit 99\n",
            )?;
        }
        for policy in ["Never", "IfNotPresent"] {
            assert!(!load(policy)?.status.success());
            assert_eq!(server.state.lock().expect("state").requests.len(), requests);
            if let Some(calls) = &calls {
                assert_eq!(&std::fs::read(root.join("helper.calls"))?, calls);
            }
        }
        for file in std::fs::read_dir(root.join("cache/images-v1"))? {
            let bytes = std::fs::read(file?.path())?;
            let text = String::from_utf8_lossy(&bytes);
            for secret in [IDENTITY, "publish-token", "refresh-secret%2B"] {
                assert!(!text.contains(secret));
            }
        }
    }
    Ok(())
}

#[test]
fn sigint_and_sigterm_during_refresh_exchange_stop_before_registry_mutation() -> Result {
    use std::{
        io::{BufRead, BufReader},
        process::{Child, Stdio},
        sync::mpsc::sync_channel,
        time::{Duration, Instant},
    };
    struct Process(Child);
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    for helper in [false, true] {
        for signal in ["INT", "TERM"] {
            let temp = setup()?;
            let root = temp.path();
            let server = registry::Registry::new();
            let host = server.reference.split('/').next().ok_or("host")?;
            identity_config(root, host, helper)?;
            configure_identity(&server, "pull%2Cpush");
            let (waiting, paused) = sync_channel(0);
            let (resume, continued) = sync_channel(0);
            server.state.lock().expect("state").pause =
                Some(("POST /token".into(), waiting, continued));
            let object = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../e2e/testdata/bpf/xdp_pass.bpf.o");
            let mut process = Process(
                identity_command(root, helper)
                    .env("RUST_LOG", "debug")
                    .args(["image", "build", &server.reference])
                    .arg(object)
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()?,
            );
            let (notified, notification) = sync_channel(1);
            let stderr = process.0.stderr.take().ok_or("stderr")?;
            let reader = std::thread::spawn(move || {
                let mut output = String::new();
                for line in BufReader::new(stderr).lines() {
                    let line = line.expect("stderr line");
                    if line.contains("shutdown requested") {
                        let _ = notified.send(());
                    }
                    output.push_str(&line);
                    output.push('\n');
                }
                output
            });
            paused.recv_timeout(Duration::from_secs(5))?;
            assert!(
                Command::new("kill")
                    .arg(format!("-{signal}"))
                    .arg(process.0.id().to_string())
                    .status()?
                    .success()
            );
            // Synchronize with the CLI signal thread before releasing the read.
            notification.recv_timeout(Duration::from_secs(5))?;
            resume.send(())?;
            let deadline = Instant::now() + Duration::from_secs(5);
            let status = loop {
                if let Some(status) = process.0.try_wait()? {
                    break status;
                }
                assert!(
                    Instant::now() < deadline,
                    "refresh cancellation did not finish"
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            assert_eq!(status.code(), Some(if signal == "INT" { 130 } else { 143 }));
            let stderr = reader.join().expect("stderr reader");
            assert!(stderr.contains("cancelled"), "{stderr}");
            assert!(!stderr.contains("secret"), "{stderr}");
            if helper {
                assert_helper_reaped(root)?;
            }
            let state = server.state.lock().expect("state");
            assert!(state.uploads.is_empty());
            assert!(state.manifests.is_empty());
            assert_eq!(state.token_forms.len(), 1);
            assert!(
                state
                    .requests
                    .iter()
                    .all(|r| r.starts_with("HEAD ") || r == "POST /token")
            );
            assert!(!root.join("runtime").exists());
            assert!(!root.join("cache").exists());
        }
    }
    Ok(())
}
