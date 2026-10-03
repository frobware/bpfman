//! CLI integration with the explicitly selected SQLite backend and Go schema.
//! SQL fixtures here test format interoperability; generic CLI/kernel scenarios
//! use public observations and store contracts instead.

use std::process::Command;
#[path = "../../../tests/support/mod.rs"]
mod support;

#[test]
fn lists_seeded_go_state_with_typed_filters() -> Result<(), Box<dyn std::error::Error>> {
    let db = support::database()?;
    support::seed(&db)?;
    let before = std::fs::read(&db.path)?;

    for (options, expected) in [
        (vec!["-q"], "7\n42\n"),
        (vec!["-q", "--type", "XDP"], "42\n"),
        (vec!["-q", "--type", "xdp", "-p", "tracepoint"], "7\n42\n"),
        (vec!["-q", "--application", "demo"], "42\n"),
        (vec!["-q", "--application", "absent"], ""),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .env("BPFMAN_RUNTIME_DIR", &db.runtime)
            .args(["program", "list"])
            .args(options)
            .output()?;

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout)?, expected);
    }

    assert_eq!(before, std::fs::read(&db.path)?);

    Ok(())
}

#[test]
fn table_preserves_the_go_columns_and_link_handles() -> Result<(), Box<dyn std::error::Error>> {
    let db = support::database()?;
    support::seed(&db)?;
    let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
        .env("BPFMAN_RUNTIME_DIR", &db.runtime)
        .args(["program", "list", "-o", "text"])
        .output()?;

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let text = String::from_utf8(output.stdout)?;
    let rows: Vec<_> = text
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>())
        .collect();

    assert_eq!(
        rows,
        vec![
            vec![
                "PROGRAM",
                "ID",
                "APPLICATION",
                "TYPE",
                "FUNCTION",
                "NAME",
                "LINK",
                "IDS"
            ],
            vec!["7", "tracepoint", "trace", "<none>"],
            vec!["42", "demo", "xdp", "pass", "2", "10"],
        ]
    );

    Ok(())
}

#[test]
fn schema_error_retains_diagnostic_chain() -> Result<(), Box<dyn std::error::Error>> {
    let db = support::database()?;
    db.connection
        .execute("UPDATE goose_db_version SET version_id = 99", [])?;
    let before = std::fs::read(&db.path)?;
    let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
        .env("BPFMAN_RUNTIME_DIR", &db.runtime)
        .args(["program", "list"])
        .output()?;

    assert_eq!(output.status.code(), Some(1));

    let error = String::from_utf8(output.stderr)?;

    assert!(error.contains("access bpfman runtime"));
    assert_eq!(error.matches("schema version mismatch").count(), 1);
    assert!(error.contains("99"));
    assert!(output.stdout.is_empty());
    assert_eq!(std::fs::read(&db.path)?, before);

    Ok(())
}

#[test]
fn existing_reader_rejects_incompatible_schema_without_waiting_for_writer()
-> Result<(), Box<dyn std::error::Error>> {
    let db = support::database()?;
    db.connection
        .execute("UPDATE goose_db_version SET version_id = 99", [])?;
    let layout = bpfman_fs::RuntimeLayout::try_from(db.runtime.clone())?;
    bpfman_lock::with_write_lock(
        &layout.lock_path(),
        bpfman_lock::AcquireOptions {
            timeout: std::time::Duration::from_secs(1),
            cancelled: None,
        },
        |_| -> Result<(), Box<dyn std::error::Error>> {
            let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
                .env("BPFMAN_RUNTIME_DIR", layout.root())
                .args(["program", "list", "--lock-timeout", "25ms"])
                .output()?;

            assert_eq!(output.status.code(), Some(1));

            let error = String::from_utf8(output.stderr)?;

            assert!(error.contains("schema version mismatch"));
            assert!(!error.contains("timed out waiting for lock"));

            Ok(())
        },
    )??;

    Ok(())
}

#[test]
fn first_run_creates_go_compatible_state_and_lists_nothing()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let runtime = directory.path().join("new/runtime");
    let database = runtime.join("db/store.db");

    for _ in 0..2 {
        let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .env("BPFMAN_RUNTIME_DIR", &runtime)
            .args(["program", "list"])
            .output()?;

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
    }

    assert!(runtime.join(".lock").is_file());

    let connection = rusqlite::Connection::open(&database)?;
    let version: i64 =
        connection.query_row("SELECT MAX(version_id) FROM goose_db_version", [], |row| {
            row.get(0)
        })?;

    assert_eq!(version, 2);
    let mode: String = connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    assert_eq!(mode, "wal");
    for entry in std::fs::read_dir(runtime.join("db"))? {
        let name = entry?.file_name();
        assert!(matches!(
            name.to_str(),
            Some("store.db" | "store.db-wal" | "store.db-shm")
        ));
    }

    Ok(())
}

#[test]
fn startup_waits_for_writer_lock_before_creating_database() -> Result<(), Box<dyn std::error::Error>>
{
    use bpfman_lock::{AcquireOptions, with_write_lock};
    use std::time::Duration;
    let directory = tempfile::tempdir()?;
    let runtime = directory.path();
    with_write_lock(
        &runtime.join(".lock"),
        AcquireOptions {
            timeout: Duration::from_secs(1),
            cancelled: None,
        },
        |_| -> Result<(), Box<dyn std::error::Error>> {
            for via_environment in [true, false] {
                let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman"));
                command
                    .env("BPFMAN_RUNTIME_DIR", runtime)
                    .env_remove("BPFMAN_LOCK_TIMEOUT")
                    .args(["program", "list"]);

                if via_environment {
                    command.env("BPFMAN_LOCK_TIMEOUT", "25ms");
                } else {
                    command.args(["--lock-timeout", "25ms"]);
                }

                let output = command.output()?;

                assert_eq!(output.status.code(), Some(1));

                let stderr = String::from_utf8(output.stderr)?;

                assert_eq!(stderr.matches("timed out waiting for lock").count(), 1);
                assert!(output.stdout.is_empty());
                assert!(!runtime.join("db").exists());
            }

            Ok(())
        },
    )??;

    Ok(())
}

#[test]
fn concurrent_first_runs_observe_only_a_complete_database() -> Result<(), Box<dyn std::error::Error>>
{
    let directory = tempfile::tempdir()?;
    let runtime = directory.path().join("new-runtime");
    let mut children = Vec::new();

    for _ in 0..8 {
        children.push(
            Command::new(env!("CARGO_BIN_EXE_bpfman"))
                .env("BPFMAN_RUNTIME_DIR", &runtime)
                .args(["program", "list", "--lock-timeout", "5s"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?,
        );
    }

    for child in children {
        let output = child.wait_with_output()?;

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty());
    }

    for entry in std::fs::read_dir(runtime.join("db"))? {
        let name = entry?.file_name();
        assert!(matches!(
            name.to_str(),
            Some("store.db" | "store.db-wal" | "store.db-shm")
        ));
    }

    Ok(())
}

#[test]
fn missing_and_linked_gets_fail_without_fabricating_kernel_observations()
-> Result<(), Box<dyn std::error::Error>> {
    let db = support::database()?;
    support::seed(&db)?;
    let before = std::fs::read(&db.path)?;

    for (id, diagnostic) in [
        ("99", "does not exist"),
        ("42", "attached programs is not implemented"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .env("BPFMAN_RUNTIME_DIR", &db.runtime)
            .args(["program", "get", id, "-o", "json"])
            .output()?;

        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());

        let stderr = String::from_utf8(output.stderr)?;

        assert!(stderr.contains(diagnostic), "{stderr}");
    }

    assert_eq!(std::fs::read(&db.path)?, before);
    assert!(!db.runtime.join("fs").exists());

    Ok(())
}

#[test]
fn json_list_has_an_empty_envelope_and_quiet_takes_precedence()
-> Result<(), Box<dyn std::error::Error>> {
    let db = support::database()?;
    let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
        .env("BPFMAN_RUNTIME_DIR", &db.runtime)
        .args(["program", "list", "-o", "json"])
        .output()?;

    assert!(output.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout)?,
        serde_json::json!({"programs":[]})
    );
    support::seed(&db)?;

    let output = Command::new(env!("CARGO_BIN_EXE_bpfman"))
        .env("BPFMAN_RUNTIME_DIR", &db.runtime)
        .args(["program", "list", "-o", "json", "-q"])
        .output()?;

    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout)?, "7\n42\n");

    Ok(())
}
