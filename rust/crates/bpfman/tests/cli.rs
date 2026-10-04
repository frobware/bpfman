//! Process-level checks for the initial CLI surface; no privileged setup.

use std::process::{Command, Output};

fn run(args: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_bpfman"))
        .env_remove("BPFMAN_RUNTIME_DIR")
        .env_remove("BPFMAN_LOCK_TIMEOUT")
        .env_remove("BPFMAN_STORE")
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
    ] {
        let output = run(&args)?;

        assert_eq!(output.status.code(), Some(2));
        assert!(!String::from_utf8(output.stderr)?.contains("SQLite"));
    }

    Ok(())
}

#[test]
fn selected_store_reopens_and_refuses_a_different_format() -> Result<(), Box<dyn std::error::Error>>
{
    for (selected, wrong) in [("sqlite", "json"), ("json", "sqlite")] {
        let directory = tempfile::tempdir()?;
        let command = || {
            let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman"));
            command
                .arg("--runtime-dir")
                .arg(directory.path())
                .env("BPFMAN_STORE", selected)
                .args(["program", "list", "-o", "json"]);
            command
        };

        let initial = command().output()?;

        assert!(initial.status.success());

        let programs: serde_json::Value = serde_json::from_slice(&initial.stdout)?;

        assert_eq!(programs["programs"], serde_json::json!([]));

        // The explicit CLI choice overrides the environment; mismatched state
        // fails rather than being replaced or interpreted as an empty inventory.

        let refused = command().args(["--store", wrong]).output()?;

        assert_eq!(refused.status.code(), Some(1));
        assert!(refused.stdout.is_empty());

        let reopened = command().output()?;

        assert!(reopened.status.success());
        assert_eq!(reopened.stdout, initial.stdout);
    }

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
