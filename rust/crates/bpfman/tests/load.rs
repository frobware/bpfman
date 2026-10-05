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

#[test]
fn unsupported_tracepoint_options_are_rejected_before_source_or_runtime_effects() -> Result {
    let temporary = tempfile::tempdir()?;
    let runtime = temporary.path().join("runtime");

    for options in [
        vec!["--programs", "tracepoint:a,xdp:b"],
        vec!["--programs", "tracepoint:a", "--map-owner-id", "1"],
    ] {
        assert_failure(
            command(&runtime)
                .args(["file", "not-opened.o"])
                .args(options)
                .output()?,
            1,
            "execution is not implemented",
        )?;
        assert!(!runtime.exists());
    }

    Ok(())
}

#[test]
fn unknown_or_wrong_size_globals_fail_before_creating_runtime() -> Result {
    let temporary = tempfile::tempdir()?;
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../e2e/testdata/bpf/tracepoint_kmod_counter.bpf.o");

    for store in ["sqlite", "json"] {
        let runtime = temporary.path().join(store);
        for global in [
            "missing_global=00000000",
            "weight=00",
            "expected_slot=0000000000000000",
        ] {
            let output = command(&runtime)
                .env("BPFMAN_STORE", store)
                .arg("file")
                .arg(&source)
                .args([
                    "--programs",
                    "tracepoint:tracepoint_kmod_recorder",
                    "-g",
                    global,
                ])
                .output()?;
            assert_failure(output, 1, "parse local ELF")?;
            assert!(
                !runtime.exists(),
                "invalid globals must precede store startup"
            );
        }
    }

    Ok(())
}

#[test]
fn local_tracepoint_validates_source_before_runtime_creation() -> Result {
    let temporary = tempfile::tempdir()?;
    let runtime = temporary.path().join("runtime");
    let source = temporary.path().join("invalid.o");
    std::fs::write(&source, b"not an ELF file")?;
    assert_failure(
        command(&runtime)
            .arg("file")
            .arg(&source)
            .args(["--programs", "tracepoint:a"])
            .output()?,
        1,
        "parse local ELF",
    )?;

    assert!(!runtime.exists());

    for format in ["text", "json"] {
        assert_failure(
            command(&runtime)
                .args([
                    "file",
                    "missing.o",
                    "--programs",
                    "tracepoint:a",
                    "-o",
                    format,
                ])
                .output()?,
            1,
            "read local ELF",
        )?;
    }

    assert!(!runtime.exists());

    Ok(())
}

#[test]
fn batch_preparation_uses_captured_elf_and_rejects_duplicate_or_missing_selections() -> Result {
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("source.o");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../e2e/testdata/bpf/multi_prog_tracepoint_kmod_counter.bpf.o");
    std::fs::copy(fixture, &source)?;
    let prepare = || {
        bpfman_runtime::PreparedTracepoint::new(
            &source,
            "tp_a".try_into().expect("symbol"),
            Default::default(),
        )
    };

    for extra in ["tp_a", "missing"] {
        let result = prepare()?.with_additional_programs(vec![extra.try_into().expect("symbol")]);
        match result {
            Err(error) => assert_eq!(error.kind(), bpfman_runtime::LoadErrorKind::InvalidInput),
            Ok(_) => return Err("invalid batch selection accepted".into()),
        }
    }

    let prepared = prepare()?;
    std::fs::write(&source, b"replaced after validation")?;
    let _batch = prepared.with_additional_programs(vec!["tp_b".try_into().expect("symbol")])?;
    Ok(())
}
