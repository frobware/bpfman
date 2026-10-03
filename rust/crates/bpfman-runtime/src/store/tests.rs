#![allow(clippy::expect_used)]

use super::testing::Memory;
use crate::{ActiveStore, Bpfman};
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
    runtime.layout().clone()
}

fn setup() -> (tempfile::TempDir, RuntimeDirectory, Memory, Bpfman<Memory>) {
    let temp = tempfile::tempdir().expect("temp");
    let runtime = RuntimeDirectory::open_or_create(
        RuntimeLayout::try_from(temp.path().to_path_buf()).expect("layout"),
    )
    .expect("runtime");
    let store = scope(&runtime, Memory::new);
    let active = ActiveStore::open(store.clone(), runtime.layout(), TIMEOUT).expect("active store");
    let bpfman = Bpfman::new(active, Duration::from_millis(25));
    (temp, runtime, store, bpfman)
}

fn calls(store: &Memory) -> Vec<&'static str> {
    // Exclude the initialization read; assertions below concern operation effects.
    store.calls().into_iter().skip(1).collect()
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
    let (_temp, runtime, store, bpfman) = setup();
    scope(&runtime, |w| seed(&store, w));
    let programs = bpfman.list(&Default::default()).expect("list");

    assert_eq!(programs.len(), 1);
    assert_eq!(programs[0].id(), id());
    assert_eq!(programs[0].application(), "test");

    // Filter out the record so JSON listing needs no live kernel access.

    let filter = bpfman_core::ProgramFilter {
        types: vec![],
        application: Some("absent".into()),
    };

    assert!(bpfman.list_entries(&filter).expect("list").is_empty());

    let err = bpfman
        .get(NonZeroU32::new(99).expect("id"))
        .expect_err("missing");

    assert_eq!(err.kind(), crate::ObservationErrorKind::NotFound);
    assert_eq!(
        calls(&store),
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
        let (_temp, runtime, store, bpfman) = setup();
        store.fail(fault, kind);
        let err = bpfman.list(&Default::default()).expect_err("injected");

        assert_eq!(err.kind(), expected);
        assert_eq!(
            calls(&store).iter().filter(|call| **call == fault).count(),
            1
        );
        assert_eq!(store.residue(), (false, false));
        assert!(!layout(&runtime).database_path().exists());
    }
}

#[test]
fn full_read_failure_is_not_missing_or_an_empty_list() {
    let (_temp, runtime, store, bpfman) = setup();
    store.fail("records", ErrorKind::Unavailable);
    let get = bpfman
        .get(id())
        .expect_err("failed read must not become not found");
    let list = bpfman
        .list_entries(&Default::default())
        .expect_err("failed read must not become an empty list");

    assert_eq!(get.kind(), crate::ObservationErrorKind::Unavailable);
    assert_eq!(list.kind(), crate::ObservationErrorKind::Unavailable);
    assert_eq!(calls(&store), ["open", "records", "open", "records"]);
    assert!(!layout(&runtime).database_path().exists());
}

#[test]
fn retained_backend_receipts_survive_failed_unload_and_explicit_retry() {
    let (_temp, runtime, store, bpfman) = setup();
    scope(&runtime, |w| seed(&store, w));
    store.fail("delete program", ErrorKind::Unavailable);
    let error = bpfman.unload(id()).expect_err("record failure");

    assert_eq!(store.residue(), (true, true));
    assert_eq!(calls(&store), ["commit", "observe", "delete program"]);

    let error = bpfman.retry_unload(error).expect_err("unchanged fault");

    assert_eq!(store.residue(), (true, true));
    assert_eq!(error.report().expect("report").attempts().len(), 2);
    store.clear_faults();
    store.fail("delete map set", ErrorKind::Unavailable);

    let report = bpfman.retry_unload(error).expect("GC warning");

    assert_eq!(store.residue(), (false, true));
    assert_eq!(report.unresolved(), 1);
    store.clear_faults();

    let (_other_temp, _other, _, other) = setup();
    let other_store = scope(&runtime, Memory::new);
    let active = ActiveStore::open(other_store, runtime.layout(), TIMEOUT).expect("other store");
    let other_instance = Bpfman::new(active, TIMEOUT);
    let report = other
        .retry_unload_cleanup(report)
        .expect("wrong root GC warning");

    assert_eq!(report.unresolved(), 1);
    store.clear_faults();

    // Same backend type does not authorize a different backend instance.

    let report = other_instance
        .retry_unload_cleanup(report)
        .expect("foreign backend warning");

    assert_eq!(report.unresolved(), 1);
    assert_eq!(store.residue(), (false, true));

    let report = bpfman.retry_unload_cleanup(report).expect("clean");

    assert_eq!(report.unresolved(), 0);
    assert_eq!(store.residue(), (false, false));
    assert_eq!(calls(&store).iter().filter(|c| **c == "observe").count(), 1);
    assert_eq!(
        calls(&store)
            .iter()
            .filter(|c| **c == "delete program")
            .count(),
        3
    );
    assert!(!layout(&runtime).database_path().exists());
}

#[test]
fn unload_and_retry_keep_receipts_when_writer_acquisition_times_out() {
    for fault in ["delete program", "delete map set"] {
        let (_temp, runtime, store, bpfman) = setup();
        scope(&runtime, |w| seed(&store, w));

        // Admission failure must not perform even teardown observations.
        let blocked = scope(&runtime, |_| {
            std::thread::scope(|threads| {
                threads
                    .spawn(|| bpfman.unload(id()))
                    .join()
                    .expect("worker")
            })
        })
        .expect_err("writer held");

        assert!(blocked.report().is_none());
        assert_eq!(calls(&store), ["commit"]);
        assert_eq!(store.residue(), (true, true));

        store.fail(fault, ErrorKind::Unavailable);
        let result = bpfman.unload(id());
        let (attempts, remaining) = match &result {
            Ok(report) => (report.attempts().len(), report.unresolved()),
            Err(error) => {
                let report = error.report().expect("teardown started");
                (report.attempts().len(), report.unresolved())
            }
        };
        let before = calls(&store);
        let blocked = scope(&runtime, |_| {
            std::thread::scope(|threads| {
                threads
                    .spawn(|| match result {
                        Ok(report) => bpfman.retry_unload_cleanup(report),
                        Err(error) => bpfman.retry_unload(error),
                    })
                    .join()
                    .expect("worker")
            })
        })
        .expect_err("retry must acquire writer authority");

        let report = blocked.report().expect("receipts retained after timeout");
        assert_eq!(report.attempts().len(), attempts);
        assert_eq!(report.unresolved(), remaining);
        assert_eq!(calls(&store), before);
        assert_eq!(blocked.kind(), crate::UnloadErrorKind::Unavailable);

        store.clear_faults();
        let report = bpfman
            .retry_unload(blocked)
            .expect("retry after lock release");

        assert_eq!(report.unresolved(), 0);
        assert_eq!(store.residue(), (false, false));
        assert_eq!(
            calls(&store)
                .iter()
                .filter(|call| **call == "observe")
                .count(),
            1
        );
    }
}

#[test]
fn operations_retain_the_adopted_runtime_when_its_path_is_replaced() {
    let temp = tempfile::tempdir().expect("tempdir");
    let original = temp.path().join("runtime");
    let runtime = RuntimeDirectory::open_or_create(
        RuntimeLayout::try_from(original.clone()).expect("layout"),
    )
    .expect("runtime");
    let store = scope(&runtime, Memory::new);
    scope(&runtime, |writer| seed(&store, writer));
    let active = ActiveStore::open(store.clone(), runtime.layout(), TIMEOUT).expect("active");
    let bpfman = Bpfman::new(active, TIMEOUT);

    std::fs::rename(&original, temp.path().join("adopted")).expect("move root");
    std::fs::create_dir(&original).expect("replacement");

    assert_eq!(
        bpfman.list(&Default::default()).expect("list")[0].id(),
        id()
    );
    assert_eq!(bpfman.unload(id()).expect("unload").unresolved(), 0);
    assert_eq!(store.residue(), (false, false));
    assert_eq!(std::fs::read_dir(original).expect("replacement").count(), 0);
}
