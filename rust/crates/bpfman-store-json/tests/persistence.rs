//! JSON-specific format and publication guarantees. Behaviour lives in the
//! shared store and outside-in lifecycle suites, run against both backends.
#![allow(clippy::expect_used)]

use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_model::Symbol;
use bpfman_store::{
    CommitLoad, ErrorKind, LinkReader, LinkStore, OpenStore, PendingTracepoint, ProgramReader,
    TracepointRecord, UnloadStore,
};
use bpfman_store_json::Backend;
use std::{collections::BTreeMap, fs, num::NonZeroU32, os::unix::fs::symlink, time::Duration};

fn writer<T>(runtime: &RuntimeDirectory, work: impl FnOnce(&RuntimeWriter<'_>) -> T) -> T {
    runtime
        .with_writer(
            AcquireOptions {
                timeout: Duration::from_secs(1),
                cancelled: None,
            },
            |w| work(&w),
        )
        .expect("writer")
}

fn commit(writer: &RuntimeWriter<'_>) -> Result<(), bpfman_store::Error> {
    Backend.commit_tracepoint(
        writer,
        TracepointRecord {
            id: NonZeroU32::new(42).expect("id"),
            name: &Symbol::try_from("trace").expect("symbol"),
            source: "/source.o",
            license: "GPL",
            created_at: "2026-10-03T12:00:00Z",
            metadata: &BTreeMap::new(),
        },
    )?;

    Ok(())
}

#[test]
fn malformed_and_future_snapshots_are_never_replaced() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    writer(&runtime, |w| {
        Backend.open(w).expect("create");
    });
    let pristine = fs::read(layout.database_path()).expect("snapshot");

    for (bytes, kind) in [
        (b"{".as_slice(), ErrorKind::InvalidData),
        (b"{\"version\":3}".as_slice(), ErrorKind::IncompatibleState),
        (b"SQLite format 3\0".as_slice(), ErrorKind::InvalidData),
    ] {
        fs::write(layout.database_path(), bytes).expect("fixture");
        writer(&runtime, |w| {
            let error = Backend.open(w).map(|_| ()).expect_err("reject state");

            assert_eq!(error.kind(), kind);
        });

        assert_eq!(fs::read(layout.database_path()).expect("unchanged"), bytes);
    }

    fs::write(layout.database_path(), &pristine).expect("restore");
    let mut reader = writer(&runtime, |w| Backend.open(w).expect("open"));
    fs::write(layout.database_path(), b"{\"version\":9}").expect("change version");

    assert_eq!(
        reader
            .read_records()
            .expect_err("revalidate version")
            .kind(),
        ErrorKind::IncompatibleState
    );
}

#[test]
fn publication_failure_never_commits_half_a_load() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");

    writer(&runtime, |w| {
        let mut reader = Backend.open(w).expect("create");
        let previous = fs::read(layout.database_path()).expect("snapshot");
        let outside = temporary.path().join("outside");
        fs::write(&outside, b"sentinel").expect("outside");
        let pending = temporary.path().join("db/store.next");
        symlink(&outside, &pending).expect("blocked publication");

        assert!(commit(w).is_err());
        assert!(reader.read_records().expect("records").is_empty());
        assert_eq!(
            fs::read(layout.database_path()).expect("snapshot"),
            previous
        );
        assert_eq!(fs::read(&outside).expect("sentinel"), b"sentinel");

        fs::rename(&pending, temporary.path().join("saved")).expect("remove obstruction");
        commit(w).expect("commit after obstruction removed");

        let published = fs::read(layout.database_path()).expect("published");
        let state: serde_json::Value = serde_json::from_slice(&published).expect("JSON");

        assert_eq!(state["programs"].as_array().expect("programs").len(), 1);
        assert_eq!(state["map_sets"].as_array().expect("maps").len(), 1);

        assert!(commit(w).is_err());
        assert_eq!(
            fs::read(layout.database_path()).expect("unchanged"),
            published
        );
    });
}

#[test]
fn invalid_relationships_are_refused_before_teardown() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    writer(&runtime, |w| {
        Backend.open(w).expect("create");
        commit(w).expect("commit");
    });
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(layout.database_path()).expect("read")).expect("JSON");

    for mutation in 0..4 {
        let mut state = original.clone();

        match mutation {
            0 => state["map_sets"] = serde_json::json!([]),
            1 => state["programs"][0]["created_at"] = serde_json::json!("invalid"),
            2 => state["programs"][0]["links"] = serde_json::json!([1]),
            _ => state["programs"]
                .as_array_mut()
                .expect("programs")
                .push(original["programs"][0].clone()),
        }

        let bytes = serde_json::to_vec(&state).expect("encode");
        fs::write(layout.database_path(), &bytes).expect("fixture");

        writer(&runtime, |w| {
            assert!(
                Backend
                    .observe_unload(w, NonZeroU32::new(42).expect("id"))
                    .is_err()
            );
        });

        assert_eq!(fs::read(layout.database_path()).expect("unchanged"), bytes);
    }
}

#[test]
fn receipts_reject_recreated_records_and_replaced_store_identity() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    let id = NonZeroU32::new(42).expect("id");

    writer(&runtime, |w| {
        Backend.open(w).expect("create");
        commit(w).expect("commit");

        let (stale_program, stale_maps) = Backend
            .observe_unload(w, id)
            .expect("observe")
            .expect("present");
        let (program, maps) = Backend
            .observe_unload(w, id)
            .expect("observe")
            .expect("present");
        Backend
            .delete_program(w, program)
            .map_err(|e| e.cause)
            .expect("delete");
        Backend
            .delete_map_set(w, maps)
            .map_err(|e| e.cause)
            .expect("delete map set");

        // Same ID, timestamp and contents, but a new generation owns the artifacts.
        commit(w).expect("recreate identical record");

        assert!(Backend.delete_program(w, stale_program).is_err());

        let (program, maps) = Backend
            .observe_unload(w, id)
            .expect("observe")
            .expect("present");
        Backend
            .delete_program(w, program)
            .map_err(|e| e.cause)
            .expect("delete new generation");

        assert!(Backend.delete_map_set(w, stale_maps).is_err());
        Backend
            .delete_map_set(w, maps)
            .map_err(|e| e.cause)
            .expect("delete new map generation");

        commit(w).expect("commit");

        let (program, _) = Backend
            .observe_unload(w, id)
            .expect("observe")
            .expect("present");
        fs::rename(layout.database_path(), temporary.path().join("old-store"))
            .expect("replace store");
        Backend.open(w).expect("create independent store");
        commit(w).expect("same record in new store");

        assert!(Backend.delete_program(w, program).is_err());
        assert_eq!(
            Backend
                .open(w)
                .expect("reopen")
                .read_records()
                .expect("records")
                .len(),
            1
        );
    });
}

#[test]
fn version_one_programs_remain_usable_without_implicit_upgrade() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");

    writer(&runtime, |w| {
        Backend.open(w).expect("create");
    });
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&fs::read(layout.database_path()).expect("read")).expect("JSON");
    legacy["version"] = serde_json::json!(1);
    let bytes = serde_json::to_vec(&legacy).expect("legacy snapshot");
    fs::write(layout.database_path(), &bytes).expect("version 1 fixture");

    writer(&runtime, |w| {
        let mut reader = Backend.open(w).expect("version 1 still opens");

        assert!(reader.read_links().expect("no links").is_empty());
        assert_eq!(fs::read(layout.database_path()).expect("unchanged"), bytes);

        commit(w).expect("legacy program load");
        let committed = fs::read(layout.database_path()).expect("committed");
        let target = "sched/sched_switch".parse().expect("target");
        let error = Backend
            .create_pending_tracepoint(
                w,
                PendingTracepoint {
                    program_id: NonZeroU32::new(42).expect("id"),
                    target: &target,
                    metadata: &BTreeMap::new(),
                    created_at: "2026-10-04T12:00:00Z",
                },
            )
            .map(|_| ())
            .expect_err("version 1 has no links");

        assert_eq!(error.kind(), ErrorKind::IncompatibleState);
        assert_eq!(
            fs::read(layout.database_path()).expect("no implicit upgrade"),
            committed
        );

        let (program, maps) = Backend
            .observe_unload(w, NonZeroU32::new(42).expect("id"))
            .expect("legacy unload")
            .expect("present");
        Backend
            .delete_program(w, program)
            .map_err(|e| e.cause)
            .expect("delete program");
        Backend
            .delete_map_set(w, maps)
            .map_err(|e| e.cause)
            .expect("delete map set");
        let state: serde_json::Value =
            serde_json::from_slice(&fs::read(layout.database_path()).expect("read")).expect("JSON");

        assert_eq!(state["version"], 1);
        assert!(state.get("links").is_none());
        assert!(state.get("next_link_id").is_none());
        assert!(reader.read_records().expect("empty").is_empty());
    });
}

#[test]
fn failed_link_publication_retains_previous_state_and_receipts() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().to_owned()).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    let outside = temporary.path().join("outside");
    fs::write(&outside, b"sentinel").expect("outside");
    let pending = temporary.path().join("db/store.next");
    let block = || symlink(&outside, &pending).expect("block publication");
    let unblock = |step: &str| fs::rename(&pending, temporary.path().join(step)).expect("unblock");

    writer(&runtime, |w| {
        let mut reader = Backend.open(w).expect("create");
        commit(w).expect("commit");
        let target = "sched/sched_switch".parse().expect("target");
        let metadata = BTreeMap::new();
        let request = || PendingTracepoint {
            program_id: NonZeroU32::new(42).expect("id"),
            target: &target,
            metadata: &metadata,
            created_at: "2026-10-04T12:00:00Z",
        };
        let previous = fs::read(layout.database_path()).expect("snapshot");
        block();

        assert!(Backend.create_pending_tracepoint(w, request()).is_err());
        assert_eq!(
            fs::read(layout.database_path()).expect("unchanged"),
            previous
        );
        assert!(reader.read_links().expect("no intent").is_empty());

        unblock("create-obstruction");
        let (record, receipt) = Backend
            .create_pending_tracepoint(w, request())
            .expect("intent");
        let previous = fs::read(layout.database_path()).expect("snapshot");
        block();
        let error = Backend
            .finalise_link(w, receipt, NonZeroU32::new(7).expect("kernel ID"))
            .expect_err("finalisation not committed");

        assert_eq!(
            fs::read(layout.database_path()).expect("unchanged"),
            previous
        );
        assert_eq!(reader.read_links().expect("still pending"), [record]);

        unblock("finalise-obstruction");
        let attached = Backend
            .finalise_link(w, error.remaining, NonZeroU32::new(7).expect("kernel ID"))
            .map_err(|e| e.cause)
            .expect("explicit finalisation attempt");
        let (_, receipt) = Backend
            .observe_link(w, attached.id)
            .expect("observe")
            .expect("present");
        let previous = fs::read(layout.database_path()).expect("snapshot");
        block();
        let error = Backend
            .delete_link(w, receipt)
            .expect_err("deletion not committed");

        assert_eq!(
            fs::read(layout.database_path()).expect("unchanged"),
            previous
        );
        assert_eq!(reader.read_links().expect("record retained"), [attached]);

        unblock("delete-obstruction");
        Backend
            .delete_link(w, error.remaining)
            .map_err(|e| e.cause)
            .expect("explicit deletion attempt");

        assert!(reader.read_links().expect("empty").is_empty());
        assert_eq!(fs::read(&outside).expect("sentinel"), b"sentinel");
    });
}
