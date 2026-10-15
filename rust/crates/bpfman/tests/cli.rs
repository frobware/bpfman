//! Process-level checks for the initial CLI surface; no privileged setup.

use std::process::{Command, Output};

fn run(args: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_bpfman"))
        .env_remove("BPFMAN_RUNTIME_DIR")
        .env_remove("BPFMAN_LOCK_TIMEOUT")
        .env_remove("BPFMAN_STORE")
        .env_remove("BPFMAN_CONFIG")
        .args(args)
        .output()
}

#[test]
fn invalid_flags_are_rejected_before_opening_the_database() -> Result<(), Box<dyn std::error::Error>>
{
    for args in [
        vec!["--runtime-dir", "relative", "program", "list"],
        vec!["program", "list", "--type", "bogus"],
        vec!["program", "list", "--output", "yaml"],
        vec!["program", "list", "--all"],
        vec!["program", "list", "--attached"],
        vec!["--store", "unknown", "program", "list"],
        vec!["--store", "sqlite", "program", "list"],
        vec!["--store", "json", "program", "list"],
    ] {
        let output = run(&args)?;

        assert_eq!(output.status.code(), Some(2));
        assert!(!String::from_utf8(output.stderr)?.contains("SQLite"));
    }

    Ok(())
}

#[test]
fn implicit_sqlite_reopens_and_ignores_the_retired_store_environment()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman"));

        command
            .arg("--runtime-dir")
            .arg(directory.path())
            .arg("--config")
            .arg(directory.path().join("missing.toml"))
            .args(["program", "list", "-o", "json"])
            .env("BPFMAN_STORE", "json");

        command
    };

    let initial = command().output()?;

    assert!(
        initial.status.success(),
        "{}",
        String::from_utf8_lossy(&initial.stderr)
    );

    let programs: serde_json::Value = serde_json::from_slice(&initial.stdout)?;
    assert_eq!(programs["programs"], serde_json::json!([]));

    let database = std::fs::read(directory.path().join("db/store.db"))?;
    assert!(database.starts_with(b"SQLite format 3\0"));

    let reopened = command().output()?;

    assert!(reopened.status.success());
    assert_eq!(reopened.stdout, initial.stdout);

    Ok(())
}

#[test]
fn incompatible_store_is_refused_without_replacement() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    std::fs::create_dir(directory.path().join("db"))?;
    let database = directory.path().join("db/store.db");
    let original = br#"{"version":16,"identity":"0123456789abcdef0123456789abcdef","next_generation":1,"programs":[],"map_sets":[],"links":[]}"#;
    std::fs::write(&database, original)?;

    let refused = Command::new(env!("CARGO_BIN_EXE_bpfman"))
        .arg("--runtime-dir")
        .arg(directory.path())
        .args(["program", "list", "-o", "json"])
        .output()?;

    assert_eq!(refused.status.code(), Some(1));
    assert!(refused.stdout.is_empty());
    assert_eq!(std::fs::read(database)?, original);

    Ok(())
}

#[test]
fn help_describes_the_experimental_scope() -> Result<(), Box<dyn std::error::Error>> {
    let output = run(&["--help"])?;

    assert!(output.status.success());

    let stdout = String::from_utf8(output.stdout)?;

    assert!(stdout.contains("Usage: bpfman"));
    assert!(stdout.contains("managed program listing"));
    assert!(stdout.contains("--lock-timeout"));
    assert!(stdout.contains("--version"));
    assert!(!stdout.contains("--store"));
    assert!(output.stderr.is_empty());

    Ok(())
}

#[test]
fn invalid_timeout_does_not_touch_runtime() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let runtime = directory.path().join("absent");

    for value in ["-1s", "potato"] {
        let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .env("BPFMAN_RUNTIME_DIR", &runtime)
            .args(["program", "list", &format!("--lock-timeout={value}")])
            .output()?;

        assert_eq!(output.status.code(), Some(2));
        assert!(!runtime.exists());
    }

    Ok(())
}

#[test]
fn invalid_runtime_roots_fail_at_the_input_boundary() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;

    for root in ["", "relative-runtime", "/", "/tmp/.."] {
        for via_environment in [true, false] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman"));
            command
                .current_dir(directory.path())
                .env_remove("BPFMAN_RUNTIME_DIR")
                .env_remove("BPFMAN_LOCK_TIMEOUT")
                .args(["program", "list"]);

            if via_environment {
                command.env("BPFMAN_RUNTIME_DIR", root);
            } else {
                command.args(["--runtime-dir", root]);
            }

            let output = command.output()?;

            assert_eq!(output.status.code(), Some(2));
            assert!(output.stdout.is_empty());
            assert!(!String::from_utf8(output.stderr)?.contains("SQLite"));
            assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
        }
    }

    Ok(())
}

#[test]
fn version_identifies_the_new_workspace_build() -> Result<(), Box<dyn std::error::Error>> {
    let output = run(&["--version"])?;

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)?,
        concat!("bpfman ", env!("CARGO_PKG_VERSION"), "\n")
    );
    assert!(output.stderr.is_empty());

    Ok(())
}

#[test]
fn absent_and_unimplemented_commands_do_not_report_success()
-> Result<(), Box<dyn std::error::Error>> {
    for args in [&[][..], &["program", "load"][..], &["--unknown"][..]] {
        let output = run(args)?;

        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!output.stderr.is_empty());
    }

    Ok(())
}

#[test]
fn get_rejects_bad_ids_before_runtime_creation() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let runtime = directory.path().join("absent");

    for id in ["0", "-1", "4294967296", "abc"] {
        let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .env("BPFMAN_RUNTIME_DIR", &runtime)
            .args(["program", "get", id, "-o", "json"])
            .output()?;

        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!runtime.exists());
    }

    Ok(())
}

#[test]
fn invalid_link_requests_fail_before_runtime_creation() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let runtime = directory.path().join("absent");

    for args in [
        vec!["get", "0"],
        vec!["detach", "18446744073709551616"],
        vec!["attach", "kprobe", "42", "../bad"],
        vec!["attach", "kprobe", "0", "function"],
        vec!["attach", "kprobe", "42", "function", "--offset", "-1"],
        vec![
            "attach",
            "kprobe",
            "42",
            "function",
            "--offset",
            "18446744073709551615",
        ],
        vec!["attach", "tracepoint", "0", "syscalls/sys_enter_kill"],
        vec!["attach", "tracepoint", "42", "../event"],
        vec!["attach", "tracepoint", "42", "a/b/c"],
        vec!["attach", "tracepoint", "42", "a/b", "-m", "=value"],
        vec!["attach", "xdp", "42", "lo"],
        vec!["list", "--all"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .env("BPFMAN_RUNTIME_DIR", &runtime)
            .arg("link")
            .args(args)
            .output()?;
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(!runtime.exists());
    }

    Ok(())
}

#[test]
fn invalid_xdp_config_fails_before_runtime_creation() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let runtime = directory.path().join("absent");
    let config = directory.path().join("bpfman.toml");

    for (contents, diagnostic) in [
        (None, "read bpfman config"),
        (Some("[interfaces"), "parse bpfman config"),
        (
            Some("[interfaces.lo]\nxdp_mode = \"bogus\"\n"),
            "invalid xdp_mode",
        ),
    ] {
        if let Some(contents) = contents {
            std::fs::write(&config, contents)?;
        }
        let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .env("BPFMAN_RUNTIME_DIR", &runtime)
            .env_remove("BPFMAN_STORE")
            .env_remove("BPFMAN_LOCK_TIMEOUT")
            .arg("--config")
            .arg(&config)
            .args(["link", "attach", "xdp", "42", "lo", "--priority", "50"])
            .output()?;

        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8(output.stderr)?.contains(diagnostic));
        assert!(!runtime.exists());
    }

    Ok(())
}

#[test]
fn host_probe_admission_precedes_runtime_creation() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let runtime = temp.path().join("absent");
    for args in [
        vec!["link", "attach", "uprobe", "42", ".", "--fn-name", "malloc"],
        vec!["link", "attach", "uprobe", "42", "/bin/target"],
        vec![
            "link",
            "attach",
            "uprobe",
            "0",
            "/bin/target",
            "--fn-name",
            "entry",
        ],
        vec![
            "link",
            "attach",
            "uprobe",
            "42",
            "/bin/target",
            "--fn-name",
            "bad:symbol",
        ],
        vec![
            "link",
            "attach",
            "uprobe",
            "42",
            "/bin/target",
            "--offset",
            "0",
        ],
        vec![
            "link",
            "attach",
            "uprobe",
            "42",
            "/bin/target",
            "--offset",
            "-1",
        ],
        vec![
            "link",
            "attach",
            "uprobe",
            "42",
            "/bin/target",
            "--offset",
            "18446744073709551615",
        ],
        vec![
            "link",
            "attach",
            "uprobe",
            "42",
            "/bin/target",
            "--fn-name",
            "entry",
            "--pid",
            "-1",
        ],
        vec![
            "link",
            "attach",
            "uprobe",
            "42",
            "/bin/target",
            "--fn-name",
            "entry",
            "--pid",
            "2147483648",
        ],
        vec![
            "link",
            "attach",
            "uprobe",
            "42",
            "/bin/target",
            "--fn-name",
            "entry",
            "--container-pid",
            "2147483648",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .env("BPFMAN_RUNTIME_DIR", &runtime)
            .args(&args)
            .output()?;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!runtime.exists());
    }
    // Noncanonical zero spellings bypass Clap's literal conditional but must
    // still fail domain preparation before any store/runtime effects.
    let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
        .env("BPFMAN_RUNTIME_DIR", &runtime)
        .args([
            "link",
            "attach",
            "uprobe",
            "42",
            "/bin/target",
            "--offset",
            "00",
        ])
        .output()?;
    assert_eq!(output.status.code(), Some(1));
    assert!(!runtime.exists());

    Ok(())
}
