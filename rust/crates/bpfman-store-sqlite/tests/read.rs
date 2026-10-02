//! Read-only contract against the Go migration SQL, with no kernel dependency.

use bpfman_store_sqlite::{ErrorKind, Store, initialise_if_missing};

#[path = "../../../tests/support/mod.rs"]
mod support;

fn read_programs(
    path: &std::path::Path,
) -> Result<Vec<bpfman_model::StoredProgramSummary>, bpfman_store_sqlite::Error> {
    Store::open(path)?.read_programs()
}

fn initialise(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    bpfman_lock::with_write_lock(
        &directory.path().join(".lock"),
        bpfman_lock::AcquireOptions {
            timeout: std::time::Duration::from_secs(1),
            cancelled: None,
        },
        |permit| initialise_if_missing(path, &permit),
    )??;
    Ok(())
}

#[test]
fn reads_go_schema_and_leaves_database_unchanged() -> Result<(), Box<dyn std::error::Error>> {
    let db = support::database()?;
    support::seed(&db)?;
    let before = std::fs::read(&db.path)?;
    let programs = read_programs(&db.path)?;
    assert_eq!(
        programs.iter().map(|p| p.id().get()).collect::<Vec<_>>(),
        [7, 42]
    );
    assert_eq!(programs[1].name(), "pass");
    assert_eq!(programs[1].application(), "demo");
    assert_eq!(
        programs[1]
            .links()
            .iter()
            .map(|id| id.get())
            .collect::<Vec<_>>(),
        [2, 10]
    );
    assert_eq!(before, std::fs::read(&db.path)?);
    assert_eq!(std::fs::read_dir(db.runtime.join("db"))?.count(), 1);
    Ok(())
}

#[test]
fn missing_database_is_not_created() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("missing.db");
    let error = read_programs(&path).expect_err("missing database");
    assert_eq!(error.kind(), ErrorKind::Unavailable);
    assert!(!path.exists());
    Ok(())
}

#[test]
fn refuses_old_and_new_schemas_without_migrating() -> Result<(), Box<dyn std::error::Error>> {
    for version in [1, 3] {
        let db = support::database()?;
        db.connection
            .execute("UPDATE goose_db_version SET version_id = ?1", [version])?;
        let before = std::fs::read(&db.path)?;
        initialise(&db.path)?;
        assert_eq!(
            read_programs(&db.path)
                .expect_err("unsupported schema")
                .kind(),
            ErrorKind::IncompatibleSchema
        );
        assert_eq!(before, std::fs::read(&db.path)?);
    }
    Ok(())
}

#[test]
fn initialises_go_schema_and_history_idempotently() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime/db/store.db");
    initialise(&path)?;
    assert!(read_programs(&path)?.is_empty());
    let before = std::fs::read(&path)?;
    initialise(&path)?;
    assert_eq!(before, std::fs::read(&path)?);
    let actual = rusqlite::Connection::open(&path)?;
    let expected = support::database()?;
    for query in [
        "SELECT name, sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' ORDER BY name",
        "SELECT CAST(version_id AS TEXT), CAST(is_applied AS TEXT) FROM goose_db_version ORDER BY id",
    ] {
        let values =
            |connection: &rusqlite::Connection| -> rusqlite::Result<Vec<(String, String)>> {
                connection
                    .prepare(query)?
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect()
            };
        assert_eq!(values(&actual)?, values(&expected.connection)?);
    }
    assert_eq!(
        std::fs::read_dir(path.parent().ok_or("missing parent")?)?.count(),
        1
    );
    Ok(())
}

#[test]
fn existing_empty_or_corrupt_files_are_never_repaired() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("store.db");
    for bytes in [b"".as_slice(), b"not a SQLite database".as_slice()] {
        std::fs::write(&path, bytes)?;
        initialise(&path)?;
        assert!(read_programs(&path).is_err());
        assert_eq!(std::fs::read(&path)?, bytes);
    }
    Ok(())
}

#[test]
fn initialisation_reports_filesystem_errors_without_backend_types()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let parent = directory.path().join("not-a-directory");
    std::fs::write(&parent, b"preserve me")?;
    bpfman_lock::with_write_lock(
        &directory.path().join(".lock"),
        bpfman_lock::AcquireOptions {
            timeout: std::time::Duration::from_secs(1),
            cancelled: None,
        },
        |permit| {
            let error = initialise_if_missing(&parent.join("store.db"), &permit)
                .expect_err("invalid parent");
            assert_eq!(error.kind(), ErrorKind::Unavailable);
        },
    )?;
    assert_eq!(std::fs::read(&parent)?, b"preserve me");
    Ok(())
}

#[test]
fn rejects_malformed_stored_values() -> Result<(), Box<dyn std::error::Error>> {
    for update in [
        "UPDATE managed_programs SET program_type = 'bogus' WHERE program_id = 7",
        "UPDATE managed_programs SET program_id = 0 WHERE program_id = 7",
        "UPDATE managed_programs SET metadata_json = '{\"x\":2}' WHERE program_id = 7",
    ] {
        let db = support::database()?;
        support::seed(&db)?;
        db.connection.execute(update, [])?;
        assert_eq!(
            read_programs(&db.path)
                .expect_err("invalid stored value")
                .kind(),
            ErrorKind::InvalidData
        );
    }
    Ok(())
}

#[test]
fn reads_committed_wal_state() -> Result<(), Box<dyn std::error::Error>> {
    let db = support::database()?;
    db.connection.pragma_update(None, "journal_mode", "WAL")?;
    support::seed(&db)?;
    assert_eq!(read_programs(&db.path)?.len(), 2);
    Ok(())
}

#[test]
fn inspection_distinguishes_absence_from_incompatible_or_broken_state()
-> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let missing = directory.path().join("missing/store.db");
    assert!(Store::inspect(&missing)?.is_none());
    assert!(!directory.path().join("missing").exists());
    let db = support::database()?;
    db.connection
        .execute("UPDATE goose_db_version SET version_id = 99", [])?;
    let before = std::fs::read(&db.path)?;
    let mut observed = Store::inspect(&db.path)?.ok_or("existing store")?;
    assert_eq!(observed.schema_version(), 99);
    assert_eq!(
        observed
            .read_programs()
            .expect_err("unsupported schema")
            .kind(),
        ErrorKind::IncompatibleSchema
    );
    assert_eq!(std::fs::read(&db.path)?, before);
    let corrupt = directory.path().join("corrupt.db");
    std::fs::write(&corrupt, b"not SQLite")?;
    assert!(Store::inspect(&corrupt).is_err());
    let dangling = directory.path().join("dangling.db");
    std::os::unix::fs::symlink(&missing, &dangling)?;
    assert!(Store::inspect(&dangling).is_err());
    assert!(!missing.exists());
    Ok(())
}

#[test]
fn reads_recheck_schema_after_setup_observation() -> Result<(), Box<dyn std::error::Error>> {
    let db = support::database()?;
    let mut store = Store::open(&db.path)?;
    assert_eq!(store.schema_version(), 2);
    db.connection
        .execute("UPDATE goose_db_version SET version_id = 3", [])?;
    assert_eq!(
        store.read_programs().expect_err("schema changed").kind(),
        ErrorKind::IncompatibleSchema
    );
    Ok(())
}
