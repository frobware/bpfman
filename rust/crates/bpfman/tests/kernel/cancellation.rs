//! Identical outside-in cancellation scenarios for every persistence backend.

use super::{
    faults::{Faults, Point},
    support::*,
};
use bpfman_runtime::{
    ActiveStore, Bpfman, Cancellation, LoadErrorKind, ObservationErrorKind, PreparedProgram,
    UnloadErrorKind,
};
use bpfman_store::{CommitLoad, OpenStore, UnloadStore};

fn request() -> PreparedProgram {
    PreparedProgram::new(
        &fixture("tracepoint_counter.bpf.o"),
        bpfman_model::ProgramSpec::Tracepoint(
            bpfman_model::Symbol::try_from(NAME).expect("symbol"),
        ),
        Default::default(),
    )
    .expect("prepare")
}

pub(super) fn exercise<S: OpenStore + CommitLoad + UnloadStore + bpfman_store::LinkStore + Clone>(
    backend: S,
) where
    S::Reader: bpfman_store::LinkReader,
{
    let c = Context::new();
    let store = Faults::new(backend);
    let app = Bpfman::new(
        ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("startup"),
        TIMEOUT,
    );

    let cancellation = Cancellation::new();
    store.cancel_at(Point::Validate, &cancellation);
    let error = app
        .load_with_cancellation(request(), &cancellation)
        .expect_err("cancel during preflight");
    assert_eq!(error.kind(), LoadErrorKind::Cancelled);
    assert_eq!(error.unresolved(), 0);
    assert_eq!(store.count(Point::Commit), 0);
    c.no_artifacts();

    // The commit effect has begun. Cancellation must preserve its true result.
    let cancellation = Cancellation::new();
    store.cancel_at(Point::Commit, &cancellation);
    let observed = app
        .load_with_cancellation(request(), &cancellation)
        .expect("commit completes");
    let id = observed.record.id;
    assert!(cancellation.is_cancelled());
    c.present(id);
    assert_eq!(app.list(&Default::default()).expect("new caller").len(), 1);

    let cancellation = Cancellation::new();
    store.cancel_at(Point::ReadSummaries, &cancellation);
    assert_eq!(
        app.list_with_cancellation(&Default::default(), &cancellation)
            .expect_err("cancelled snapshot read")
            .kind(),
        bpfman_runtime::ErrorKind::Cancelled
    );
    assert_eq!(
        app.list(&Default::default())
            .expect("independent read")
            .len(),
        1
    );

    let cancellation = Cancellation::new();
    store.cancel_at(Point::ReadRecords, &cancellation);
    assert_eq!(
        app.get_with_cancellation(id, &cancellation)
            .expect_err("cancelled before kernel observation")
            .kind(),
        ObservationErrorKind::Cancelled
    );

    let cancellation = Cancellation::new();
    store.cancel_at(Point::ObserveUnload, &cancellation);
    let error = app
        .unload_with_cancellation(id, &cancellation)
        .expect_err("cancelled preflight");
    assert_eq!(error.kind(), UnloadErrorKind::Cancelled);
    assert!(error.report().is_none());
    assert_eq!(store.count(Point::DeleteProgram), 0);
    c.present(id);

    // Cancellation after unpin cannot strand teardown. A failed record deletion
    // retains all history and receipts, including successful bytecode removal.
    let cancellation = Cancellation::new();
    store.cancel_at(Point::DeleteProgram, &cancellation);
    store.set(Some(Point::DeleteProgram));
    let error = app
        .unload_with_cancellation(id, &cancellation)
        .expect_err("record failure");
    assert!(cancellation.is_cancelled());
    assert_eq!(error.kind(), UnloadErrorKind::Unavailable);
    let report = error.report().expect("teardown began");
    let remaining = report.unresolved();
    let history = report.attempts().len();
    assert!(!c.layout.program_pin_path(id).exists());
    assert!(!c.layout.bytecode_path(id).exists());

    let error = app
        .retry_unload_with_cancellation(error, &cancellation)
        .expect_err("cancelled retry admission");
    assert_eq!(error.kind(), UnloadErrorKind::Cancelled);
    assert_eq!(error.report().expect("retained").unresolved(), remaining);
    assert_eq!(error.report().expect("history").attempts().len(), history);
    assert_eq!(store.count(Point::DeleteProgram), 1);
    store.set(None);
    let report = app
        .retry_unload(error)
        .expect("fresh caller retries retained work");
    assert_eq!(report.unresolved(), 0);
    assert_eq!(
        report
            .attempts()
            .iter()
            .filter(|a| a.kind == bpfman_core::UnloadKind::Bytecode)
            .count(),
        1
    );
    c.absent(id);

    let cancellation = Cancellation::new();
    store.cancel_at(Point::Commit, &cancellation);
    store.set(Some(Point::Commit));
    let error = app
        .load_with_cancellation(request(), &cancellation)
        .expect_err("failed commit wins over cancellation");
    assert_eq!(error.kind(), LoadErrorKind::Unavailable);
    assert_eq!(error.unresolved(), 0);
    assert!(format!("{:#}", anyhow::Error::new(error)).contains("injected Commit"));
    c.no_artifacts();
    store.set(None);

    let id = app
        .load(request())
        .expect("load for successful teardown")
        .record
        .id;
    let cancellation = Cancellation::new();
    store.cancel_at(Point::DeleteProgram, &cancellation);
    let report = app
        .unload_with_cancellation(id, &cancellation)
        .expect("admitted teardown finishes");
    assert!(cancellation.is_cancelled());
    assert_eq!(report.unresolved(), 0);
    c.absent(id);
    c.no_artifacts();
}
