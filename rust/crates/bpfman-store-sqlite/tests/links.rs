//! SQLite-only schema, transaction, and malformed-row guarantees. Shared link
//! behaviour is exercised in bpfman's backend-independent link_store suite.
#![allow(clippy::expect_used)]

use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_model::Symbol;
use bpfman_store::{CommitLoad, LinkReader, LinkStore, LoadRecord, OpenStore, PendingTracepoint};
use bpfman_store_sqlite::{Backend, Store};
use rusqlite::Connection;
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
    time::Duration,
};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn scope(test: impl FnOnce(&RuntimeWriter<'_>, &Connection, &mut Store) -> Result) -> Result {
    let temp = tempfile::tempdir()?;
    let layout = RuntimeLayout::try_from(temp.path().to_owned())?;
    let runtime = RuntimeDirectory::open_or_create(layout.clone())?;
    runtime.with_writer(
        AcquireOptions {
            timeout: Duration::from_secs(1),
            cancelled: None,
        },
        |w| -> Result {
            let mut reader = Backend.open(&w)?;
            Backend.commit_program(
                &w,
                LoadRecord {
                    map_owner: None,
                    shared_maps: &[],
                    globals: &Default::default(),
                    id: NonZeroU32::new(42).expect("id"),
                    spec: &bpfman_model::ProgramSpec::Tracepoint(Symbol::try_from("trace")?),
                    source: &bpfman_model::ProgramSource::File(Some("source.o".into())),
                    license: "GPL",
                    created_at: "2026-10-04T12:00:00Z",
                    metadata: &BTreeMap::new(),
                },
            )?;
            let db = Connection::open(layout.database_path())?;

            test(&w, &db, &mut reader)
        },
    )??;

    Ok(())
}

fn pending(
    writer: &RuntimeWriter<'_>,
) -> std::result::Result<
    (bpfman_model::StoredLink, bpfman_store_sqlite::LinkReceipt),
    bpfman_store::Error,
> {
    Backend.create_pending_tracepoint(
        writer,
        PendingTracepoint {
            program_id: NonZeroU32::new(42).expect("id"),
            target: &"sched/sched_switch".parse().expect("target"),
            metadata: &BTreeMap::new(),
            created_at: "2026-10-04T12:00:00Z",
        },
    )
}

#[test]
fn registry_details_and_pin_path_publish_as_one_transaction() -> Result {
    scope(|writer, db, reader| {
        db.execute_batch("CREATE TRIGGER reject_pin BEFORE UPDATE OF pin_path ON links BEGIN SELECT RAISE(ABORT, 'injected publication failure'); END")?;

        assert!(pending(writer).is_err());
        assert!(reader.read_links()?.is_empty());
        let details: u32 =
            db.query_row("SELECT count(*) FROM link_tracepoint_details", [], |row| {
                row.get(0)
            })?;

        assert_eq!(details, 0);

        db.execute_batch("DROP TRIGGER reject_pin")?;
        let (record, receipt) = pending(writer)?;
        db.execute_batch("CREATE TRIGGER reject_finalise BEFORE UPDATE OF kernel_link_id ON links BEGIN SELECT RAISE(ABORT, 'injected finalisation failure'); END")?;
        let failure = Backend
            .finalise_link(writer, receipt, NonZeroU32::new(5).expect("id"))
            .expect_err("finalisation rejected");

        assert_eq!(reader.read_links()?, [record]);

        db.execute_batch("DROP TRIGGER reject_finalise")?;
        let attached = Backend
            .finalise_link(writer, failure.remaining, NonZeroU32::new(5).expect("id"))
            .map_err(|e| e.cause)?;
        let (_, receipt) = Backend.observe_link(writer, attached.id)?.expect("present");
        db.execute_batch("CREATE TRIGGER reject_delete BEFORE DELETE ON links BEGIN SELECT RAISE(ABORT, 'injected deletion failure'); END")?;
        let failure = Backend
            .delete_link(writer, receipt)
            .expect_err("deletion rejected");

        assert_eq!(reader.read_links()?, [attached]);

        db.execute_batch("DROP TRIGGER reject_delete")?;
        Backend
            .delete_link(writer, failure.remaining)
            .map_err(|e| e.cause)?;
        let details: u32 =
            db.query_row("SELECT count(*) FROM link_tracepoint_details", [], |row| {
                row.get(0)
            })?;

        assert!(reader.read_links()?.is_empty());
        assert_eq!(details, 0);

        Ok(())
    })
}

#[test]
fn malformed_links_and_changed_evidence_are_refused() -> Result {
    for sql in [
        "UPDATE links SET kind='xdp'",
        "PRAGMA foreign_keys=OFF; UPDATE links SET kernel_prog_id=999",
        "UPDATE managed_programs SET program_type='xdp'",
        "UPDATE links SET kernel_link_id=0",
        "UPDATE links SET kernel_link_id=4294967296",
        "UPDATE links SET pin_path=NULL",
        "UPDATE links SET metadata_json='{\"owner\":3}'",
        "UPDATE links SET created_at='not a timestamp'",
        "DELETE FROM link_tracepoint_details",
        "UPDATE link_tracepoint_details SET tp_group='..'",
        "UPDATE goose_db_version SET version_id=99",
    ] {
        scope(|writer, db, reader| {
            let (record, receipt) = pending(writer)?;
            db.execute_batch(sql)?;

            assert!(reader.read_links().is_err(), "{sql}");
            assert!(Backend.observe_link(writer, record.id).is_err(), "{sql}");
            assert!(Backend.delete_link(writer, receipt).is_err(), "{sql}");

            Ok(())
        })?;
    }

    scope(|writer, db, reader| {
        let (record, receipt) = pending(writer)?;
        db.execute_batch("UPDATE links SET pin_path='/outside'; UPDATE link_tracepoint_details SET tp_name='sched_wakeup'")?;

        assert!(Backend.observe_link(writer, record.id).is_err());
        assert!(Backend.delete_link(writer, receipt).is_err());
        assert_eq!(reader.read_links()?.len(), 1);

        Ok(())
    })
}

#[test]
fn non_tracepoint_program_cannot_acquire_tracepoint_intent() -> Result {
    scope(|writer, db, reader| {
        db.execute_batch("UPDATE managed_programs SET program_type='xdp'")?;

        assert!(pending(writer).is_err());
        assert!(reader.read_links()?.is_empty());
        assert!(
            Backend
                .observe_link(writer, NonZeroU64::new(u64::MAX).expect("id"))?
                .is_none()
        );

        Ok(())
    })
}

#[test]
fn lsm_details_publish_atomically_and_validate_the_load_bound_hook() -> Result {
    scope(|writer, db, reader| {
        db.execute_batch("UPDATE managed_programs SET program_type='lsm', attach_func='file_open';
            CREATE TRIGGER reject_lsm BEFORE INSERT ON link_lsm_details BEGIN SELECT RAISE(ABORT, 'injected detail failure'); END")?;
        let target = bpfman_model::TracingLink {
            function: "file_open".try_into()?,
            kind: bpfman_model::TracingKind::Lsm,
        };
        let pending = || {
            Backend.create_pending_tracing(
                writer,
                bpfman_store::PendingTracing {
                    program_id: NonZeroU32::new(42).expect("id"),
                    target: &target,
                    metadata: &BTreeMap::new(),
                    created_at: "2026-10-16T12:00:00Z",
                },
            )
        };
        assert!(pending().is_err());
        assert!(reader.read_links()?.is_empty());
        assert_eq!(
            db.query_row("SELECT count(*) FROM links", [], |row| row.get::<_, u32>(0))?,
            0
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM link_lsm_details", [], |row| row
                .get::<_, u32>(0))?,
            0
        );
        db.execute_batch("DROP TRIGGER reject_lsm")?;
        let (link, _) = pending()?;
        assert_eq!(
            reader.read_links()?[0].details,
            bpfman_model::LinkDetails::Tracing(target)
        );
        db.execute_batch("UPDATE link_lsm_details SET hook_name='file_permission'")?;
        assert!(
            reader.read_links().is_err(),
            "hook must match the load target"
        );
        db.execute_batch("UPDATE link_lsm_details SET hook_name='file_open'; UPDATE managed_programs SET program_type='fentry'")?;
        assert!(reader.read_links().is_err(), "program family must match");
        db.execute_batch(
            "UPDATE managed_programs SET program_type='lsm'; DELETE FROM link_lsm_details",
        )?;
        assert!(
            reader.read_links().is_err(),
            "missing details must not become an empty intent"
        );
        assert_eq!(link.program_id.get(), 42);
        Ok(())
    })
}
