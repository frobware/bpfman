//! CLI input and effect boundary; these tests never load kernel programs.

use std::{
    path::Path,
    process::{Command, Output},
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn command(runtime: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman"));
    command
        .env("BPFMAN_RUNTIME_DIR", runtime)
        .env_remove("BPFMAN_LOCK_TIMEOUT")
        .env_remove("BPFMAN_REGISTRY_AUTH")
        .args(["program", "load"]);
    command
}

fn assert_failure(output: Output, code: i32, diagnostic: &str) -> Result {
    assert_eq!(output.status.code(), Some(code));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr)?;
    assert!(stderr.contains(diagnostic), "{stderr}");
    assert!(!stderr.contains("SQLite"));
    Ok(())
}

#[test]
fn invalid_programs_and_options_fail_before_any_runtime_effects() -> Result {
    let temporary = tempfile::tempdir()?;
    let runtime = temporary.path().join("runtime");
    for spec in [
        "",
        "xdp",
        "unknown:p",
        "xdp:",
        "xdp:p:ignored",
        "fentry:p",
        "fexit:p:",
        "lsm:p",
        "lsm:p:hook:extra",
        "xdp:p,",
        "xdp:p,xdp:p",
        "xdp:p,tc:p",
    ] {
        let output = command(&runtime)
            .args(["file", "not-opened.o", "--programs", spec])
            .output()?;
        assert_failure(output, 2, "error:")?;
        assert!(!runtime.exists());
    }
    for options in [
        vec!["--map-owner-id", "0"],
        vec!["--map-owner-id", "4294967296"],
        vec!["-m", "missing-equals"],
        vec!["-m", " =value"],
        vec!["-g", "name=0x1"],
        vec!["-g", "name=zz"],
        vec!["-g", "=00"],
        vec!["--output", "yaml"],
        vec!["--pull-policy", "Never"],
    ] {
        let output = command(&runtime)
            .args(["file", "not-opened.o", "--programs", "xdp:p"])
            .args(options)
            .output()?;
        assert_failure(output, 2, "error:")?;
        assert!(!runtime.exists());
    }
    for args in [
        vec!["file", "not-opened.o"],
        vec!["file", "", "--programs", "xdp:p"],
        vec!["image", "", "--programs", "xdp:p"],
        vec!["image", "bad reference", "--programs", "xdp:p"],
        vec![
            "image",
            "example.invalid/test",
            "--programs",
            "xdp:p",
            "-p",
            "sometimes",
        ],
    ] {
        assert_failure(command(&runtime).args(args).output()?, 2, "error:")?;
        assert!(!runtime.exists());
    }
    assert_eq!(std::fs::read_dir(temporary.path())?.count(), 0);
    Ok(())
}

#[test]
fn valid_requests_fail_explicitly_without_creating_runtime_or_accessing_source() -> Result {
    let temporary = tempfile::tempdir()?;
    let runtime = temporary.path().join("runtime");
    for (source, name) in [
        ("file", "missing-object.o"),
        ("image", "example.invalid/never-pull:latest"),
    ] {
        for format in ["text", "json"] {
            let output = command(&runtime)
                .current_dir(temporary.path())
                .args([
                    source,
                    name,
                    "--programs",
                    "tracepoint:trace,fentry:enter:do_open",
                    "-m",
                    "owner=test",
                    "-g",
                    "counter=01000000",
                    "-a",
                    "parity",
                    "--map-owner-id",
                    "42",
                    "-o",
                    format,
                ])
                .output()?;
            assert_failure(
                output,
                1,
                &format!("program load {source} execution is not implemented"),
            )?;
            assert!(!runtime.exists());
        }
    }
    assert_eq!(std::fs::read_dir(temporary.path())?.count(), 0);
    Ok(())
}

#[test]
fn registry_credentials_are_not_echoed_in_errors_or_help() -> Result {
    let temporary = tempfile::tempdir()?;
    let runtime = temporary.path().join("runtime");
    // Valid, invalid base64, missing separator, and empty username/password.
    for secret in [
        "dXNlcjpwYXNzOndvcmQ=",
        "secret-not-base64!",
        "c2VjcmV0",
        "OnNlY3JldA==",
        "c2VjcmV0Og==",
    ] {
        for via_environment in [false, true] {
            let mut process = command(&runtime);
            process.args(["image", "example.invalid/test", "--programs", "xdp:p"]);
            if via_environment {
                process.env("BPFMAN_REGISTRY_AUTH", secret);
            } else {
                process.args(["--registry-auth", secret]);
            }
            let output = process.output()?;
            let code = if secret == "dXNlcjpwYXNzOndvcmQ=" {
                1
            } else {
                2
            };
            let diagnostic = if code == 1 {
                "execution is not implemented"
            } else {
                "registry auth"
            };
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(!stderr.contains(secret), "credentials were echoed");
            assert!(!stderr.contains("pass:word"));
            assert_failure(output, code, diagnostic)?;
        }
        let output = command(&runtime)
            .env("BPFMAN_REGISTRY_AUTH", secret)
            .args(["image", "--help"])
            .output()?;
        assert!(output.status.success());
        assert!(!String::from_utf8(output.stdout)?.contains(secret));
        assert!(output.stderr.is_empty());
    }
    assert!(!runtime.exists());
    Ok(())
}

#[test]
fn native_object_paths_are_not_lossily_decoded_or_opened() -> Result {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let temporary = tempfile::tempdir()?;
    let runtime = temporary.path().join("runtime");
    let output = command(&runtime)
        .current_dir(temporary.path())
        .arg("file")
        .arg(OsString::from_vec(b"object-\xff.o".to_vec()))
        .args(["--programs", "xdp:p"])
        .output()?;
    assert_failure(output, 1, "execution is not implemented")?;
    assert_eq!(std::fs::read_dir(temporary.path())?.count(), 0);
    Ok(())
}
