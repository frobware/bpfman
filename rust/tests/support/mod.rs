//! SQLite-specific Go compatibility fixture for adapter and CLI integration tests.
//! The production CLI selects SQLite. Backend-neutral runtime tests instead use
//! domain records and failures through the store contracts, with no SQL fixtures.

use std::path::PathBuf;

use rusqlite::Connection;

// The Go tree is the schema authority. Do not copy these migrations into rust/.
const MIGRATIONS: [&str; 2] = [
    include_str!("../../../platform/store/sqlite/migrations/00001_baseline.sql"),
    include_str!("../../../platform/store/sqlite/migrations/00002_add_lsm.sql"),
];

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
    let connection = Connection::open(&path)?;
    for migration in MIGRATIONS {
        let up = migration
            .split("-- +goose Down")
            .next()
            .ok_or("missing Up migration")?;
        connection.execute_batch(up)?;
    }
    // Goose owns its version table separately from the application migrations.
    connection.execute_batch(
        "CREATE TABLE goose_db_version (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             version_id INTEGER NOT NULL, is_applied INTEGER NOT NULL,
             tstamp TIMESTAMP DEFAULT (datetime('now'))
         );
         INSERT INTO goose_db_version (version_id, is_applied) VALUES (0, 1), (1, 1), (2, 1);",
    )?;
    Ok(Database {
        _directory: directory,
        runtime,
        path,
        connection,
    })
}

pub(crate) fn seed(database: &Database) -> rusqlite::Result<()> {
    database.connection.execute_batch(
        "INSERT INTO map_sets VALUES (42, '/run/bpfman/fs/maps/42', '2026-07-07T12:00:00Z');
         INSERT INTO managed_programs
             (program_id, program_name, program_type, object_path, pin_path, map_set_id, metadata_json, created_at)
         VALUES
             (42, 'pass', 'xdp', '/bytecode/xdp.o', '/pins/42', 42,
              '{\"bpfman.io/application\":\"demo\"}', '2026-07-07T12:00:00Z'),
             (7, 'trace', 'tracepoint', '/bytecode/tp.o', '/pins/7', 42,
              '{}', '2026-07-07T12:00:00Z');
         INSERT INTO links (id, kind, kernel_prog_id, created_at) VALUES
             (10, 'xdp', 42, '2026-07-07T12:00:00Z'),
             (2, 'xdp', 42, '2026-07-07T12:00:00Z');",
    )
}
