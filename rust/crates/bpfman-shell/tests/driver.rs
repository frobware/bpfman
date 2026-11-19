//! Whole-program CLI contracts for the first Rust shell driver.

use std::{
    io::{self, Write},
    process::{Command, Output, Stdio},
};

fn run(args: &[&str], source: &str) -> io::Result<Output> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_bpfman-shell"))
        .args(args)
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
