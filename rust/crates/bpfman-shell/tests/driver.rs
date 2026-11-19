//! Whole-program CLI contracts for the first Rust shell driver.

use std::{
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    process::{Command, Output, Stdio},
};

fn run(args: &[&str], source: &str) -> io::Result<Output> {
    run_command(
        Command::new(env!("CARGO_BIN_EXE_bpfman-shell")).args(args),
        source,
    )
}

fn run_command(command: &mut Command, source: &str) -> io::Result<Output> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("missing stdin"))?;
    stdin.write_all(source.as_bytes())?;
    drop(stdin);
    child.wait_with_output()
}

#[test]
fn typed_bpfman_uses_the_selected_binary_and_round_trips_ids() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let binary = dir.path().join("selected-bpfman");
    fs::write(
        &binary,
        concat!(
            "#!/bin/sh\n",
            "test \"$1 $2 $3 $4 $5\" = 'program get 7 -o json' || exit 71\n",
            "printf '%s' '{\"record\":{\"program_id\":7},\"big\":18446744073709551615}'\n",
        ),
    )?;
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
    let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman-shell"));
    command.env("BPFMAN_BIN", &binary);
    let output = run_command(
        &mut command,
        "guard p <- bpfman program get 7\nguard q <- bpfman program get $p\nprint $q.record.program_id $q.big",
    )?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"7 18446744073709551615\n");
    Ok(())
}

#[test]
fn checked_stdin_refuses_before_output_and_unchecked_runs_prior_statements() -> io::Result<()> {
    let source = "print before\nprint $missing\n";
    let checked = run(&[], source)?;
    assert!(!checked.status.success());
    assert!(checked.stdout.is_empty());
    assert!(String::from_utf8_lossy(&checked.stderr).contains("undefined variable"));
    let unchecked = run(&["--no-check"], source)?;
    assert!(!unchecked.status.success());
    assert_eq!(unchecked.stdout, b"before\n");
    Ok(())
}

#[test]
fn inspection_never_dispatches_and_execution_drives_external_commands() -> io::Result<()> {
    let source = "print before\nexec printf '%s\\n' after\n";
    let checked = run(&["--check"], source)?;
    assert!(checked.status.success(), "{checked:?}");
    assert!(checked.stdout.is_empty());
    let lowered = run(&["--lowered"], source)?;
    assert!(lowered.status.success(), "{lowered:?}");
    assert!(String::from_utf8_lossy(&lowered.stdout).contains("DispatchCommand argv=t1"));
    let executed = run(&["-"], source)?;
    assert!(executed.status.success(), "{executed:?}");
    assert_eq!(executed.stdout, b"before\nafter\n");
    Ok(())
}

#[test]
fn assertions_use_stderr_continue_and_require_stops_after_cleanup() -> io::Result<()> {
    let source = "let r = record { nested: record { n: 2 } }\nassert $r matches exhaustive { nested: matches { n: 2 } }\nassert false\nassert fail exec false\nprint continued\nrequire false\nprint unreachable";
    let output = run(&[], source)?;
    assert!(!output.status.success());
    assert_eq!(output.stdout, b"continued\n");
    let errors = String::from_utf8_lossy(&output.stderr);
    assert_eq!(errors.matches("assertion failed").count(), 1);
    assert!(errors.contains("require failed"));
    Ok(())
}
