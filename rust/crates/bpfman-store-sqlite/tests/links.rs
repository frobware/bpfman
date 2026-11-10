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
                    owner: "",
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
fn targeted_links_include_pending_tcx_peers_without_decoding_other_hooks() -> Result {
    scope(|writer, db, reader| {
        db.execute_batch(
            "UPDATE managed_programs SET program_type='tcx';
             INSERT INTO managed_programs(program_id,program_name,program_type,object_path,pin_path,map_set_id,created_at)
             SELECT 43,'peer',program_type,object_path,pin_path,map_set_id,created_at FROM managed_programs WHERE program_id=42;
             INSERT INTO managed_programs(program_id,program_name,program_type,object_path,pin_path,map_set_id,created_at)
             SELECT 44,'other',program_type,object_path,pin_path,map_set_id,created_at FROM managed_programs WHERE program_id=42;",
        )?;
        let mut ids = Vec::new();
        for (program, direction, priority) in [
            (43, bpfman_model::TcDirection::Ingress, 50),
            (44, bpfman_model::TcDirection::Egress, 0),
            (42, bpfman_model::TcDirection::Ingress, 50),
        ] {
            let target = bpfman_model::TcxLink {
                key: bpfman_model::TcKey {
                    nsid: NonZeroU64::MIN,
                    ifindex: NonZeroU32::MIN,
                    direction,
                },
                interface: "fake0".parse()?,
                netns: Default::default(),
                priority,
                position: 0,
            };
            let (link, _) = Backend.create_pending_tcx(
                writer,
                bpfman_store::PendingTcx {
                    program_id: NonZeroU32::new(program).expect("program"),
                    target: &target,
                    metadata: &BTreeMap::new(),
                    created_at: "2026-10-04T12:00:00Z",
                },
            )?;
            ids.push(link.id);
        }
        let program = NonZeroU32::new(42).expect("program");
        let all = reader.read_links()?;
        let selected = reader.read_links_by_program(program)?;
        assert_eq!(
            selected,
            all.into_iter()
                .filter(|link| link.program_id == program)
                .collect::<Vec<_>>()
        );
        assert_eq!(selected.len(), 1);
        assert!(
            matches!(&selected[0].details, bpfman_model::LinkDetails::Tcx(link) if link.position == 1)
        );
        assert_eq!(selected[0].state, bpfman_model::LinkState::Pending);

        db.execute(
            "UPDATE links SET metadata_json='broken' WHERE id=?1",
            [ids[1].get()],
        )?;
        assert!(reader.read_links().is_err());
        assert_eq!(reader.read_links_by_program(program)?, selected);
        db.execute(
            "UPDATE links SET metadata_json='broken' WHERE id=?1",
            [ids[0].get()],
        )?;
        assert!(
            reader.read_links_by_program(program).is_err(),
            "same-hook peer must be validated"
        );
        db.execute_batch("UPDATE links SET metadata_json='{}'")?;
        db.execute(
            "UPDATE link_tcx_details SET priority=60 WHERE id=?1",
            [ids[0].get()],
        )?;
        let fresh = reader.read_links_by_program(program)?;
        assert!(
            matches!(&fresh[0].details, bpfman_model::LinkDetails::Tcx(link) if link.position == 0)
        );
        assert!(
            reader
                .read_links_by_program(NonZeroU32::new(999).expect("program"))?
                .is_empty()
        );
        db.execute_batch("UPDATE goose_db_version SET version_id=99")?;
        assert!(reader.read_links_by_program(program).is_err());
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
