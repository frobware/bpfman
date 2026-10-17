//! Rootless signature-policy CLI contracts, with real release signatures.
#[path = "../../../tests/support/oci.rs"]
mod fixture;
#[path = "../../../tests/support/publish_registry.rs"]
mod publisher;
#[path = "../../../tests/support/signed_image.rs"]
mod signed_image;

use serde_json::json;
use std::{path::Path, process::Command};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn go_and_rust_verify_the_same_private_signatures_and_reject_wrong_identities() -> Result {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let temp = tempfile::tempdir()?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let targets = root.join("vendor/github.com/sigstore/sigstore/pkg/tuf/repository/targets");
    let mut roots = Vec::new();
    for name in [
        "fulcio.crt.pem",
        "fulcio_v1.crt.pem",
        "fulcio_intermediate_v1.crt.pem",
    ] {
        roots.extend(std::fs::read(targets.join(name))?);
        roots.push(b'\n');
    }
    let roots_file = temp.path().join("fulcio.pem");
    std::fs::write(&roots_file, roots)?;
    let config = temp.path().join("config.toml");
    std::fs::write(&config, "[signing]\nallow_unsigned = false\n")?;
    let captured = signed_image::registry();
    let go = std::env::var_os("BPFMAN_GO_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("bin/bpfman"));
    for auth in ["public", "basic", "bearer"] {
        let server = publisher::Registry::new();
        let encoded = STANDARD.encode("private-user:private-password:with:colons");
        let mut state = server.state.lock().expect("state");
        state.bearer = auth == "bearer";
        state.required_auth = (auth != "public").then(|| format!("Basic {encoded}"));
        for (path, (_, body)) in captured.responses.lock().expect("routes").iter() {
            if let Some(selector) = path.strip_prefix("/v2/test/manifests/") {
                state.manifests.insert(selector.into(), body.clone());
            }
            if let Some(digest) = path.strip_prefix("/v2/test/blobs/") {
                state.blobs.insert(digest.into(), body.clone());
            }
        }
        drop(state);
        for binary in [go.as_path(), Path::new(env!("CARGO_BIN_EXE_bpfman"))] {
            for (identity, success) in [(signed_image::SUBJECT, true), ("untrusted", false)] {
                let output = Command::new(binary)
                    .arg("--config")
                    .arg(&config)
                    .arg("--runtime-dir")
                    .arg(temp.path().join("runtime"))
                    .arg("--image-cache-dir")
                    .arg(temp.path().join("cache"))
                    .args([
                        "image",
                        "verify",
                        &server.reference,
                        "--certificate-identity",
                        identity,
                        "--certificate-oidc-issuer",
                        signed_image::ISSUER,
                    ])
                    .env(
                        "BPFMAN_REGISTRY_AUTH",
                        if auth == "public" { "" } else { &encoded },
                    )
                    // Real production trust anchors, supplied explicitly to keep
                    // Go's TUF refresh out of this local offline comparison.
                    .env("SIGSTORE_ROOT_FILE", &roots_file)
                    .env("SIGSTORE_REKOR_PUBLIC_KEY", targets.join("rekor.pub"))
                    .env(
                        "SIGSTORE_CT_LOG_PUBLIC_KEY_FILE",
                        targets.join("ctfe_2022.pub"),
                    )
                    .output()?;
                assert_eq!(
                    output.status.success(),
                    success,
                    "{} ({auth}): {}",
                    binary.display(),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        // Cancellation while receiving an authenticated signature must prevent
        // admission and leave no cache stage or other owned resource.
        server.state.lock().expect("state").cancel_on =
            Some(format!("GET {}", signed_image::signature_path()));
        let credentials =
            bpfman_image_oci::Credentials::new("private-user", "private-password:with:colons")?;
        assert!(
            bpfman_image_oci::verify(
                &bpfman_image_oci::Reference::parse(&server.reference)?,
                Some(&credentials),
                &bpfman_image_oci::SigningPolicy::verify(false, vec![])?,
                &server.cancelled
            )
            .expect_err("cancelled")
            .is_cancelled()
        );
    }
    assert!(!temp.path().join("runtime").exists());
    assert!(!temp.path().join("cache").exists());
    Ok(())
}

fn command(root: &Path, config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman"));
    command
        .arg("--config")
        .arg(config)
        .arg("--runtime-dir")
        .arg(root.join("runtime"))
        .arg("--image-cache-dir")
        .arg(root.join("cache"))
        .env_remove("BPFMAN_REGISTRY_AUTH");
    command
}

#[test]
fn verify_forces_verification_and_honours_identity_overrides_without_runtime_state() -> Result {
    let temp = tempfile::tempdir()?;
    let config = temp.path().join("config.toml");
    std::fs::write(
        &config,
        "[signing]\nverify_enabled = false\nallow_unsigned = false\n",
    )?;
    let server = signed_image::registry();
    for options in [
        vec![],
        vec![
            "--certificate-identity",
            signed_image::SUBJECT,
            "--certificate-oidc-issuer",
            signed_image::ISSUER,
        ],
        vec![
            "--certificate-identity-regexp",
            "https://github.com/bpfman/.*",
            "--certificate-oidc-issuer-regexp",
            "https://token[.]actions[.]githubusercontent[.]com",
        ],
    ] {
        let output = command(temp.path(), &config)
            .args(["image", "verify", &server.reference])
            .args(options)
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout)?,
            format!("Image {}: signature verified\n", server.reference)
        );
    }
    let output = command(temp.path(), &config)
        .args([
            "image",
            "verify",
            &server.reference,
            "--certificate-identity",
            "untrusted",
            "--certificate-oidc-issuer",
            signed_image::ISSUER,
        ])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr)?.contains("not trusted"));
    let count = server.requests.load(std::sync::atomic::Ordering::Relaxed);
    for options in [
        vec!["--certificate-identity", "incomplete"],
        vec![
            "--certificate-identity-regexp",
            "[",
            "--certificate-oidc-issuer",
            signed_image::ISSUER,
        ],
        vec![
            "--certificate-identity",
            "a",
            "--certificate-identity-regexp",
            ".*",
        ],
    ] {
        assert!(
            !command(temp.path(), &config)
                .args(["image", "verify", &server.reference])
                .args(options)
                .output()?
                .status
                .success()
        );
    }
    assert_eq!(
        server.requests.load(std::sync::atomic::Ordering::Relaxed),
        count
    );
    let missing = serde_json::to_vec(&json!({"errors":[{"code":"MANIFEST_UNKNOWN"}]}))?;
    server
        .responses
        .lock()
        .expect("routes")
        .insert(signed_image::signature_path(), (404, missing));
    for (flag, allowed) in [
        (None, false),
        (Some("--allow-unsigned"), true),
        (Some("--allow-unsigned=false"), false),
    ] {
        let mut process = command(temp.path(), &config);
        process.args(["image", "verify", &server.reference]);
        if let Some(flag) = flag {
            process.arg(flag);
        }
        let output = process.output()?;
        assert_eq!(
            output.status.success(),
            allowed,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if allowed {
            assert!(
                String::from_utf8(output.stdout)?.contains("unsigned image accepted by policy")
            );
        }
    }
    assert!(!temp.path().join("runtime").exists());
    assert!(!temp.path().join("cache").exists());
    Ok(())
}

#[test]
fn signing_config_is_validated_before_registry_access_even_when_verification_is_disabled() -> Result
{
    let temp = tempfile::tempdir()?;
    let config = temp.path().join("config.toml");
    let server = signed_image::registry();
    for identity in [
        "certificate_identity = 'incomplete'",
        "certificate_identity = 'a'\ncertificate_identity_regexp = '.*'\ncertificate_oidc_issuer = 'issuer'",
        "certificate_identity_regexp = '['\ncertificate_oidc_issuer = 'issuer'",
    ] {
        std::fs::write(
            &config,
            format!(
                "[signing]\nverify_enabled = false\n[[signing.trusted_identities]]\n{identity}\n"
            ),
        )?;
        let output = command(temp.path(), &config)
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
    }
    assert_eq!(
        server.requests.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    assert!(!temp.path().join("runtime").exists());
    assert!(!temp.path().join("cache").exists());
    assert!(server.digest.starts_with("sha256:"));
    Ok(())
}

#[test]
fn signals_during_signature_reads_reap_the_cli_without_cache_or_runtime_state() -> Result {
    use base64::{Engine, engine::general_purpose::STANDARD};
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
    let captured = signed_image::registry();
    for signal in ["INT", "TERM"] {
        for load in [false, true] {
            let temp = tempfile::tempdir()?;
            let config = temp.path().join("config.toml");
            std::fs::write(&config, "[signing]\nallow_unsigned = false\n")?;
            let server = publisher::Registry::new();
            let encoded = STANDARD.encode("private-user:private-password:with:colons");
            let (waiting, wait) = mpsc::sync_channel(1);
            let (resume, resumed) = mpsc::sync_channel(1);
            {
                let mut state = server.state.lock().expect("state");
                state.bearer = true;
                state.required_auth = Some(format!("Basic {encoded}"));
                for (path, (_, body)) in captured.responses.lock().expect("routes").iter() {
                    if let Some(selector) = path.strip_prefix("/v2/test/manifests/") {
                        state.manifests.insert(selector.into(), body.clone());
                    }
                    if let Some(digest) = path.strip_prefix("/v2/test/blobs/") {
                        state.blobs.insert(digest.into(), body.clone());
                    }
                }
                state.pause = Some((
                    format!("GET {}", signed_image::signature_path()),
                    waiting,
                    resumed,
                ));
            }
            let mut process = command(temp.path(), &config);
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
            let mut child = Process(
                process
                    .env("BPFMAN_REGISTRY_AUTH", &encoded)
                    .env("RUST_LOG", "trace,ureq_proto::util=trace,ureq::util=trace")
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()?,
            );
            let stderr = child.0.stderr.take().expect("stderr");
            let (notified, notification) = mpsc::sync_channel(1);
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
            for secret in [&encoded, "private-password:with:colons", "publish-token"] {
                assert!(!stderr.contains(secret));
            }
            assert!(!temp.path().join("cache").exists());
            assert!(!temp.path().join("runtime").exists());
            assert!(server.state.lock().expect("state").uploads.is_empty());
        }
    }
    Ok(())
}
