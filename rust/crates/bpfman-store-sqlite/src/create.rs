use std::{io, path::Path};

use rusqlite::Connection;

use crate::{Error, error::Failure};

// Go remains the schema authority. These files contain plain SQL plus Goose
// comment directives; only the Up section is executed for a fresh database.
const MIGRATIONS: [(i64, &str); 2] = [
    (
        1,
        include_str!("../../../../platform/store/sqlite/migrations/00001_baseline.sql"),
    ),
    (
        2,
        include_str!("../../../../platform/store/sqlite/migrations/00002_add_lsm.sql"),
    ),
];

/// Create a missing database and its parent directories using Go's schema.
///
/// Existing paths are never replaced, repaired, or migrated. Creation happens
/// in a temporary sibling file, published without clobbering any competing
/// creator's database. Callers must still validate existing state when reading.
pub fn create_if_missing(writer: &bpfman_fs::RuntimeWriter<'_>) -> Result<(), Error> {
    create(&writer.database_path(), &MIGRATIONS).map_err(Error::from)
}

fn create(path: &Path, migrations: &[(i64, &str)]) -> Result<(), Failure> {
    let filesystem = |source| Failure::Filesystem {
        path: path.to_owned(),
        source,
    };
    // Inspect the entry itself: a dangling symlink is existing state too.
    match std::fs::symlink_metadata(path) {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(filesystem(error)),
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = tempfile::NamedTempFile::new_in(parent).map_err(filesystem)?;
    let mut connection = Connection::open(temporary.path())?;
    let tx = connection.transaction()?;
    // Goose creates its own migration history separately from application SQL.
    tx.execute_batch(
        "CREATE TABLE goose_db_version (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             version_id INTEGER NOT NULL, is_applied INTEGER NOT NULL,
             tstamp TIMESTAMP DEFAULT (datetime('now'))
         );
         INSERT INTO goose_db_version (version_id, is_applied) VALUES (0, 1);",
    )?;
    for &(version, sql) in migrations {
        let up = sql.split_once("-- +goose Down").map_or(sql, |(up, _)| up);
        tx.execute_batch(up)?;
        tx.execute(
            "INSERT INTO goose_db_version (version_id, is_applied) VALUES (?1, 1)",
            [version],
        )?;
    }
    tx.commit()?;
    connection
        .close()
        .map_err(|(_, error)| Failure::Sqlite(error))?;
    temporary.as_file().sync_all().map_err(filesystem)?;
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(filesystem(error.error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_schema_creation_is_not_published() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("store.db");
        let migrations = [
            (1, "CREATE TABLE partial (id INTEGER);"),
            (2, "invalid SQL;"),
        ];
        assert!(create(&path, &migrations).is_err());
        assert!(!path.exists());
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
        Ok(())
    }
}
