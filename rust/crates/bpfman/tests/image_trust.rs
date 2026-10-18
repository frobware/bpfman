//! Default TUF selection and real cancellation before registry/runtime effects.
#![allow(clippy::expect_used)]
#[path = "../../../tests/support/oci.rs"]
mod fixture;
#[path = "../../../tests/support/signed_image.rs"]
mod signed_image;

use bpfman_fs::{RuntimeDirectory, RuntimeLayout};
use bpfman_lock::AcquireOptions;
use std::{
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, Command, Stdio},
    sync::{atomic::Ordering, mpsc},
    time::{Duration, Instant},
};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;
const TRUSTED_ROOT: &str = include_str!(
    "../../../../vendor/github.com/sigstore/sigstore/pkg/tuf/repository/targets/trusted_root.json"
);

fn command(root: &Path, config: &Path, trust: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman"));
    command.env(
        "BPFMAN_CONFIG",
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/cli.toml"),
    );
    command
        .arg("--config")
        .arg(config)
        .arg("--runtime-dir")
        .arg(root.join("runtime"))
        .arg("--image-cache-dir")
        .arg(root.join("images"))
        .arg("--sigstore-cache-dir")
        .arg(trust)
        .env_remove("BPFMAN_REGISTRY_AUTH")
        .env_remove("REGISTRY_AUTH_FILE")
        .env_remove("HOME")
        .env_remove("XDG_RUNTIME_DIR")
        .env_remove("DOCKER_CONFIG");
    command
}

#[test]
fn explicit_root_overrides_tuf_and_bundled_and_rejects_untrusted_signatures() -> Result {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("trusted_root.json");
    std::fs::write(&root, TRUSTED_ROOT)?;
    let link = temp.path().join("root.json");
    std::os::unix::fs::symlink(&root, &link)?;
    let config = temp.path().join("config.toml");
    let trust = temp.path().join("trust");
    std::fs::write(&trust, "foreign")?;
    let server = signed_image::registry();
    for source in ["", "trust_source = 'tuf'\n", "trust_source = 'bundled'\n"] {
        std::fs::write(
            &config,
            format!("[signing]\n{source}trusted_root = '{}'\n", link.display()),
        )?;
        let output = command(temp.path(), &config, &trust)
            .args([
                "image",
                "verify",
                &server.reference,
                "--certificate-identity",
                signed_image::SUBJECT,
                "--certificate-oidc-issuer",
                signed_image::ISSUER,
            ])
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8(output.stdout)?.contains("signature verified"));
    }
    // Keep a structurally valid root but remove the CA that issued the fixture.
    // Retaining the older CA must never cause fallback to bundled production.
    let mut untrusted: serde_json::Value = serde_json::from_str(TRUSTED_ROOT)?;
    untrusted["certificateAuthorities"]
        .as_array_mut()
        .expect("CAs")
        .truncate(1);
    std::fs::write(&root, serde_json::to_vec(&untrusted)?)?;
    let output = command(temp.path(), &config, &trust)
        .args(["image", "verify", &server.reference, "--allow-unsigned"])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains("signature verification failed"));
    assert!(!temp.path().join("runtime").exists());
    assert!(!temp.path().join("images").exists());
    assert_eq!(std::fs::read_to_string(&trust)?, "foreign");
    Ok(())
}

#[test]
fn invalid_selected_roots_fail_before_registry_and_disabled_load_ignores_them() -> Result {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("root.json");
    let config = temp.path().join("config.toml");
    let trust = temp.path().join("trust");
    std::fs::write(
        &config,
        format!("[signing]\ntrusted_root = '{}'\n", root.display()),
    )?;
    let server = signed_image::registry();
    // Missing, directory and FIFO inputs must fail immediately, with no TUF or
    // registry fallback. No writer is opened for this operator-owned file.
    for path in [
        root.clone(),
        temp.path().join("directory"),
        temp.path().join("fifo"),
    ] {
        if path.ends_with("directory") {
            std::fs::create_dir(&path)?;
        }
        if path.ends_with("fifo") {
            rustix::fs::mknodat(
                rustix::fs::CWD,
                &path,
                rustix::fs::FileType::Fifo,
                rustix::fs::Mode::RUSR,
                0,
            )?;
        }
        std::fs::write(
            &config,
            format!("[signing]\ntrusted_root = '{}'\n", path.display()),
        )?;
        let output = command(temp.path(), &config, &trust)
            .args(["image", "verify", &server.reference])
            .output()?;
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr)?.contains("Sigstore trust root"));
    }
    std::fs::write(
        &config,
        format!("[signing]\ntrusted_root = '{}'\n", root.display()),
    )?;
    for bytes in [
        b"operator-private-input invalid JSON".to_vec(),
        vec![0xff],
        vec![b' '; 1024 * 1024 + 1],
    ] {
        std::fs::write(&root, &bytes)?;
        for load in [false, true] {
            let mut process = command(temp.path(), &config, &trust);
            if load {
                process.args([
                    "program",
                    "load",
                    "image",
                    &server.reference,
                    "--programs",
                    "xdp:pass",
                ]);
            } else {
                process.args(["image", "verify", &server.reference]);
            }
            let output = process.output()?;
            assert!(!output.status.success());
            let stderr = String::from_utf8(output.stderr)?;
            assert!(stderr.contains("Sigstore trust root"), "{stderr}");
            assert!(!stderr.contains("operator-private-input"));
        }
        assert_eq!(std::fs::read(&root)?, bytes);
    }
    assert_eq!(server.requests.load(Ordering::Relaxed), 0);
    std::fs::write(
        &config,
        format!(
            "[signing]\nverify_enabled = false\ntrusted_root = '{}'\n",
            root.display()
        ),
    )?;
    // Inspection traverses every index child, including attestation metadata;
    // use the complete ordinary metadata fixture for this independent operation.
    let inspection = fixture::Registry::new(vec![], false);
    let output = command(temp.path(), &config, &trust)
        .args(["image", "inspect", &inspection.reference])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // A runtime file stops the command after verified cache admission, without
    // requiring kernel privilege or creating a store.
    std::fs::write(temp.path().join("runtime"), "foreign")?;
    let output = command(temp.path(), &config, &trust)
        .args([
            "program",
            "load",
            "image",
            &server.reference,
            "--programs",
            "xdp:pass",
        ])
        .output()?;
    assert!(!output.status.success());
    assert!(temp.path().join("images/images-v1").is_dir());
    assert!(!String::from_utf8(output.stderr)?.contains("Sigstore trust root"));
    let before = server.requests.load(Ordering::Relaxed);
    let output = command(temp.path(), &config, &trust)
        .args(["image", "verify", &server.reference])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains("Sigstore trust root"));
    assert_eq!(server.requests.load(Ordering::Relaxed), before);
    assert!(!trust.exists());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("runtime"))?,
        "foreign"
    );
    Ok(())
}

#[test]
fn cli_reopens_root_file_and_requires_cache_revalidation_for_changed_bytes() -> Result {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("root.json");
    std::fs::write(&root, TRUSTED_ROOT)?;
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[signing]\nallow_unsigned = false\ntrusted_root = '{}'\n",
            root.display()
        ),
    )?;
    let trust = temp.path().join("trust");
    std::fs::write(temp.path().join("runtime"), "foreign")?;
    let server = signed_image::registry();
    let load = |mode: &str| {
        command(temp.path(), &config, &trust)
            .args([
                "program",
                "load",
                "image",
                &server.reference,
                "--programs",
                "xdp:pass",
                "--pull-policy",
                mode,
            ])
            .output()
    };
    let initial = load("Always")?;
    assert!(!initial.status.success());
    let stderr = String::from_utf8(initial.stderr)?;
    assert!(temp.path().join("images/images-v1").is_dir(), "{stderr}");
    assert!(
        !stderr.contains("signature verification failed"),
        "{stderr}"
    );
    let before = server.requests.load(Ordering::Relaxed);
    let cached = load("Never")?;
    assert!(!cached.status.success());
    assert_eq!(server.requests.load(Ordering::Relaxed), before);
    assert!(!String::from_utf8(cached.stderr)?.contains("different signing policy"));
    std::fs::write(&root, format!("{TRUSTED_ROOT}\n"))?;
    let changed = load("Never")?;
    assert!(!changed.status.success());
    assert!(String::from_utf8(changed.stderr)?.contains("different signing policy"));
    assert_eq!(server.requests.load(Ordering::Relaxed), before);
    let revalidated = load("IfNotPresent")?;
    assert!(!revalidated.status.success());
    assert!(!String::from_utf8(revalidated.stderr)?.contains("signature verification failed"));
    assert!(server.requests.load(Ordering::Relaxed) > before);
    let before = server.requests.load(Ordering::Relaxed);
    let reopened = load("Never")?;
    assert!(!reopened.status.success());
    assert!(!String::from_utf8(reopened.stderr)?.contains("different signing policy"));
    assert_eq!(server.requests.load(Ordering::Relaxed), before);
    std::fs::write(&root, "invalid")?;
    let invalid = load("IfNotPresent")?;
    assert!(!invalid.status.success());
    assert!(String::from_utf8(invalid.stderr)?.contains("Sigstore trust root"));
    assert_eq!(server.requests.load(Ordering::Relaxed), before);
    assert!(!trust.exists());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("runtime"))?,
        "foreign"
    );
    Ok(())
}

#[test]
fn default_tuf_fails_before_registry_and_bundled_mode_is_explicit() -> Result {
    let temp = tempfile::tempdir()?;
    let config = temp.path().join("config.toml");
    std::fs::write(&config, "")?;
    let foreign = temp.path().join("foreign");
    std::fs::create_dir(&foreign)?;
    let trust = temp.path().join("trust");
    std::os::unix::fs::symlink(&foreign, &trust)?;
    let server = fixture::Registry::new(vec![], false);
    for load in [false, true] {
        let mut process = command(temp.path(), &config, &trust);
        if load {
            process.args([
                "program",
                "load",
                "image",
                &server.reference,
                "--programs",
                "xdp:pass",
            ]);
        } else {
            process.args(["image", "verify", &server.reference]);
        }
        let output = process.output()?;
        assert!(!output.status.success());
        assert!(String::from_utf8(output.stderr)?.contains("Sigstore TUF cache"));
    }
    assert_eq!(server.requests.load(Ordering::Relaxed), 0);
    std::fs::write(&config, "[signing]\ntrust_source = 'bundled'\n")?;
    server.responses.lock().expect("routes").insert(
        format!("/v2/test/manifests/{}.sig", server.digest.replace(':', "-")),
        (
            404,
            serde_json::to_vec(&serde_json::json!({"errors":[{"code":"MANIFEST_UNKNOWN"}]}))?,
        ),
    );
    let output = command(temp.path(), &config, &trust)
        .args(["image", "verify", &server.reference])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8(output.stdout)?.contains("unsigned image accepted"));
    assert!(server.requests.load(Ordering::Relaxed) > 0);
    assert!(!server.bearer.load(Ordering::Relaxed));
    assert_eq!(std::fs::read_dir(&foreign)?.count(), 0);
    assert!(!temp.path().join("runtime").exists());
    assert!(!temp.path().join("images").exists());
    Ok(())
}

#[test]
fn disabled_load_and_inspection_do_not_access_trust_but_verify_forces_it_on() -> Result {
    let temp = tempfile::tempdir()?;
    let config = temp.path().join("config.toml");
    std::fs::write(&config, "[signing]\nverify_enabled = false\n")?;
    let trust = temp.path().join("trust");
    // An invalid regular-file root would fail any trust access.
    std::fs::write(&trust, b"foreign")?;
    let server = fixture::Registry::new(vec![], false);
    let output = command(temp.path(), &config, &trust)
        .args(["image", "inspect", &server.reference])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = command(temp.path(), &config, &trust)
        .args([
            "program",
            "load",
            "image",
            &server.reference,
            "--programs",
            "xdp:pass",
        ])
        .output()?;
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(
        stderr.contains("no BPF bytecode (.o file) found"),
        "{stderr}"
    );
    let requests = server.requests.load(Ordering::Relaxed);
    let output = command(temp.path(), &config, &trust)
        .args(["image", "verify", &server.reference])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains("Sigstore TUF cache"));
    assert_eq!(server.requests.load(Ordering::Relaxed), requests);
    assert_eq!(std::fs::read(trust)?, b"foreign");
    assert!(!temp.path().join("runtime").exists());
    assert!(!temp.path().join("images").exists());
    Ok(())
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn signals_cancel_tuf_writer_waits_and_reap_children_without_runtime_state() -> Result {
    let server = fixture::Registry::new(vec![], false);
    for signal in ["INT", "TERM"] {
        for load in [false, true] {
            let temp = tempfile::tempdir()?;
            let config = temp.path().join("config.toml");
            std::fs::write(&config, "")?;
            let layout = RuntimeLayout::try_from(temp.path().join("trust"))?;
            let directory = RuntimeDirectory::open_or_create(layout.clone())?;
            directory.with_cache_writer(
                AcquireOptions {
                    timeout: Duration::ZERO,
                    cancelled: None,
                },
                |_| {
                    let mut process = command(temp.path(), &config, layout.root());
                    process
                        .args(["--lock-timeout", "0"])
                        .env("RUST_LOG", "bpfman_lock=trace,bpfman=debug")
                        .stdout(Stdio::null())
                        .stderr(Stdio::piped());
                    if load {
                        process.args([
                            "program",
                            "load",
                            "image",
                            &server.reference,
                            "--programs",
                            "xdp:pass",
                        ]);
                    } else {
                        process.args(["image", "verify", &server.reference]);
                    }
                    let mut child = Process(process.spawn().expect("CLI"));
                    let stderr = child.0.stderr.take().expect("stderr");
                    let (notify, notification) = mpsc::channel();
                    let reader = std::thread::spawn(move || {
                        let mut text = String::new();
                        for line in BufReader::new(stderr).lines() {
                            let line = line.expect("stderr line");
                            if line.contains("\"name\":\"lock.wait\"")
                                && line.contains("\"message\":\"new\"")
                            {
                                let _ = notify.send(());
                            }
                            text.push_str(&line);
                            text.push('\n');
                        }
                        text
                    });
                    notification
                        .recv_timeout(Duration::from_secs(5))
                        .expect("waiting for TUF lock");
                    assert!(
                        Command::new("kill")
                            .arg(format!("-{signal}"))
                            .arg(child.0.id().to_string())
                            .status()
                            .expect("signal")
                            .success()
                    );
                    let deadline = Instant::now() + Duration::from_secs(5);
                    let status = loop {
                        if let Some(status) = child.0.try_wait().expect("reap") {
                            break status;
                        }
                        assert!(Instant::now() < deadline, "TUF lock wait did not cancel");
                        std::thread::sleep(Duration::from_millis(5));
                    };
                    let stderr = reader.join().expect("reader");
                    assert_eq!(
                        status.code(),
                        Some(if signal == "INT" { 130 } else { 143 }),
                        "{stderr}"
                    );
                    assert!(stderr.contains("cancelled"), "{stderr}");
                },
            )?;
            assert!(!layout.database_path().exists());
            assert_eq!(
                std::fs::read_dir(layout.root())?.count(),
                1,
                "only the cooperative lock remains"
            );
            assert!(!temp.path().join("runtime").exists());
            assert!(!temp.path().join("images").exists());
        }
    }
    assert_eq!(server.requests.load(Ordering::Relaxed), 0);
    Ok(())
}
