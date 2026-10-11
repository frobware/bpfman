//! SQLite-specific Go compatibility fixture for adapter and CLI integration tests.
//! The production CLI selects SQLite. Backend-neutral runtime tests instead use
//! domain records and failures through the store contracts, with no SQL fixtures.

use std::path::PathBuf;

use rusqlite::Connection;

mod schema;

pub(crate) struct Database {
    _directory: tempfile::TempDir,
    pub(crate) runtime: PathBuf,
    pub(crate) path: PathBuf,
    pub(crate) connection: Connection,
}

pub(crate) fn database() -> Result<Database, Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let runtime = directory.path().to_owned();
    std::fs::create_dir(runtime.join("db"))?;
    let path = runtime.join("db/store.db");
    let mut connection = Connection::open(&path)?;
    schema::initialize(&mut connection)?;

    Ok(Database {
        _directory: directory,
        runtime,
        path,
        connection,
    })
}

pub(crate) fn seed(database: &Database) -> rusqlite::Result<()> {
    schema::seed(&database.connection)
}
