//! Go's authoritative SQLite schema and seed data for adapter test fixtures.

use rusqlite::Connection;

// Include the migrations rather than maintaining a test-specific SQL schema.
const MIGRATIONS: [&str; 2] = [
    include_str!("../../../platform/store/sqlite/migrations/00001_baseline.sql"),
    include_str!("../../../platform/store/sqlite/migrations/00002_add_lsm.sql"),
];

pub(super) fn initialize(connection: &mut Connection) -> rusqlite::Result<()> {
    let tx = connection.transaction()?;

    for sql in MIGRATIONS {
        let up = sql.split_once("-- +goose Down").map_or(sql, |(up, _)| up);
        tx.execute_batch(up)?;
    }

    // Goose owns its version table separately from the application migrations.
    tx.execute_batch(
        "CREATE TABLE goose_db_version (
             id INTEGER PRIMARY KEY AUTOINCREMENT,
             version_id INTEGER NOT NULL, is_applied INTEGER NOT NULL,
             tstamp TIMESTAMP DEFAULT (datetime('now'))
         );
         INSERT INTO goose_db_version (version_id, is_applied) VALUES (0, 1), (1, 1), (2, 1);",
    )?;

    tx.commit()
}

pub(super) fn seed(connection: &Connection) -> rusqlite::Result<()> {
    let tx = connection.unchecked_transaction()?;
    tx.execute_batch(
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
    )?;

    tx.commit()
}
