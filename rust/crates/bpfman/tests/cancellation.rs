//! Shared cancellation behaviour through the same active-store setup as the CLI.
#![allow(clippy::expect_used)]

use bpfman_fs::{RuntimeDirectory, RuntimeLayout};
use bpfman_runtime::{
    ActiveStore, Bpfman, Cancellation, ErrorKind, ObservationErrorKind, UnloadErrorKind,
};
use bpfman_store::{OpenStore, UnloadStore};
use std::{num::NonZeroU32, sync::mpsc, time::Duration};

const BUDGET: Duration = Duration::from_secs(3);

fn exercise<S: OpenStore + UnloadStore + bpfman_store::LinkStore + Copy + Send + Sync>(backend: S)
where
    S::Reader: bpfman_store::LinkReader,
{
    let temp = tempfile::tempdir().expect("runtime");
    let layout = RuntimeLayout::try_from(temp.path().join("runtime")).expect("layout");
    let cancelled = Cancellation::new();
    cancelled.cancel();

    let error = ActiveStore::open_with_cancellation(backend, &layout, BUDGET, &cancelled)
        .err()
        .expect("cancelled startup");
    assert_eq!(error.kind(), ErrorKind::Cancelled);
    assert!(!layout.root().exists());

    let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
    runtime
        .with_writer(
            bpfman_lock::AcquireOptions {
                timeout: BUDGET,
                cancelled: None,
            },
            |_| {
                std::thread::scope(|scope| {
                    let cancellation = Cancellation::new();
                    let token = cancellation.clone();
                    let layout = &layout;
                    let (send, receive) = mpsc::channel();
                    let worker = scope.spawn(move || {
                        send.send(()).expect("starting");
                        ActiveStore::open_with_cancellation(backend, layout, BUDGET, &token)
                            .err()
                            .expect("cancelled wait")
                    });
                    receive.recv_timeout(BUDGET).expect("worker ready");
                    cancellation.cancel();
                    assert_eq!(worker.join().expect("worker").kind(), ErrorKind::Cancelled);
                });
            },
        )
        .expect("writer");
    assert!(backend.open_reader(&runtime).expect("reader").is_none());

    let app = Bpfman::new(
        ActiveStore::open(backend, &layout, BUDGET).expect("store"),
        bpfman_kernel_aya::Kernel,
        BUDGET,
    );
    assert_eq!(
        app.list_with_cancellation(&Default::default(), &cancelled)
            .expect_err("cancelled list")
            .kind(),
        ErrorKind::Cancelled
    );
    assert_eq!(
        app.get_with_cancellation(NonZeroU32::MIN, &cancelled)
            .expect_err("cancelled get")
            .kind(),
        ObservationErrorKind::Cancelled
    );
    assert_eq!(
        app.list_entries_with_cancellation(&Default::default(), &cancelled)
            .expect_err("cancelled full list")
            .kind(),
        ObservationErrorKind::Cancelled
    );
    let error = app
        .unload_with_cancellation(NonZeroU32::MIN, &cancelled)
        .expect_err("cancelled unload");
    assert_eq!(error.kind(), UnloadErrorKind::Cancelled);
    assert!(error.report().is_none());

    // Cancellation belongs to an individual caller, not the shared application.
    assert!(
        app.list(&Default::default())
            .expect("independent operation")
            .is_empty()
    );
    assert_eq!(
        app.get(NonZeroU32::MIN)
            .expect_err("ordinary absence")
            .kind(),
        ObservationErrorKind::NotFound
    );
}

#[test]
fn sqlite_cancellation() {
    exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn json_cancellation() {
    exercise(bpfman_store_json::Backend);
}

#[test]
fn cancelled_preparation_does_not_read_the_source() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let missing = temp.path().join("missing.o");
    let cancellation = Cancellation::new();
    cancellation.cancel();

    let error = bpfman_runtime::PreparedProgram::new_with_cancellation(
        &missing,
        bpfman_model::ProgramSpec::Tracepoint(
            bpfman_model::Symbol::try_from("trace").expect("symbol"),
        ),
        Default::default(),
        &cancellation,
    )
    .err()
    .expect("cancelled preparation");

    assert_eq!(error.kind(), bpfman_runtime::LoadErrorKind::Cancelled);
    assert_eq!(error.unresolved(), 0);
    assert!(!missing.exists());
}
