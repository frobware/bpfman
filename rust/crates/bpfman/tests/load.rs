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
fn missing_sources_and_image_requests_fail_without_creating_runtime() -> Result {
    let temporary = tempfile::tempdir()?;
    let runtime = temporary.path().join("runtime");

    let config_directory = tempfile::tempdir()?;
    let config = config_directory.path().join("policy.toml");
    std::fs::write(&config, "[signing]\nverify_enabled = true\n")?;

    for (source, name) in [
        ("file", "missing-object.o"),
        ("image", "example.invalid/never-pull:latest"),
    ] {
        for format in ["text", "json"] {
            let output = command(&runtime)
                .current_dir(temporary.path())
                .arg("--config")
                .arg(&config)
                .args([
                    source,
                    name,
                    "--programs",
                    "tracepoint:trace,lsm:check:socket_connect",
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
                if source == "file" {
                    "read local ELF"
                } else {
                    "OCI signature verification is not implemented"
                },
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

    let config = temporary.path().join("config.toml");
    std::fs::write(&config, "")?;

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
            process.arg("--config").arg(&config);
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
                "signature verification"
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
    assert_failure(output, 1, "source path is not UTF-8")?;

    assert_eq!(std::fs::read_dir(temporary.path())?.count(), 0);

    Ok(())
}

#[test]
fn lsm_batch_reaches_source_validation_before_runtime_effects() -> Result {
    let temporary = tempfile::tempdir()?;
    let runtime = temporary.path().join("runtime");

    {
        let options = ["--programs", "tracepoint:a,lsm:b:socket_connect"];
        assert_failure(
            command(&runtime)
                .args(["file", "not-opened.o"])
                .args(options)
                .output()?,
            1,
            "read local ELF",
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

    let runtime = temporary.path().join("runtime");
    for global in [
        "missing_global=00000000",
        "weight=00",
        "expected_slot=0000000000000000",
    ] {
        let output = command(&runtime)
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

    Ok(())
}

#[test]
fn local_program_load_validates_source_before_runtime_creation() -> Result {
    let temporary = tempfile::tempdir()?;
    let runtime = temporary.path().join("runtime");
    let source = temporary.path().join("invalid.o");
    std::fs::write(&source, b"not an ELF file")?;
    for programs in ["tracepoint:a", "tc:a", "tracepoint:a,tc:b"] {
        assert_failure(
            command(&runtime)
                .arg("file")
                .arg(&source)
                .args(["--programs", programs])
                .output()?,
            1,
            "parse local ELF",
        )?;

        assert!(!runtime.exists());

        for format in ["text", "json"] {
            assert_failure(
                command(&runtime)
                    .args(["file", "missing.o", "--programs", programs, "-o", format])
                    .output()?,
                1,
                "read local ELF",
            )?;
        }
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
        bpfman_runtime::PreparedProgram::new(
            &bpfman_kernel_aya::Kernel::default(),
            &source,
            bpfman_model::ProgramSpec::Tracepoint("tp_a".try_into().expect("symbol")),
            Default::default(),
        )
    };

    for extra in ["tp_a", "missing"] {
        let result = prepare()?.with_additional_programs(
            &bpfman_kernel_aya::Kernel::default(),
            vec![bpfman_model::ProgramSpec::Tracepoint(
                extra.try_into().expect("symbol"),
            )],
        );
        match result {
            Err(error) => assert_eq!(error.kind(), bpfman_runtime::LoadErrorKind::InvalidInput),
            Ok(_) => return Err("invalid batch selection accepted".into()),
        }
    }

    let prepared = prepare()?;
    std::fs::write(&source, b"replaced after validation")?;
    let _batch = prepared.with_additional_programs(
        &bpfman_kernel_aya::Kernel::default(),
        vec![bpfman_model::ProgramSpec::Tracepoint(
            "tp_b".try_into().expect("symbol"),
        )],
    )?;
    Ok(())
}

#[test]
fn internal_map_names_cannot_request_implicit_pinning() -> Result {
    let temp = tempfile::tempdir()?;
    let runtime = temp.path().join("runtime");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../e2e/testdata/bpf/tc_counter_pinned.bpf.o");
    let mut bytes = std::fs::read(fixture)?;
    let name = b"tc_stats_map";
    let positions: Vec<_> = bytes
        .windows(name.len())
        .enumerate()
        .filter_map(|(offset, value)| (value == name).then_some(offset))
        .collect();
    assert!(!positions.is_empty());
    // Keep every ELF/BTF string offset and relocation unchanged while giving
    // the public PinByName map an internal-looking name.
    for offset in positions {
        bytes[offset] = b'.';
    }
    let source = temp.path().join("internal-map.o");
    std::fs::write(&source, bytes)?;
    assert_failure(
        command(&runtime)
            .arg("file")
            .arg(source)
            .args(["--programs", "tc:stats"])
            .output()?,
        1,
        "internal maps cannot declare PinByName",
    )?;
    assert!(
        !runtime.exists(),
        "refuse before filesystem or kernel effects"
    );
    Ok(())
}

#[test]
fn tracing_family_mismatches_fail_before_runtime_creation() -> Result {
    let temporary = tempfile::tempdir()?;
    let runtime = temporary.path().join("runtime");
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../e2e/testdata/bpf/fentry_counter.bpf.o");
    for spec in [
        "fentry:test_fexit:do_unlinkat",
        "fexit:test_fentry:do_unlinkat",
    ] {
        assert_failure(
            command(&runtime)
                .arg("file")
                .arg(&source)
                .args(["--programs", spec])
                .output()?,
            1,
            "matching fentry/fexit section",
        )?;
        assert!(!runtime.exists());
    }
    Ok(())
}

#[test]
fn lsm_family_mismatches_fail_before_runtime_creation() -> Result {
    let temporary = tempfile::tempdir()?;
    let runtime = temporary.path().join("runtime");
    for (object, spec, diagnostic) in [
        (
            "fentry_counter.bpf.o",
            "lsm:test_fentry:file_open",
            "LSM selection requires an lsm section",
        ),
        (
            "lsm_exact.bpf.o",
            "fentry:test_lsm:file_open",
            "matching fentry/fexit section",
        ),
        ("lsm_exact.bpf.o", "tracepoint:test_lsm", "not a tracepoint"),
    ] {
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../e2e/testdata/bpf")
            .join(object);
        assert_failure(
            command(&runtime)
                .arg("file")
                .arg(source)
                .args(["--programs", spec])
                .output()?,
            1,
            diagnostic,
        )?;
        assert!(!runtime.exists());
    }
    Ok(())
}
