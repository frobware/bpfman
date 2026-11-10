//! CSI startup remains independent of the temporary management service.
#![allow(clippy::expect_used)]

use std::{
    fs,
    os::unix::{
        fs::MetadataExt,
        net::{UnixListener, UnixStream},
    },
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn csi_runs_without_management_socket_cache_or_signing_policy() {
    let temporary = tempfile::tempdir().expect("fixture");
    let runtime = temporary.path().join("runtime");
    let socket = runtime.join("csi/csi.sock");
    let management = temporary.path().join("management.sock");
    let listener = UnixListener::bind(&management).expect("occupied management socket");
    let inode = fs::metadata(&management)
        .expect("management metadata")
        .ino();
    let cache = temporary.path().join("unusable-cache");
    fs::write(&cache, b"operator data").expect("cache blocker");
    let read_config = temporary.path().join("cli.toml");
    fs::write(&read_config, "[signing]\nverify_enabled = false\n").expect("CLI config");
    let config = temporary.path().join("bpfman.toml");
    fs::write(&config, "[signing]\nverify_enabled = true\n[[signing.trusted_identities]]\ncertificate_identity_regexp = \"[\"\ncertificate_oidc_issuer = \"https://issuer.invalid\"\n").expect("unused management policy");
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");

    for signal in ["TERM", "INT"] {
        let mut daemon = Daemon(
            Command::new(env!("CARGO_BIN_EXE_bpfman"))
                .arg("csi")
                .arg("--runtime-dir")
                .arg(&runtime)
                .arg("--config")
                .arg(&config)
                .arg("--image-cache-dir")
                .arg(&cache)
                .arg("--sigstore-cache-dir")
                .arg(&cache)
                .env("BPFMAN_SOCKET_PATH", &management)
                .env_remove("BPFMAN_RUNTIME_DIR")
                .env_remove("BPFMAN_CONFIG")
                .env_remove("BPFMAN_LOCK_TIMEOUT")
                .stdout(Stdio::null())
                .spawn()
                .expect("CSI-only daemon"),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(&socket).is_err() {
            assert!(
                daemon.0.try_wait().expect("status").is_none(),
                "CSI exited before readiness"
            );
            assert!(Instant::now() < deadline, "CSI readiness deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            fs::metadata(&socket).expect("CSI metadata").mode() & 0o777,
            0o660
        );
        assert!(runtime.join("db/store.db").exists());
        let identity = Command::new(repository.join("bin/csi-contract"))
            .arg("-csi-socket")
            .arg(&socket)
            .args(["-lane", "identity"])
            .output()
            .expect("stock CSI client");
        assert!(
            identity.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&identity.stdout),
            String::from_utf8_lossy(&identity.stderr)
        );
        let listed = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .arg("--runtime-dir")
            .arg(&runtime)
            .arg("--config")
            .arg(&read_config)
            .args(["program", "list", "--output=json", "--lock-timeout=100ms"])
            .env_remove("BPFMAN_CONFIG")
            .env_remove("BPFMAN_RUNTIME_DIR")
            .output()
            .expect("independent CLI");
        assert!(
            listed.status.success(),
            "{}",
            String::from_utf8_lossy(&listed.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&listed.stdout).expect("inventory")["programs"],
            serde_json::json!([])
        );
        assert!(
            Command::new("kill")
                .arg(format!("-{signal}"))
                .arg(daemon.0.id().to_string())
                .status()
                .expect("signal")
                .success()
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = daemon.0.try_wait().expect("exit") {
                assert!(status.success(), "CSI graceful shutdown: {status}");
                break;
            }
            assert!(Instant::now() < deadline, "CSI shutdown deadline");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!socket.exists(), "owned CSI socket removed");
        assert_eq!(
            fs::metadata(&management)
                .expect("management socket retained")
                .ino(),
            inode
        );
        assert_eq!(
            fs::read(&cache).expect("cache blocker retained"),
            b"operator data"
        );
    }
    drop(listener);
}

#[cfg(not(feature = "management-api"))]
#[test]
fn build_without_management_api_has_no_serve_command() {
    let result = Command::new(env!("CARGO_BIN_EXE_bpfman"))
        .args(["serve", "--help"])
        .output()
        .expect("management command");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("unrecognized subcommand"));
    let result = Command::new(env!("CARGO_BIN_EXE_bpfman"))
        .args(["csi", "--help"])
        .output()
        .expect("CSI command");
    assert!(result.status.success());
}
