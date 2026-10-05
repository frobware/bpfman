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
                globals: &BTreeMap::from([
                    ("weight".into(), vec![0, 1, 255]),
                    ("empty".into(), vec![]),
                ]),
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

fn exercise<S: OpenStore + CommitLoad + UnloadStore>(backend: S) {
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().join("one")).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    let other = RuntimeDirectory::open_or_create(
        RuntimeLayout::try_from(temporary.path().join("two")).expect("layout"),
    )
    .expect("runtime");
    let id = NonZeroU32::new(42).expect("id");
    let store = bpfman_runtime::ActiveStore::open(backend, &layout, Duration::from_secs(1))
        .expect("active store");

    let (program, maps) = with_writer(&runtime, |w| {
        let mut reader = store.open(w).expect("open");

        assert!(reader.read_records().expect("records").is_empty());
        commit(&store, w, 42);

        // Previously opened readers observe a complete, newly committed record.

        let records = reader.read_records().expect("records");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].id, id);
        assert_eq!(
            records[0].globals,
            BTreeMap::from([
                ("weight".into(), Some(vec![0, 1, 255])),
                ("empty".into(), Some(vec![])),
            ])
        );
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

fn batch_contract<S: OpenStore + CommitLoad>(backend: S) {
    let temporary = tempfile::tempdir().expect("tempdir");
    let layout = RuntimeLayout::try_from(temporary.path().join("runtime")).expect("layout");
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    let store =
        bpfman_runtime::ActiveStore::open(backend, &layout, Duration::from_secs(1)).expect("store");
    let name = Symbol::try_from("trace").expect("symbol");
    let metadata = BTreeMap::new();
    let globals = BTreeMap::new();
    let record = |id| TracepointRecord {
        id: NonZeroU32::new(id).expect("id"),
        name: &name,
        source: "/source.o",
        license: "GPL",
        created_at: "2026-10-05T00:00:00Z",
        metadata: &metadata,
        globals: &globals,
    };

    with_writer(&runtime, |writer| {
        let mut reader = store.open(writer).expect("reader");
        store.commit_tracepoints(writer, &[]).expect("empty batch");
        assert!(reader.read_records().expect("empty").is_empty());
        commit(&store, writer, 7);
        let before = reader.read_records().expect("baseline");

        // A later existing ID and an intra-batch duplicate must both roll back
        // the earlier program AND its private map set.
        for ids in [[42, 7], [42, 42]] {
            assert!(store.commit_tracepoints(writer, &ids.map(record)).is_err());
            assert_eq!(reader.read_records().expect("unchanged"), before);
        }

        store
            .commit_tracepoints(writer, &[record(42), record(43), record(44)])
            .expect("atomic batch");
        let records = reader.read_records().expect("fresh snapshot");
        assert_eq!(records.len(), 4);
        for id in [42, 43, 44] {
            let member = records.iter().find(|r| r.id.get() == id).expect("member");
            assert_eq!(member.map_set, member.id);
        }
    });
}

#[test]
fn sqlite_atomic_batch_contract() {
    batch_contract(bpfman_store_sqlite::Backend);
}

#[test]
fn json_atomic_batch_contract() {
    batch_contract(bpfman_store_json::Backend);
}
