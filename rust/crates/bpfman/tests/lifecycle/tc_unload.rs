//! Production unload with stateful TC ownership and both concrete stores.
use super::*;
use bpfman_runtime::{TcAttach, UnloadErrorKind};

fn load<S: Store>(f: &Fixture<S>) -> std::num::NonZeroU32 {
    f.app
        .load(
            PreparedProgram::new(
                &f.kernel,
                &f.temp.path().join("source.o"),
                ProgramSpec::Tc("entry".try_into().expect("symbol")),
                Default::default(),
            )
            .expect("prepare TC"),
        )
        .expect("load TC")
        .record
        .id
}

fn request(id: std::num::NonZeroU32, interface: &str) -> TcAttach {
    TcAttach {
        program_id: id,
        interface: interface.parse().expect("interface"),
        netns: Default::default(),
        priority: 25,
        proceed_on: Default::default(),
        metadata: Default::default(),
    }
}

pub(super) fn lifecycle<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let id = load(&f);
    let a = f
        .app
        .attach_tc(request(id, "fake0"))
        .expect("first interface");
    let b = f
        .app
        .attach_tc(request(id, "fake1"))
        .expect("second interface");
    let report = f.app.unload(id).expect("attached unload");
    assert_eq!(report.unresolved(), 0);
    assert_eq!(
        report
            .tc_attempts()
            .iter()
            .map(|a| a.link_id())
            .collect::<Vec<_>>(),
        [a.id, b.id]
    );
    assert!(
        report
            .tc_attempts()
            .iter()
            .all(|a| a.outcome().expect("detach").attempts().len() == 3)
    );
    assert!(report.xdp_attempts().is_empty());
    f.clean();
}

pub(super) fn preflight<S: Store>(backend: S) {
    for cancel in [false, true] {
        let f = Fixture::new(backend);
        let id = load(&f);
        f.app.attach_tc(request(id, "fake0")).expect("first");
        f.app.attach_tc(request(id, "fake1")).expect("second");
        let before = f.app.list_link_records().expect("links");
        let token = Cancellation::new();
        if cancel {
            f.kernel.cancel_at(Point::ObserveTc, &token);
        } else {
            f.kernel.fail_after(Point::ObserveTc, Phase::Before, 1);
        }
        let offset = f.kernel.events().len();
        let error = f
            .app
            .unload_with_cancellation(id, &token)
            .expect_err("preflight");
        assert!(error.report().is_none());
        assert!(!f.kernel.events()[offset..].iter().any(|p| matches!(
            p,
            Point::RemoveTcFilter | Point::RemoveTcStage | Point::RemoveProgram
        )));
        assert_eq!(f.app.list_link_records().expect("unchanged"), before);
        f.kernel.clear();
        assert_eq!(f.app.unload(id).expect("fresh unload").unresolved(), 0);
        f.clean();
    }
}

pub(super) fn failures<S: Store>(backend: S) {
    for point in [
        Some(Point::RemoveTcFilter),
        Some(Point::RemoveTcStage),
        None,
    ] {
        let f = Fixture::new(backend);
        let id = load(&f);
        f.app.attach_tc(request(id, "fake0")).expect("first");
        f.app.attach_tc(request(id, "fake1")).expect("second");
        if let Some(point) = point {
            f.kernel.fail_after(point, Phase::Before, 1);
        } else {
            f.store.fail_after(StorePoint::TcDelete, 1);
        }
        let offset = f.kernel.events().len();
        let error = f.app.unload(id).expect_err("second prerequisite failure");
        let r = error.report().expect("retained");
        assert!(r.attempts().is_empty());
        assert_eq!(r.tc_attempts().len(), 2);
        assert!(r.tc_attempts()[0].outcome().is_ok());
        let nested = r.tc_attempts()[1].outcome().expect_err("failure");
        let remaining = match point {
            Some(Point::RemoveTcFilter) => 3,
            Some(_) => 2,
            None => 1,
        };
        assert_eq!(nested.unresolved(), remaining);
        assert_eq!(nested.attempts().len(), 4 - remaining);
        assert!(!f.kernel.events()[offset..].contains(&Point::RemoveProgram));
        assert!(f.app.get(id).is_ok());
        let attempts = nested.attempts().len();
        let error = f
            .app
            .retry_unload(error)
            .expect_err("one explicit failed pass");
        assert_eq!(
            error.report().expect("report").tc_attempts()[1]
                .outcome()
                .expect_err("still failing")
                .attempts()
                .len(),
            attempts + 1
        );
        f.kernel.clear();
        f.store.set(None);
        let report = f.app.retry_unload(error).expect("retry unresolved only");
        assert_eq!(report.unresolved(), 0);
        assert_eq!(
            report.tc_attempts()[0]
                .outcome()
                .expect("unchanged completed link")
                .attempts()
                .len(),
            3
        );
        let recovered = report.tc_attempts()[1].outcome().expect("recovered");
        assert_eq!(recovered.attempts().len(), 5);
        assert!(recovered.attempts().iter().any(|a| a.outcome.is_err()));
        assert_eq!(
            f.kernel.events()[offset..]
                .iter()
                .filter(|p| **p == Point::RemoveProgram)
                .count(),
            1
        );
        f.clean();
    }
    // Failure in the first attachment must still allow an independent second
    // attachment its one cleanup pass, while retaining the program prerequisite.
    for point in [
        Some(Point::RemoveTcFilter),
        Some(Point::RemoveTcStage),
        None,
    ] {
        let f = Fixture::new(backend);
        let id = load(&f);
        f.app.attach_tc(request(id, "fake0")).expect("first");
        f.app.attach_tc(request(id, "fake1")).expect("second");
        if let Some(point) = point {
            f.kernel.fail(point, Phase::Before);
        } else {
            f.store.set(Some(StorePoint::TcDelete));
        }
        let error = f
            .app
            .unload(id)
            .expect_err("both independent cleanups fail");
        let report = error.report().expect("retained");
        assert!(report.attempts().is_empty());
        assert_eq!(report.tc_attempts().len(), 2);
        assert!(report.tc_attempts().iter().all(|a| a.outcome().is_err()));
        f.kernel.clear();
        f.store.set(None);
        let report = f
            .app
            .retry_unload(error)
            .expect("retry each unresolved link");
        assert_eq!(report.unresolved(), 0);
        assert!(
            report
                .tc_attempts()
                .iter()
                .all(|a| a.outcome().expect("completed").attempts().len() == 4)
        );
        f.clean();
    }
}

pub(super) fn cancellation_and_foreign_retry<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let id = load(&f);
    f.app.attach_tc(request(id, "fake0")).expect("attach");
    f.kernel.fail(Point::RemoveTcFilter, Phase::Before);
    let error = f.app.unload(id).expect_err("filter failure");
    let token = Cancellation::new();
    token.cancel();
    let error = f
        .app
        .retry_unload_with_cancellation(error, &token)
        .expect_err("cancelled admission");
    assert_eq!(error.kind(), UnloadErrorKind::Cancelled);
    assert_eq!(
        error.report().expect("report").tc_attempts()[0]
            .outcome()
            .expect_err("retained")
            .attempts()
            .len(),
        1
    );
    let foreign = Fixture::new(backend);
    let error = foreign.app.retry_unload(error).expect_err("foreign root");
    assert_eq!(error.kind(), UnloadErrorKind::InvalidState);
    foreign.clean();
    let runtime = RuntimeDirectory::open_existing(f.layout.clone())
        .expect("runtime")
        .expect("exists");
    let other_kernel = FakeKernel::new(&runtime);
    let other = Bpfman::new(
        ActiveStore::open(f.store.clone(), &f.layout, Duration::from_secs(1)).expect("store"),
        other_kernel.clone(),
        Duration::from_secs(1),
    );
    let error = other.retry_unload(error).expect_err("foreign kernel");
    assert!(error.report().expect("report").attempts().is_empty());
    other_kernel.assert_empty();
    f.kernel.clear();
    let token = Cancellation::new();
    f.kernel.cancel_at(Point::RemoveTcFilter, &token);
    let report = f
        .app
        .retry_unload_with_cancellation(error, &token)
        .expect("admitted pass completes");
    assert!(token.is_cancelled());
    assert_eq!(report.unresolved(), 0);
    f.clean();
    for point in [Point::RemoveTcFilter, Point::RemoveTcStage] {
        let f = Fixture::new(backend);
        let id = load(&f);
        f.app.attach_tc(request(id, "fake0")).expect("attach");
        let token = Cancellation::new();
        f.kernel.cancel_at(point, &token);
        assert_eq!(
            f.app
                .unload_with_cancellation(id, &token)
                .expect("late cancellation")
                .unresolved(),
            0
        );
        assert!(token.is_cancelled());
        f.clean();
    }
}

pub(super) fn new_link<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let id = load(&f);
    f.app.attach_tc(request(id, "fake0")).expect("attach");
    f.store.set(Some(StorePoint::TcDelete));
    let error = f.app.unload(id).expect_err("record-only recovery");
    let new = f
        .app
        .attach_tc(request(id, "fake1"))
        .expect("later writer attaches");
    f.store.set(None);
    let error = f
        .app
        .retry_unload(error)
        .expect_err("new prerequisite blocks program removal");
    assert_eq!(error.kind(), UnloadErrorKind::InvalidState);
    assert!(error.report().expect("report").attempts().is_empty());
    assert_eq!(f.app.get(id).expect("retained program").links.len(), 1);
    f.app
        .detach_tc(new.id)
        .expect("explicitly handle new attachment");
    assert_eq!(
        f.app
            .retry_unload(error)
            .expect("finish retained program")
            .unresolved(),
        0
    );
    f.clean();
}

pub(super) fn post_detach<S: Store>(backend: S) {
    for kernel in [false, true] {
        let f = Fixture::new(backend);
        let id = load(&f);
        f.app.attach_tc(request(id, "fake0")).expect("attach");
        if kernel {
            f.kernel.fail(Point::RemoveProgram, Phase::Before);
        } else {
            f.store.set(Some(StorePoint::DeleteProgram));
        }
        let error = f.app.unload(id).expect_err("program cleanup failure");
        assert!(
            error.report().expect("report").tc_attempts()[0]
                .outcome()
                .is_ok()
        );
        assert!(f.app.list_link_records().expect("detached").is_empty());
        let offset = f.kernel.events().len();
        f.kernel.clear();
        f.store.set(None);
        let report = f.app.retry_unload(error).expect("program retry");
        assert_eq!(report.unresolved(), 0);
        assert!(!f.kernel.events()[offset..].iter().any(|p| matches!(
            p,
            Point::ObserveTc | Point::RemoveTcFilter | Point::RemoveTcStage
        )));
        f.clean();
    }
}
