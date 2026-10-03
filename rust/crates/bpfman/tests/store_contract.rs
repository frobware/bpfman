//! Shared persistence contracts. Setup varies; operations and assertions do not.
#![allow(clippy::expect_used)]

use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_model::Symbol;
use bpfman_store::{CommitLoad, OpenStore, ProgramReader, TracepointRecord, UnloadStore};
use std::{collections::BTreeMap, num::NonZeroU32, time::Duration};

fn with_writer<T>(runtime: &RuntimeDirectory, work: impl FnOnce(&RuntimeWriter<'_>) -> T) -> T {
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

fn commit<S: CommitLoad>(store: &S, writer: &RuntimeWriter<'_>, raw: u32) {
    store
        .commit_tracepoint(
            writer,
            TracepointRecord {
                id: NonZeroU32::new(raw).expect("id"),
                name: &Symbol::try_from("trace").expect("symbol"),
                source: "/source.o",
                license: "GPL",
                created_at: "2026-10-03T12:00:00Z",
                metadata: &BTreeMap::from([("bpfman.io/application".into(), "contract".into())]),
            },
        )
        .expect("commit");
}

fn exercise<S: OpenStore + CommitLoad + UnloadStore>(store: S) {
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().join("one")).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    let other = RuntimeDirectory::open_or_create(
        RuntimeLayout::try_from(temporary.path().join("two")).expect("layout"),
    )
    .expect("runtime");
    let id = NonZeroU32::new(42).expect("id");

    let (program, maps) = with_writer(&runtime, |w| {
        let mut reader = store.open(w).expect("open");
        assert!(reader.read_records().expect("records").is_empty());
        commit(&store, w, 42);

        // Previously opened readers observe a complete, newly committed record.
        let records = reader.read_records().expect("records");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, id);
        assert_eq!(
            records[0].object_path,
            layout.bytecode_path(id).to_str().expect("path")
        );
        let summaries = reader.read_programs().expect("summaries");
        assert_eq!(summaries[0].application(), "contract");

        let (program, maps) = store
            .observe_unload(w, id)
            .expect("observe")
            .expect("present");
        // An unrelated commit must not invalidate this program's receipts.
        commit(&store, w, 7);
        let error = store
            .delete_map_set(w, maps)
            .expect_err("map set still in use");
        (program, error.remaining)
    });

    let program = with_writer(&other, |w| {
        store
            .delete_program(w, program)
            .expect_err("wrong runtime")
            .remaining
    });

    with_writer(&runtime, |w| {
        store
            .delete_program(w, program)
            .map_err(|e| e.cause)
            .expect("delete original");
        store
            .delete_map_set(w, maps)
            .map_err(|e| e.cause)
            .expect("delete unused map set");
        let mut reader = store.open(w).expect("reopen");
        let records = reader.read_records().expect("records");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id.get(), 7);
        assert!(
            store
                .observe_unload(w, id)
                .expect("observe absent")
                .is_none()
        );
    });
}

#[test]
fn sqlite_contract() {
    exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn json_contract() {
    exercise(bpfman_store_json::Backend);
}
