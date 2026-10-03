//! Narrow unload command parsing, classification and preflight behavior.

use std::process::Command;
#[path = "../../../tests/support/mod.rs"]
mod support;
#[test]
fn malformed_or_unsupported_unload_operands_fail_before_runtime_setup()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().join("not-created");

    for operands in [
        &[][..],
        &["0"][..],
        &["-1"][..],
        &["4294967296"][..],
        &["42", "43"][..],
        &["42", "--ignore-missing"][..],
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
fn missing_program_is_an_error_and_does_not_create_a_database()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let result = Command::new(env!("CARGO_BIN_EXE_bpfman"))
        .arg("--runtime-dir")
        .arg(temp.path())
        .args(["program", "unload", "42"])
        .output()?;

    assert_eq!(result.status.code(), Some(1));
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8(result.stderr)?.contains("managed program 42 not found"));
    assert!(!temp.path().join("db/store.db").exists());

    Ok(())
}

#[test]
fn unsupported_stored_state_is_unchanged() -> Result<(), Box<dyn std::error::Error>> {
    let db = support::database()?;
    support::seed(&db)?;
    let before = std::fs::read(&db.path)?;

    for id in ["42", "7"] {
        let result = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .arg("--runtime-dir")
            .arg(&db.runtime)
            .args(["program", "unload", id])
            .output()?;

        assert_eq!(result.status.code(), Some(1));
        assert!(result.stdout.is_empty());
        assert!(String::from_utf8(result.stderr)?.contains("unsupported unload"));
        assert_eq!(std::fs::read(&db.path)?, before);
        assert!(!db.runtime.join("fs").exists());
    }

    Ok(())
}
