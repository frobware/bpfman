//! Unchanged Go daemon-launcher suite against the production Rust executable.
use super::support::{repository, rust};
use std::{fs, process::Command};

pub(super) fn exercise() {
    // The stock launcher removes its tmp root without unmounting bpffs. Scope
    // those mounts to its own namespace, then remove its owned temporary parent
    // after the daemon and launcher have exited. It still runs beside other cases.
    let temporary = tempfile::tempdir().expect("launcher fixture parent");
    let output = Command::new("timeout")
        .args([
            "240s",
            "unshare",
            "--mount",
            "--propagation",
            "private",
            "--",
        ])
        .arg(repository().join("bin/e2e-grpc.test"))
        .env("TMPDIR", temporary.path())
        .args(["-test.v", "-test.count=1"])
        .env("BPFMAN_BIN", rust())
        .env("BPFMAN_E2E_BYTECODE_DIR", repository().join("e2e"))
        .env_remove("BPFMAN_GRPC_GOROUTINES")
        .env_remove("BPFMAN_GRPC_ITERATIONS")
        .env_remove("BPFMAN_RUNTIME_DIR")
        .env_remove("BPFMAN_SOCKET_PATH")
        .env_remove("BPFMAN_CONFIG")
        .output()
        .expect("unchanged gRPC test launcher");
    assert!(
        output.status.success(),
        "Go gRPC launcher: {}\n{}\ndaemon: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        fs::read_to_string("/tmp/bpfman-grpc-daemon.log").unwrap_or_default()
    );
    for line in String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with("--- PASS:") || line.contains("total      "))
    {
        eprintln!("{line}");
    }
}

struct Daemon(std::process::Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) fn production() {
    use super::support::{Context, fixture};
    use std::{
        os::unix::net::UnixStream,
        process::Stdio,
        time::{Duration, Instant},
    };
    let c = Context::new();
    let parent = c.layout.root().parent().expect("parent");
    let socket = parent.join("production.sock");
    let config = parent.join("daemon.toml");
    let network = super::xdp_attach::Interface::new();
    fs::write(
        &config,
        format!(
            "[signing]\nverify_enabled = false\n[interfaces.{}]\nxdp_mode = 'skb'\n",
            network.name().as_str()
        ),
    )
    .expect("config");
    let registry = crate::oci_registry::Registry::new(
        fs::read(fixture("xdp_pass.bpf.o")).expect("object"),
        false,
    );
    registry.responses.lock().expect("registry routes").insert(
        format!(
            "/v2/test/manifests/{}.sig",
            registry.digest.replace(':', "-")
        ),
        (404, br#"{"errors":[{"code":"MANIFEST_UNKNOWN"}]}"#.to_vec()),
    );
    let libc = fs::read_to_string("/proc/self/maps")
        .expect("maps")
        .lines()
        .find_map(|line| {
            line.split_whitespace()
                .last()
                .filter(|s| s.ends_with("/libc.so.6"))
        })
        .expect("libc")
        .to_owned();
    let start = || {
        let mut daemon = Daemon(
            Command::new(rust())
                .arg("serve")
                .arg("--runtime-dir")
                .arg(c.layout.root())
                .arg("--config")
                .arg(&config)
                .arg("--socket-path")
                .arg(&socket)
                .arg("--image-cache-dir")
                .arg(parent.join("cache"))
                .arg("--lock-timeout=100ms")
                .stdout(Stdio::null())
                .spawn()
                .expect("production daemon"),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(&socket).is_err() {
            assert!(
                daemon.0.try_wait().expect("status").is_none(),
                "daemon startup"
            );
            assert!(Instant::now() < deadline, "daemon readiness");
            std::thread::sleep(Duration::from_millis(5));
        }
        daemon
    };
    let stop = |daemon: &mut Daemon| {
        assert!(
            Command::new("kill")
                .arg("-TERM")
                .arg(daemon.0.id().to_string())
                .status()
                .expect("TERM")
                .success()
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = daemon.0.try_wait().expect("status") {
                assert!(status.success(), "production shutdown: {status}");
                break;
            }
            assert!(Instant::now() < deadline, "daemon joined shutdown");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!socket.exists(), "published socket removed");
    };
    let client = |lane: &str, daemon: &Daemon, success: bool| {
        let output = Command::new("timeout")
            .arg("60s")
            .arg(repository().join("bin/grpc-contract"))
            .arg("-socket")
            .arg(&socket)
            .arg("-objects")
            .arg(fixture("."))
            .arg("-runtime")
            .arg(c.layout.root())
            .arg("-server-pid")
            .arg(daemon.0.id().to_string())
            .arg("-image")
            .arg(&registry.reference)
            .arg("-interface")
            .arg(network.name().as_str())
            .arg("-uprobe-target")
            .arg(&libc)
            .arg("-lane")
            .arg(lane)
            .output()
            .expect("shared client");
        assert_eq!(
            output.status.success(),
            success,
            "production {lane}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    let mut daemon = start();
    for lane in [
        "lifecycle",
        "families",
        "cancellation",
        "image",
        "persist-create",
    ] {
        client(lane, &daemon, true);
    }
    stop(&mut daemon);
    let mut reopened = start();
    client("persist-read", &reopened, true);
    stop(&mut reopened);
    // Configuration must reach the verifier for an image admitted by an earlier disabled policy.
    fs::write(
        &config,
        "[signing]\nverify_enabled = true\nallow_unsigned = false\ntrust_source = 'bundled'\n",
    )
    .expect("strict signing config");
    let mut strict = start();
    let rejected = client("image", &strict, false);
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("signature"),
        "unsigned evidence rejected by configured policy: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    stop(&mut strict);
    // Mode 0700 alone does not confer exclusive staging authority if another
    // account owns the directory. Refuse it before binding or opening a store.
    let foreign = parent.join(".bpfman-foreign.sock");
    fs::create_dir(&foreign).expect("foreign staging directory");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&foreign, fs::Permissions::from_mode(0o700)).expect("mode");
    assert!(
        Command::new("chown")
            .arg("65534")
            .arg(&foreign)
            .status()
            .expect("foreign owner")
            .success()
    );
    let refused = Command::new(rust())
        .arg("serve")
        .arg("--runtime-dir")
        .arg(parent.join("unused-runtime"))
        .arg("--config")
        .arg(&config)
        .arg("--socket-path")
        .arg(parent.join("foreign.sock"))
        .output()
        .expect("foreign staging refusal");
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("owned by the daemon user"));
    assert!(!parent.join("unused-runtime").exists());
    assert_eq!(fs::read_dir(&foreign).expect("foreign contents").count(), 0);
    c.no_artifacts();
}
