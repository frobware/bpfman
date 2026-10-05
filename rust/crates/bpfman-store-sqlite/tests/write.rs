//! Real filesystem and store contracts for local tracepoint loading.
#![allow(clippy::expect_used)]

use bpfman_fs::{RuntimeDirectory, RuntimeLayout};
use bpfman_lock::AcquireOptions;
use bpfman_model::Symbol;
use bpfman_store_sqlite::{LoadRecord, Store, create_if_missing, persist_program};
use rusqlite::Connection;
use std::{collections::BTreeMap, num::NonZeroU32, time::Duration};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn atomic_load_records_are_go_readable_and_failures_leave_no_partial_map_set() -> Result {
    let temp = tempfile::tempdir()?;
    let layout = RuntimeLayout::try_from(temp.path().to_owned())?;
    let runtime = RuntimeDirectory::open_or_create(layout.clone())?;
    runtime.with_writer(AcquireOptions { timeout: Duration::from_secs(1), cancelled: None }, |writer| -> Result {
        create_if_missing(&writer)?;
        let name = Symbol::try_from("tracepoint_kill_recorder")?;
        let metadata = BTreeMap::from([("bpfman.io/application".into(), "rust-slice".into())]);
        let globals = Default::default();
        let spec = bpfman_model::ProgramSpec::Tracepoint(name.clone());
    let record = |id| LoadRecord { globals: &globals, id: NonZeroU32::new(id).expect("nonzero fixture"), spec: &spec,
            source: "./original.o", license: "Dual BSD/GPL", created_at: "2026-10-03T12:00:00Z", metadata: &metadata };
        persist_program(&writer, record(42))?;
        let db = Connection::open(layout.database_path())?;
        let (source, gpl, updated): (String, bool, Option<String>) = db.query_row(
            "SELECT source_path, gpl_compatible, updated_at FROM managed_programs WHERE program_id=42", [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;

        assert_eq!(source, "./original.o"); assert!(gpl); assert!(updated.is_none());

        let mut store = Store::open(&layout.database_path())?;
        let programs = store.read_programs()?;

        assert_eq!(programs.len(), 1);
        assert_eq!(programs[0].application(), "rust-slice");
        assert!(programs[0].links().is_empty());
        assert!(persist_program(&writer, record(42)).is_err());
        db.execute_batch("CREATE TRIGGER reject_load BEFORE INSERT ON managed_programs BEGIN SELECT RAISE(ABORT, 'injected program insert failure'); END;")?;
        assert!(persist_program(&writer, record(43)).is_err());

        let counts: (u32, u32) = db.query_row("SELECT (SELECT count(*) FROM map_sets), (SELECT count(*) FROM managed_programs)", [], |r| Ok((r.get(0)?,r.get(1)?)))?;

        assert_eq!(counts, (1, 1));
        db.execute_batch("DROP TRIGGER reject_load; INSERT INTO goose_db_version(version_id,is_applied) VALUES(999,1);")?;
        assert!(persist_program(&writer, record(44)).is_err());
        assert_eq!(db.query_row("SELECT count(*) FROM map_sets", [], |r| r.get::<_, u32>(0))?, 1);

        Ok(())
    })??;

    Ok(())
}
