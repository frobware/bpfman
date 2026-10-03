#![allow(clippy::expect_used)]

use super::testing::Memory;
use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_store::{CommitLoad, ErrorKind, TracepointRecord};
use std::{collections::BTreeMap, num::NonZeroU32, time::Duration};

const TIMEOUT: Duration = Duration::from_secs(1);

fn scope<T>(runtime: &RuntimeDirectory, run: impl FnOnce(&RuntimeWriter<'_>) -> T) -> T {
    runtime
        .with_writer(
            AcquireOptions {
                timeout: TIMEOUT,
                cancelled: None,
            },
            |w| run(&w),
        )
        .expect("writer")
}

fn layout(runtime: &RuntimeDirectory) -> RuntimeLayout {
    scope(runtime, |writer| writer.layout().clone())
}

fn setup() -> (tempfile::TempDir, RuntimeDirectory, Memory) {
    let temp = tempfile::tempdir().expect("temp");
    let runtime = RuntimeDirectory::open_or_create(
        RuntimeLayout::try_from(temp.path().to_path_buf()).expect("layout"),
    )
    .expect("runtime");
    let store = scope(&runtime, Memory::new);
    (temp, runtime, store)
}

fn id() -> NonZeroU32 {
    NonZeroU32::new(42).expect("id")
}

fn seed(store: &Memory, writer: &RuntimeWriter<'_>) {
    store
        .commit_tracepoint(
            writer,
            TracepointRecord {
                id: id(),
                name: &bpfman_model::Symbol::try_from("trace").expect("symbol"),
                source: "input.o",
                license: "GPL",
                created_at: "2026-10-03T00:00:00Z",
                metadata: &BTreeMap::from([("bpfman.io/application".into(), "test".into())]),
            },
        )
        .expect("commit");
}

#[test]
fn runtime_reads_an_independent_store_without_creating_a_database() {
    let (_temp, runtime, store) = setup();
    scope(&runtime, |w| seed(&store, w));
    let programs = crate::list_programs(
        &store,
        &layout(&runtime),
        &bpfman_core::ProgramFilter::default(),
        TIMEOUT,
    )
    .expect("list");

    assert_eq!(programs.len(), 1);
    assert_eq!(programs[0].id(), id());
    assert_eq!(programs[0].application(), "test");

    // Filter out the record so JSON listing needs no live kernel access.

    let filter = bpfman_core::ProgramFilter {
        types: vec![],
        application: Some("absent".into()),
    };

    assert!(
        crate::list_program_entries(&store, &layout(&runtime), &filter, TIMEOUT)
            .expect("list")
            .is_empty()
    );

    let err = crate::get_program(
        &store,
        &layout(&runtime),
        NonZeroU32::new(99).expect("id"),
        TIMEOUT,
    )
    .expect_err("missing");

    assert_eq!(err.kind(), crate::ObservationErrorKind::NotFound);
    assert_eq!(
        store.calls(),
        [
            "commit",
            "open",
            "summaries",
            "open",
            "records",
            "open",
            "records"
        ]
    );
    assert!(!layout(&runtime).database_path().exists());
}

#[test]
fn store_errors_keep_their_category_and_are_not_retried() {
    for (fault, kind, expected) in [
        (
            "open",
            ErrorKind::IncompatibleState,
            crate::ErrorKind::IncompatibleState,
        ),
        (
            "summaries",
            ErrorKind::InvalidData,
            crate::ErrorKind::InvalidState,
        ),
        (
            "open",
            ErrorKind::Unavailable,
            crate::ErrorKind::Unavailable,
        ),
    ] {
        let (_temp, runtime, store) = setup();
        store.fail(fault, kind);
        let err = crate::list_programs(&store, &layout(&runtime), &Default::default(), TIMEOUT)
            .expect_err("injected");

        assert_eq!(err.kind(), expected);
        assert_eq!(
            store.calls().iter().filter(|call| **call == fault).count(),
            1
        );
        assert_eq!(store.residue(), (false, false));
        assert!(!layout(&runtime).database_path().exists());
    }
}

#[test]
fn full_read_failure_is_not_missing_or_an_empty_list() {
    let (_temp, runtime, store) = setup();
    store.fail("records", ErrorKind::Unavailable);
    let get = crate::get_program(&store, &layout(&runtime), id(), TIMEOUT)
        .expect_err("failed read must not become not found");
    let list = crate::list_program_entries(&store, &layout(&runtime), &Default::default(), TIMEOUT)
        .expect_err("failed read must not become an empty list");

    assert_eq!(get.kind(), crate::ObservationErrorKind::Unavailable);
    assert_eq!(list.kind(), crate::ObservationErrorKind::Unavailable);
    assert_eq!(store.calls(), ["open", "records", "open", "records"]);
    assert!(!layout(&runtime).database_path().exists());
}

#[test]
fn retained_backend_receipts_survive_failed_unload_and_explicit_retry() {
    let (_temp, runtime, store) = setup();
    scope(&runtime, |w| seed(&store, w));
    store.fail("delete program", ErrorKind::Unavailable);
    let error = crate::unload_tracepoint(&store, &layout(&runtime), id(), TIMEOUT)
        .expect_err("record failure");

    assert_eq!(store.residue(), (true, true));
    assert_eq!(store.calls(), ["commit", "observe", "delete program"]);

    let error = scope(&runtime, |w| error.retry(&store, w)).expect_err("unchanged fault");

    assert_eq!(store.residue(), (true, true));
    assert_eq!(error.report().expect("report").attempts().len(), 2);
    store.clear_faults();
    store.fail("delete map set", ErrorKind::Unavailable);

    let report = scope(&runtime, |w| error.retry(&store, w)).expect("GC warning");

    assert_eq!(store.residue(), (false, true));
    assert_eq!(report.unresolved(), 1);
    store.clear_faults();

    let (_other_temp, other, _) = setup();
    let other_store = scope(&runtime, Memory::new);
    let report = scope(&other, |w| report.retry_cleanup(&store, w)).expect("wrong root GC warning");

    assert_eq!(report.unresolved(), 1);
    store.clear_faults();

    // Same backend type does not authorize a different backend instance.

    let report = scope(&runtime, |w| report.retry_cleanup(&other_store, w))
        .expect("foreign backend warning");

    assert_eq!(report.unresolved(), 1);
    assert_eq!(store.residue(), (false, true));

    let report = scope(&runtime, |w| report.retry_cleanup(&store, w)).expect("clean");

    assert_eq!(report.unresolved(), 0);
    assert_eq!(store.residue(), (false, false));
    assert_eq!(store.calls().iter().filter(|c| **c == "observe").count(), 1);
    assert_eq!(
        store
            .calls()
            .iter()
            .filter(|c| **c == "delete program")
            .count(),
        3
    );
    assert!(!layout(&runtime).database_path().exists());
}
