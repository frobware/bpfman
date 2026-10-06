#![allow(clippy::expect_used)]

use super::testing::Faults;
use crate::{ActiveStore, Bpfman};
use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_store::{
    CommitLoad, ErrorKind, LinkReader, LinkStore, LoadRecord, OpenStore, UnloadStore,
};
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

trait TestStore:
    OpenStore<Reader: LinkReader>
    + CommitLoad
    + UnloadStore<ProgramReceipt: Send, MapSetReceipt: Send>
    + bpfman_store::XdpReplacementStore
    + LinkStore<LinkReceipt: Send>
    + Copy
    + Send
    + Sync
{
}
impl<S> TestStore for S where
    S: OpenStore<Reader: LinkReader>
        + CommitLoad
        + UnloadStore<ProgramReceipt: Send, MapSetReceipt: Send>
        + bpfman_store::XdpReplacementStore
        + LinkStore<LinkReceipt: Send>
        + Copy
        + Send
        + Sync
{
}

type Setup<S> = (
    tempfile::TempDir,
    RuntimeDirectory,
    Faults<S>,
    Bpfman<Faults<S>, bpfman_kernel_aya::Kernel>,
);

fn setup<S: TestStore>(backend: S) -> Setup<S> {
    let temp = tempfile::tempdir().expect("temp");
    let runtime = RuntimeDirectory::open_or_create(
        RuntimeLayout::try_from(temp.path().to_path_buf()).expect("layout"),
    )
    .expect("runtime");
    let store = Faults::new(backend);
    let active = ActiveStore::open(store.clone(), runtime.layout(), TIMEOUT).expect("active store");
    store.clear_calls();
    let bpfman = Bpfman::new(active, bpfman_kernel_aya::Kernel, Duration::from_millis(25));
    (temp, runtime, store, bpfman)
}

fn calls<S>(store: &Faults<S>) -> Vec<&'static str> {
    store.calls()
}

fn id() -> NonZeroU32 {
    NonZeroU32::new(42).expect("id")
}

fn seed<S: CommitLoad>(store: &Faults<S>, writer: &RuntimeWriter<'_>) {
    store
        .commit_program(
            writer,
            LoadRecord {
                globals: &Default::default(),
                id: id(),
                spec: &bpfman_model::ProgramSpec::Tracepoint(
                    bpfman_model::Symbol::try_from("trace").expect("symbol"),
                ),
                source: "input.o",
                license: "GPL",
                created_at: "2026-10-03T00:00:00Z",
                metadata: &BTreeMap::from([("bpfman.io/application".into(), "test".into())]),
            },
        )
        .expect("commit");
}

fn runtime_reads_the_real_store<S: TestStore>(backend: S) {
    let (_temp, runtime, store, bpfman) = setup(backend);
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
    assert_eq!(calls(&store), ["commit", "summaries", "records", "records"]);
    assert!(layout(&runtime).database_path().exists());
}

fn store_errors_keep_their_category_and_are_not_retried<S: TestStore>(backend: S) {
    for (fault, kind, expected) in [
        (
            "summaries",
            ErrorKind::IncompatibleState,
            crate::ErrorKind::IncompatibleState,
        ),
        (
            "summaries",
            ErrorKind::InvalidData,
            crate::ErrorKind::InvalidState,
        ),
        (
            "summaries",
            ErrorKind::Unavailable,
            crate::ErrorKind::Unavailable,
        ),
    ] {
        let (_temp, runtime, store, bpfman) = setup(backend);
        store.fail(fault, kind);
        let err = bpfman.list(&Default::default()).expect_err("injected");

        assert_eq!(err.kind(), expected);
        assert_eq!(
            calls(&store).iter().filter(|call| **call == fault).count(),
            1
        );
        assert!(store.records(&runtime).is_empty());
        assert!(layout(&runtime).database_path().exists());
    }
}

fn full_read_failure_is_not_missing_or_an_empty_list<S: TestStore>(backend: S) {
    let (_temp, runtime, store, bpfman) = setup(backend);
    store.fail("records", ErrorKind::Unavailable);
    let get = bpfman
        .get(id())
        .expect_err("failed read must not become not found");
    let list = bpfman
        .list_entries(&Default::default())
        .expect_err("failed read must not become an empty list");

    assert_eq!(get.kind(), crate::ObservationErrorKind::Unavailable);
    assert_eq!(list.kind(), crate::ObservationErrorKind::Unavailable);
    assert_eq!(calls(&store), ["records", "records"]);
    assert!(layout(&runtime).database_path().exists());
}

fn retained_backend_receipts_survive_failed_unload_and_explicit_retry<S: TestStore>(backend: S) {
    let (_temp, runtime, store, bpfman) = setup(backend);
    scope(&runtime, |w| seed(&store, w));
    store.fail("delete program", ErrorKind::Unavailable);
    let error = bpfman.unload(id()).expect_err("record failure");

    assert!(!store.records(&runtime).is_empty());
    // Standalone-link and XDP preflight each validate the retained store handle.
    assert_eq!(
        calls(&store),
        [
            "commit",
            "observe",
            "validate",
            "validate",
            "delete program"
        ]
    );

    let error = bpfman.retry_unload(error).expect_err("unchanged fault");

    assert!(!store.records(&runtime).is_empty());
    assert_eq!(error.report().expect("report").attempts().len(), 2);
    store.clear_faults();
    store.fail("delete map set", ErrorKind::Unavailable);

    let report = bpfman.retry_unload(error).expect("GC warning");

    assert!(store.records(&runtime).is_empty());
    assert_eq!(report.unresolved(), 1);
    store.clear_faults();

    let (_other_temp, _other, _, other) = setup(backend);
    let report = other
        .retry_unload_cleanup(report)
        .expect("wrong root GC warning");

    assert_eq!(report.unresolved(), 1);
    store.clear_faults();

    let report = bpfman.retry_unload_cleanup(report).expect("clean");

    assert_eq!(report.unresolved(), 0);
    assert!(store.records(&runtime).is_empty());
    assert_eq!(calls(&store).iter().filter(|c| **c == "observe").count(), 1);
    assert_eq!(
        calls(&store)
            .iter()
            .filter(|c| **c == "delete program")
            .count(),
        3
    );
    assert!(layout(&runtime).database_path().exists());
}

fn unload_and_retry_keep_receipts_when_writer_acquisition_times_out<S: TestStore>(backend: S) {
    for fault in ["delete program", "delete map set"] {
        let (_temp, runtime, store, bpfman) = setup(backend);
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
        assert!(!store.records(&runtime).is_empty());

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
        assert!(store.records(&runtime).is_empty());
        assert_eq!(
            calls(&store)
                .iter()
                .filter(|call| **call == "observe")
                .count(),
            1
        );
    }
}

fn operations_retain_the_adopted_runtime_when_its_path_is_replaced<S: TestStore>(backend: S) {
    let temp = tempfile::tempdir().expect("tempdir");
    let original = temp.path().join("runtime");
    let runtime = RuntimeDirectory::open_or_create(
        RuntimeLayout::try_from(original.clone()).expect("layout"),
    )
    .expect("runtime");
    let store = Faults::new(backend);
    scope(&runtime, |w| {
        store.open(w).expect("initialize");
    });
    scope(&runtime, |writer| seed(&store, writer));
    let active = ActiveStore::open(store.clone(), runtime.layout(), TIMEOUT).expect("active");
    let bpfman = Bpfman::new(active, bpfman_kernel_aya::Kernel, TIMEOUT);

    std::fs::rename(&original, temp.path().join("adopted")).expect("move root");
    std::fs::create_dir(&original).expect("replacement");

    // The concrete backend may retain the opened state or reject the moved path;
    // neither outcome may redirect reads or mutations into the replacement.
    if let Ok(programs) = bpfman.list(&Default::default()) {
        assert_eq!(programs[0].id(), id());
    }
    if let Ok(report) = bpfman.unload(id()) {
        assert_eq!(report.unresolved(), 0);
    }
    assert_eq!(std::fs::read_dir(original).expect("replacement").count(), 0);
}

fn startup_open_failures_keep_their_category_and_are_not_retried<S: TestStore>(backend: S) {
    for (kind, expected) in [
        (
            ErrorKind::IncompatibleState,
            crate::ErrorKind::IncompatibleState,
        ),
        (ErrorKind::Unavailable, crate::ErrorKind::Unavailable),
    ] {
        let (_temp, runtime, store, _bpfman) = setup(backend);
        store.fail("open", kind);
        let error = ActiveStore::open(store.clone(), runtime.layout(), TIMEOUT)
            .err()
            .expect("injected opening failure must fail startup");

        assert_eq!(error.kind(), expected);
        assert_eq!(calls(&store), ["open"]);
        assert!(store.records(&runtime).is_empty());
    }
}

fn retained_handle_is_revalidated_for_mutation_preflight<S: TestStore>(backend: S) {
    let (_temp, runtime, store, bpfman) = setup(backend);
    store.fail("validate", ErrorKind::IncompatibleState);
    let error = scope(&runtime, |writer| bpfman.store.open(writer))
        .err()
        .expect("changed compatibility must fail preflight");

    assert_eq!(error.kind(), ErrorKind::IncompatibleState);
    assert_eq!(calls(&store), ["validate"]);
    assert!(store.records(&runtime).is_empty());
}

macro_rules! backend_tests {
    ($module:ident, $backend:expr) => {
        mod $module {
            #[test]
            fn runtime_reads_the_real_store() {
                super::runtime_reads_the_real_store($backend);
            }
            #[test]
            fn store_errors_keep_their_category_and_are_not_retried() {
                super::store_errors_keep_their_category_and_are_not_retried($backend);
            }
            #[test]
            fn full_read_failure_is_not_missing_or_an_empty_list() {
                super::full_read_failure_is_not_missing_or_an_empty_list($backend);
            }
            #[test]
            fn retained_backend_receipts_survive_failed_unload_and_explicit_retry() {
                super::retained_backend_receipts_survive_failed_unload_and_explicit_retry($backend);
            }
            #[test]
            fn unload_and_retry_keep_receipts_when_writer_acquisition_times_out() {
                super::unload_and_retry_keep_receipts_when_writer_acquisition_times_out($backend);
            }
            #[test]
            fn operations_retain_the_adopted_runtime_when_its_path_is_replaced() {
                super::operations_retain_the_adopted_runtime_when_its_path_is_replaced($backend);
            }
            #[test]
            fn startup_open_failures_keep_their_category_and_are_not_retried() {
                super::startup_open_failures_keep_their_category_and_are_not_retried($backend);
            }
            #[test]
            fn retained_handle_is_revalidated_for_mutation_preflight() {
                super::retained_handle_is_revalidated_for_mutation_preflight($backend);
            }
        }
    };
}

backend_tests!(sqlite, bpfman_store_sqlite::Backend);
backend_tests!(json, bpfman_store_json::Backend);
