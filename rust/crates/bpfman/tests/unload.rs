//! Narrow unload command parsing, classification and preflight behavior.

use std::process::Command;
#[path = "../../../tests/support/mod.rs"]
mod support;
#[test]
fn malformed_unload_operands_fail_before_runtime_setup() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("not-created");

    for operands in [
        &[][..],
        &["0"][..],
        &["-1"][..],
        &["4294967296"][..],
        &["42", "bad"][..],
        &["42", "0"][..],
        &["--ignore-missing"][..],
        &["42", "--unknown"][..],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .arg("--runtime-dir")
            .arg(&root)
            .args(["program", "unload"])
            .args(operands)
            .output()?;

        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
        assert!(!root.exists());
    }

    Ok(())
}

#[test]
fn missing_program_is_an_error_and_leaves_the_initialized_store_empty()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let result = Command::new(env!("CARGO_BIN_EXE_bpfman"))
        .arg("--runtime-dir")
        .arg(temp.path())
        .args(["program", "unload", "42"])
        .output()?;

    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8(result.stderr)?.contains("managed program 42 does not exist"));
    let listed = Command::new(env!("CARGO_BIN_EXE_bpfman"))
        .arg("--runtime-dir")
        .arg(temp.path())
        .args(["program", "list", "-q"])
        .output()?;

    assert!(listed.status.success());
    assert!(listed.stdout.is_empty());
    assert!(!temp.path().join("fs").exists());

    Ok(())
}

#[test]
fn noncanonical_stored_state_is_unchanged() -> Result<(), Box<dyn std::error::Error>> {
    let db = support::database()?;
    support::seed(&db)?;
    let before = std::fs::read(&db.path)?;

    for (id, ignore) in [("42", false), ("7", false), ("42", true), ("7", true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman"));
        command
            .arg("--runtime-dir")
            .arg(&db.runtime)
            .args(["program", "unload", id]);
        if ignore {
            command.arg("--ignore-missing");
        }
        let result = command.output()?;

        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        assert!(
            String::from_utf8(result.stderr)?
                .contains("stored artifact path differs from canonical runtime layout")
        );
        assert_eq!(std::fs::read(&db.path)?, before);
        assert!(!db.runtime.join("fs").exists());
    }

    Ok(())
}

#[test]
fn missing_batch_reports_every_operand_in_order() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    for ignore in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman"));
        command
            .arg("--runtime-dir")
            .arg(temp.path())
            .args(["program", "unload", "43", "42", "43"]);
        if ignore {
            command.arg("--ignore-missing");
        }
        let output = command.output()?;
        assert!(output.stdout.is_empty());
        if ignore {
            assert!(output.status.success());
            assert!(output.stderr.is_empty());
        } else {
            assert_eq!(output.status.code(), Some(1));
            let stderr = String::from_utf8(output.stderr)?;
            let lines: Vec<_> = stderr.lines().collect();
            assert_eq!(lines.len(), 4);
            for (line, id) in lines[..3].iter().zip([43, 42, 43]) {
                assert!(line.starts_with(&format!("program {id}: ")));
                assert!(line.contains(&format!("managed program {id} does not exist")));
            }
            assert_eq!(lines[3], "Error: 3 of 3 program(s) failed to unload");
        }
    }
    assert!(!temp.path().join("fs").exists());
    Ok(())
}
