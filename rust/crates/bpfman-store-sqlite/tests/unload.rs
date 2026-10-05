//! Conditional deletion and scope checks against the actual Go SQLite schema.
#![allow(clippy::expect_used)]

use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_model::Symbol;
use bpfman_store_sqlite::{
    LoadRecord, create_if_missing, delete_unloaded_program, delete_unused_map_set, observe_unload,
    persist_program,
};
use rusqlite::Connection;
use std::{collections::BTreeMap, num::NonZeroU32, time::Duration};

type Result = std::result::Result<(), Box<dyn std::error::Error>>;

fn id() -> NonZeroU32 {
    NonZeroU32::new(42).expect("fixture")
}

fn seed(writer: &RuntimeWriter<'_>, id: NonZeroU32) -> Result {
    persist_program(
        writer,
        LoadRecord {
            globals: &Default::default(),
            id,
            spec: &bpfman_model::ProgramSpec::Tracepoint(Symbol::try_from("trace")?),
            source: "source.o",
            license: "GPL",
            created_at: "2026-10-03T12:00:00Z",
            metadata: &BTreeMap::new(),
        },
    )?;

    Ok(())
}

fn scope(test: impl FnOnce(&RuntimeWriter<'_>, &Connection) -> Result) -> Result {
    let temp = tempfile::tempdir()?;
    let layout = RuntimeLayout::try_from(temp.path().to_owned())?;
    let runtime = RuntimeDirectory::open_or_create(layout.clone())?;
    runtime.with_writer(
        AcquireOptions {
            timeout: Duration::from_secs(1),
            cancelled: None,
        },
        |writer| -> Result {
            create_if_missing(&writer)?;
            seed(&writer, id())?;
            let db = Connection::open(layout.database_path())?;
            test(&writer, &db)
        },
    )??;

    Ok(())
}

fn counts(db: &Connection) -> (usize, usize) {
    db.query_row(
        "SELECT (SELECT count(*) FROM managed_programs), (SELECT count(*) FROM map_sets)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .expect("counts")
}

#[test]
fn preflight_rejects_every_unsupported_relationship_and_noncanonical_path() -> Result {
    for sql in [
        "UPDATE managed_programs SET program_type='tc'",
        "INSERT INTO shared_map_pins VALUES ('shared',42)",
        "INSERT INTO managed_programs(program_id,program_name,program_type,object_path,pin_path,map_set_id,created_at) VALUES (99,'borrower','tracepoint','unused','unused',42,'now')",
        "INSERT INTO map_sets VALUES(99,'unused','now'); UPDATE managed_programs SET map_set_id=99",
        "UPDATE managed_programs SET pin_path='/'",
        "UPDATE managed_programs SET object_path='/tmp/outside'",
        "UPDATE map_sets SET pin_path='/tmp/outside'",
        "PRAGMA foreign_keys=OFF; DELETE FROM map_sets",
    ] {
        scope(|writer, db| {
            db.execute_batch(sql)?;
            let before = counts(db);

            assert!(observe_unload(writer, id()).is_err(), "{sql}");
            assert_eq!(counts(db), before);

            Ok(())
        })?;
    }

    Ok(())
}

#[test]
fn program_and_map_set_deletion_are_separate_atomic_effects() -> Result {
    scope(|writer, db| {
        seed(writer, NonZeroU32::new(7).expect("fixture"))?;
        let (record, map_set) = observe_unload(writer, id())?.expect("record").into_parts();
        let failure = delete_unused_map_set(writer, map_set).expect_err("still in use");

        assert_eq!(counts(db), (2, 2));
        db.execute_batch("CREATE TRIGGER reject_delete BEFORE DELETE ON managed_programs BEGIN SELECT RAISE(ABORT,'injected record deletion'); END")?;

        let record = delete_unloaded_program(writer, record)
            .expect_err("trigger")
            .remaining;

        assert_eq!(counts(db), (2, 2));
        db.execute_batch("DROP TRIGGER reject_delete")?;
        delete_unloaded_program(writer, record).map_err(|f| f.cause)?;
        assert_eq!(counts(db), (1, 2));
        db.execute_batch("CREATE TRIGGER reject_map_set BEFORE DELETE ON map_sets BEGIN SELECT RAISE(ABORT,'injected map-set deletion'); END")?;

        let map_set = delete_unused_map_set(writer, failure.remaining)
            .expect_err("trigger")
            .remaining;

        assert_eq!(counts(db), (1, 2));
        db.execute_batch("DROP TRIGGER reject_map_set")?;
        delete_unused_map_set(writer, map_set).map_err(|f| f.cause)?;
        assert_eq!(counts(db), (1, 1));
        assert!(observe_unload(writer, id())?.is_none());
        assert!(observe_unload(writer, NonZeroU32::new(7).expect("fixture"))?.is_some());

        Ok(())
    })
}

#[test]
fn changed_records_are_never_deleted_using_old_evidence() -> Result {
    for sql in [
        "UPDATE managed_programs SET created_at='replaced'",
        "UPDATE managed_programs SET pin_path='/outside'",
        "INSERT INTO links(kind,kernel_prog_id,created_at) VALUES ('tracepoint',42,'now')",
        "INSERT INTO shared_map_pins VALUES ('shared',42)",
    ] {
        scope(|writer, db| {
            let (record, _) = observe_unload(writer, id())?.expect("record").into_parts();
            db.execute_batch(sql)?;

            assert!(delete_unloaded_program(writer, record).is_err());
            assert_eq!(counts(db), (1, 1));

            Ok(())
        })?;
    }

    Ok(())
}

#[test]
fn changed_map_set_identity_or_new_user_blocks_deletion() -> Result {
    for sql in [
        "UPDATE map_sets SET created_at='replaced'",
        "UPDATE map_sets SET pin_path='/outside'",
        "INSERT INTO managed_programs(program_id,program_name,program_type,object_path,pin_path,map_set_id,created_at) VALUES(99,'other','tracepoint','unused','unused',42,'now')",
    ] {
        scope(|writer, db| {
            let (record, map_set) = observe_unload(writer, id())?.expect("record").into_parts();
            delete_unloaded_program(writer, record).map_err(|f| f.cause)?;
            db.execute_batch(sql)?;

            assert!(delete_unused_map_set(writer, map_set).is_err());
            assert_eq!(counts(db).1, 1);

            Ok(())
        })?;
    }

    Ok(())
}

#[test]
fn ignored_delete_is_not_reported_as_success() -> Result {
    scope(|writer, db| {
        let (record, _) = observe_unload(writer, id())?.expect("record").into_parts();
        db.execute_batch("CREATE TRIGGER ignore_delete BEFORE DELETE ON managed_programs BEGIN SELECT RAISE(IGNORE); END")?;

        assert!(delete_unloaded_program(writer, record).is_err());
        assert_eq!(counts(db), (1, 1));

        Ok(())
    })
}

#[test]
fn store_receipts_require_the_original_opened_runtime() -> Result {
    scope(|writer, db| {
        let other = tempfile::tempdir()?;
        let layout = RuntimeLayout::try_from(other.path().to_owned())?;
        let runtime = RuntimeDirectory::open_or_create(layout)?;
        let (record, map_set) = observe_unload(writer, id())?.expect("record").into_parts();
        let record = runtime.with_writer(
            AcquireOptions {
                timeout: Duration::from_secs(1),
                cancelled: None,
            },
            |other_writer| -> std::result::Result<_, Box<dyn std::error::Error>> {
                create_if_missing(&other_writer)?;
                seed(&other_writer, id())?;

                Ok(delete_unloaded_program(&other_writer, record)
                    .expect_err("wrong runtime")
                    .remaining)
            },
        )??;

        assert_eq!(counts(db), (1, 1));
        delete_unloaded_program(writer, record).map_err(|f| f.cause)?;

        let map_set = runtime.with_writer(
            AcquireOptions {
                timeout: Duration::from_secs(1),
                cancelled: None,
            },
            |other_writer| {
                delete_unused_map_set(&other_writer, map_set)
                    .expect_err("wrong runtime")
                    .remaining
            },
        )?;

        assert_eq!(counts(db), (0, 1));
        delete_unused_map_set(writer, map_set).map_err(|f| f.cause)?;
        assert_eq!(counts(db), (0, 0));

        let other_db =
            Connection::open(RuntimeLayout::try_from(other.path().to_owned())?.database_path())?;

        assert_eq!(counts(&other_db), (1, 1));

        Ok(())
    })
}

#[test]
fn linked_preflight_receipt_requires_links_removed_before_deletion() -> Result {
    scope(|writer, db| {
        db.execute_batch(
            "INSERT INTO links(kind,kernel_prog_id,created_at) VALUES ('tracepoint',42,'now')",
        )?;
        let (record, _) = observe_unload(writer, id())?
            .expect("linked program")
            .into_parts();
        let failure = delete_unloaded_program(writer, record).expect_err("link prerequisite");
        assert_eq!(counts(db), (1, 1));

        db.execute_batch("DELETE FROM links WHERE kernel_prog_id=42")?;
        delete_unloaded_program(writer, failure.remaining).map_err(|f| f.cause)?;
        assert_eq!(counts(db), (0, 1));

        Ok(())
    })
}
