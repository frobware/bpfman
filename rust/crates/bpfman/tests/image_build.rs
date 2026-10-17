//! Image commands validate every ELF before network access and never open the store.
#![allow(clippy::expect_used)]
#[path = "../../../tests/support/publish_registry.rs"]
mod registry;
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../e2e/testdata/bpf/xdp_pass.bpf.o")
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman"));
    command
        .arg("--runtime-dir")
        .arg(root.join("runtime"))
        .arg("--image-cache-dir")
        .arg(root.join("cache"))
        .arg("--config")
        .arg(root.join("missing.toml"))
        .env_remove("BPFMAN_CONFIG")
        .env_remove("BPFMAN_RUNTIME_DIR")
        .env_remove("BPFMAN_REGISTRY_AUTH")
        .env_remove("SOURCE_DATE_EPOCH");
    command
}

fn success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn build_contract_and_multi_arch_publication() -> Result {
    let temp = tempfile::tempdir()?;
    let path = fixture();
    let path = path.to_str().expect("fixture path");
    let json: Value = serde_json::from_slice(&success(
        command(temp.path())
            .args(["image", "generate-build-args", path, "-o", "json"])
            .output()?,
    ))?;
    assert_eq!(json["platforms"], serde_json::json!([]));
    assert_eq!(json["programs"]["pass"], "xdp");
    assert_eq!(json["maps"]["xdp_pass_stats_map"], "per_cpu_array");
    assert_eq!(json["build_args"]["BYTECODE_FILE"], path);
    let foreign = fixture()
        .parent()
        .expect("fixture directory")
        .join("xdp_pass_be.bpf.o");
    let foreign = foreign.to_str().expect("foreign path");
    let registry = registry::Registry::new();
    let mut previous = None;
    for _ in 0..2 {
        let output = success(
            command(temp.path())
                .args([
                    "image",
                    "build",
                    &registry.reference,
                    &format!("linux/amd64={path}"),
                    &format!("linux/arm64={path}"),
                    &format!("linux/s390x={foreign}"),
                ])
                .output()?,
        );
        assert!(output.starts_with(b"published: "));
        if let Some(previous) = previous {
            assert_eq!(output, previous);
        }
        previous = Some(output);
    }
    let inspected: Value = serde_json::from_slice(&success(
        command(temp.path())
            .args(["image", "inspect", &registry.reference])
            .output()?,
    ))?;
    assert_eq!(
        inspected["manifests"].as_array().expect("manifests").len(),
        3
    );
    let go = std::env::var_os("BPFMAN_GO_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bin/bpfman"));
    let go_inspected: Value = serde_json::from_slice(&success(
        Command::new(go)
            .arg("--config")
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../../e2e/config/no-signature-verification.toml"),
            )
            .args(["image", "inspect", &registry.reference])
            .env_remove("BPFMAN_REGISTRY_AUTH")
            .output()?,
    ))?;
    assert_eq!(
        go_inspected, inspected,
        "Go consumes Rust's published index"
    );
    assert_eq!(inspected["manifests"][0]["platform"], "linux/amd64");
    assert_eq!(inspected["manifests"][1]["platform"], "linux/arm64");
    assert_eq!(inspected["manifests"][2]["platform"], "linux/s390x");
    assert!(!temp.path().join("runtime").exists());
    assert!(!temp.path().join("cache").exists());
    assert!(
        !registry
            .cancelled
            .load(std::sync::atomic::Ordering::Relaxed)
    );
    Ok(())
}

#[test]
fn invalid_inputs_and_timestamps_fail_before_registry_access() -> Result {
    let temp = tempfile::tempdir()?;
    let registry = registry::Registry::new();
    let path = fixture().to_str().expect("fixture path").to_owned();
    let malformed = temp.path().join("malformed.o");
    std::fs::write(&malformed, b"not ELF")?;
    for inputs in [
        vec![],
        vec![path.clone(), path.clone()],
        vec![format!("linux/amd64={path}"), path.clone()],
        vec![format!("linux/amd64={path}"), format!("linux/amd64={path}")],
        vec![format!("linux/s390x={path}")],
        vec!["linux/future=x.o".into()],
        vec!["linux/amd64=".into()],
        vec![malformed.to_str().expect("path").into()],
        vec![
            format!("linux/amd64={path}"),
            "linux/arm64=/no/such/bytecode.o".into(),
        ],
    ] {
        let output = command(temp.path())
            .args(["image", "build", &registry.reference])
            .args(inputs)
            .output()?;
        assert!(!output.status.success());
        assert!(registry.state.lock().expect("state").requests.is_empty());
    }
    for epoch in ["-1", "not-a-time", "18446744073709551615"] {
        let output = command(temp.path())
            .args(["image", "build", &registry.reference, &path])
            .env("SOURCE_DATE_EPOCH", epoch)
            .output()?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("SOURCE_DATE_EPOCH"));
        assert!(registry.state.lock().expect("state").requests.is_empty());
    }
    assert!(!temp.path().join("runtime").exists());
    assert!(!temp.path().join("cache").exists());
    Ok(())
}

#[test]
fn signals_abort_acquired_uploads_and_reap_the_cli() -> Result {
    use std::{
        io::{BufRead, BufReader},
        process::{Child, Stdio},
        sync::mpsc,
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
        let temp = tempfile::tempdir()?;
        let server = registry::Registry::new();
        let (waiting, wait) = mpsc::sync_channel(1);
        let (resume, resumed) = mpsc::sync_channel(1);
        server.state.lock().expect("state").pause =
            Some(("POST /v2/test/blobs/uploads/".into(), waiting, resumed));
        let mut child = Process(
            command(temp.path())
                .args(["image", "build", &server.reference])
                .arg(fixture())
                .env("RUST_LOG", "debug")
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()?,
        );
        let (notified, notification) = mpsc::sync_channel(1);
        let stderr = child.0.stderr.take().expect("stderr");
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
        wait.recv_timeout(Duration::from_secs(5))?;
        assert!(
            Command::new("kill")
                .arg(format!("-{signal}"))
                .arg(child.0.id().to_string())
                .status()?
                .success()
        );
        // Wait for the CLI signal thread to set its cancellation flag before
        // releasing the already-acquired session's response to the HTTP caller.
        notification.recv_timeout(Duration::from_secs(5))?;
        resume.send(())?;
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.0.try_wait()? {
                break status;
            }
            assert!(Instant::now() < deadline, "CLI did not finish cancellation");
            std::thread::sleep(Duration::from_millis(5));
        };
        let stderr = reader.join().expect("stderr reader");
        assert_eq!(
            status.code(),
            Some(if signal == "INT" { 130 } else { 143 }),
            "{stderr}"
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
        assert!(!temp.path().join("runtime").exists());
        assert!(!temp.path().join("cache").exists());
    }
    Ok(())
}

#[test]
fn canonical_labels_match_go_across_attachment_families() -> Result {
    let temp = tempfile::tempdir()?;
    let go = std::env::var_os("BPFMAN_GO_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bin/bpfman"));
    let fixtures = fixture().parent().expect("fixture directory").to_owned();
    for name in [
        "xdp_pass.bpf.o",
        "tracepoint_counter.bpf.o",
        "kprobe_counter.bpf.o",
        "uprobe_counter.bpf.o",
        "fentry_counter.bpf.o",
        "fexit_kmod_exact.bpf.o",
        "tc_counter_pinned.bpf.o",
        "shared_map_contract.bpf.o",
    ] {
        let path = fixtures.join(name);
        let path = path.to_str().expect("fixture path");
        let rust: Value = serde_json::from_slice(&success(
            command(temp.path())
                .args(["image", "generate-build-args", path, "-o", "json"])
                .output()?,
        ))?;
        let go: Value = serde_json::from_slice(&success(
            Command::new(&go)
                .arg("--config")
                .arg(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../../e2e/config/no-signature-verification.toml"),
                )
                .args(["image", "generate-build-args", path, "-o", "json"])
                .output()?,
        ))?;
        assert_eq!(rust, go, "{name}");
    }
    Ok(())
}

#[test]
fn foreign_endian_contract_matches_go() -> Result {
    let temp = tempfile::tempdir()?;
    let go = std::env::var_os("BPFMAN_GO_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../bin/bpfman"));
    let foreign = fixture()
        .parent()
        .expect("fixture directory")
        .join("xdp_pass_be.bpf.o");
    for arch in ["mips", "mips64", "ppc64", "s390x"] {
        let input = format!("linux/{arch}={}", foreign.display());
        let rust: Value = serde_json::from_slice(&success(
            command(temp.path())
                .args(["image", "generate-build-args", &input, "-o", "json"])
                .output()?,
        ))?;
        let go: Value = serde_json::from_slice(&success(
            Command::new(&go)
                .arg("--config")
                .arg(
                    Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("../../../e2e/config/no-signature-verification.toml"),
                )
                .args(["image", "generate-build-args", &input, "-o", "json"])
                .output()?,
        ))?;
        assert_eq!(rust, go, "{arch}");
    }
    Ok(())
}
