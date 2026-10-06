//! Unload enters the production dispatcher interpreter with both concrete stores.
use super::*;

pub(super) fn lifecycle<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let a = f.app.load(f.request(true)).expect("load").record.id;
    let b = f.app.load(f.request(true)).expect("load").record.id;
    let first = f.app.attach_xdp(xdp(a)).expect("attach");
    let second = f
        .app
        .attach_xdp(xdp(a))
        .expect("second link for same program");
    let survivor = f.app.attach_xdp(xdp(b)).expect("survivor");
    let key = xdp_key(&first);
    let before = f.app.get_xdp_dispatcher(key).expect("before");
    let report = f.app.unload(a).expect("attached unload");
    assert_eq!(report.unresolved(), 0);
    assert_eq!(
        report
            .xdp_attempts()
            .iter()
            .map(|a| a.link_id())
            .collect::<Vec<_>>(),
        [first.id, second.id]
    );
    assert!(report.xdp_attempts().iter().all(|a| a.outcome().is_ok()));
    assert!(f.app.get(a).is_err());
    let after = f.app.get_xdp_dispatcher(key).expect("survivor");
    assert_eq!(after.members().len(), 1);
    assert_eq!(after.members()[0].member.id, survivor.id);
    assert_eq!(
        after.members()[0].outer_link_id,
        before.members()[0].outer_link_id
    );
    assert_eq!(
        after.members()[0].details.revision.get(),
        before.members()[0].details.revision.get() + 2
    );
    let held = f.kernel.hold_outer();
    assert_eq!(f.app.unload(b).expect("last member unload").unresolved(), 0);
    assert!(!held.attached());
    drop(held);
    f.clean();
}

pub(super) fn preflight<S: Store>(backend: S) {
    for point in [Point::ObserveXdp, Point::PrepareXdp, Point::ObserveUnload] {
        for cancel in [false, true] {
            let f = Fixture::new(backend);
            let a = f.app.load(f.request(true)).expect("load").record.id;
            let first = f.app.attach_xdp(xdp(a)).expect("attach");
            let before = f.app.get_xdp_dispatcher(xdp_key(&first)).expect("before");
            let token = Cancellation::new();
            if cancel {
                f.kernel.cancel_at(point, &token);
            } else {
                f.kernel.fail(point, Phase::Before);
            }
            let offset = f.kernel.events().len();
            let error = f
                .app
                .unload_with_cancellation(a, &token)
                .expect_err("preflight");
            assert!(error.report().is_none());
            assert!(!f.kernel.events()[offset..].iter().any(|p| matches!(
                p,
                Point::Switch | Point::RemoveOuter | Point::RemoveProgram | Point::RemoveExtension
            )));
            assert_eq!(
                f.app
                    .get_xdp_dispatcher(xdp_key(&first))
                    .expect("unchanged"),
                before
            );
            f.kernel.clear();
            assert_eq!(f.app.unload(a).expect("unload").unresolved(), 0);
            f.clean();
        }
    }
}

pub(super) fn replacement_failures<S: Store>(backend: S) {
    for failure in [
        Some(Point::LoadDispatcher),
        Some(Point::Revision),
        Some(Point::PinDispatcher),
        Some(Point::Extension),
        Some(Point::Switch),
        Some(Point::RemoveExtension),
        None,
    ] {
        let f = Fixture::new(backend);
        let a = f.app.load(f.request(true)).expect("load").record.id;
        let b = f.app.load(f.request(true)).expect("load").record.id;
        let first = f.app.attach_xdp(xdp(a)).expect("attach");
        f.app.attach_xdp(xdp(b)).expect("survivor");
        if let Some(point) = failure {
            f.kernel.fail(
                point,
                if point == Point::LoadDispatcher {
                    Phase::Before
                } else {
                    Phase::After
                },
            );
        } else {
            f.store.set(Some(StorePoint::XdpReplace));
        }
        // Remove failures only occur before removal in this stateful fake.
        if failure == Some(Point::RemoveExtension) {
            f.kernel.fail(Point::RemoveExtension, Phase::Before);
        }
        let offset = f.kernel.events().len();
        let error = f.app.unload(a).expect_err("XDP prerequisite");
        let report = error.report().expect("retained program prerequisites");
        assert!(report.attempts().is_empty(), "program teardown blocked");
        assert!(report.unresolved() > 0);
        assert_eq!(report.xdp_attempts().len(), 1);
        let nested = report.xdp_attempts()[0].outcome().expect_err("XDP failure");
        if failure == Some(Point::RemoveExtension) {
            assert!(nested.committed_snapshot().is_some());
            assert!(error.to_string().contains("XDP replacement committed"));
        }
        assert!(!f.kernel.events()[offset..].contains(&Point::RemoveProgram));
        assert!(f.app.get(a).is_ok());
        f.kernel.clear();
        f.store.set(None);
        let report = f.app.retry_unload(error).expect("resume unload");
        assert_eq!(report.unresolved(), 0);
        assert!(f.app.get(a).is_err());
        assert_eq!(
            f.app
                .get_xdp_dispatcher(xdp_key(&first))
                .expect("survivor")
                .members()
                .len(),
            1
        );
        assert_eq!(f.app.unload(b).expect("unload").unresolved(), 0);
        f.clean();
    }
}

pub(super) fn restoration<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let a = f.app.load(f.request(true)).expect("load").record.id;
    let b = f.app.load(f.request(true)).expect("load").record.id;
    let first = f.app.attach_xdp(xdp(a)).expect("attach");
    f.app.attach_xdp(xdp(b)).expect("survivor");
    let before = f.app.get_xdp_dispatcher(xdp_key(&first)).expect("before");
    f.store.set(Some(StorePoint::XdpReplace));
    f.kernel.fail(Point::Restore, Phase::Before);
    let error = f.app.unload(a).expect_err("failed restore");
    let nested = error.report().expect("report").xdp_attempts()[0]
        .outcome()
        .expect_err("restore");
    assert_eq!(nested.restoration_attempts().len(), 1);
    assert!(nested.attempts().is_empty());
    let token = Cancellation::new();
    token.cancel();
    let error = f
        .app
        .retry_unload_with_cancellation(error, &token)
        .expect_err("cancelled admission");
    assert_eq!(error.kind(), bpfman_runtime::UnloadErrorKind::Cancelled);
    let foreign = Fixture::new(backend);
    let error = foreign.app.retry_unload(error).expect_err("foreign root");
    let runtime = RuntimeDirectory::open_existing(f.layout.clone())
        .expect("runtime")
        .expect("exists");
    let other_kernel = FakeKernel::new(&runtime);
    let other_app = Bpfman::new(
        ActiveStore::open(f.store.clone(), &f.layout, Duration::from_secs(1)).expect("store"),
        other_kernel.clone(),
        Duration::from_secs(1),
    );
    let error = other_app
        .retry_unload(error)
        .expect_err("foreign kernel at same root");
    other_kernel.assert_empty();
    let error = f
        .app
        .retry_unload(error)
        .expect_err("one failed restore pass");
    assert_eq!(
        error.report().expect("report").xdp_attempts()[0]
            .outcome()
            .expect_err("restore")
            .restoration_attempts()
            .len(),
        2
    );
    f.kernel.clear();
    f.store.set(None);
    let error = f
        .app
        .retry_unload(error)
        .expect_err("restored; forward detach waits for another explicit pass");
    assert!(error.report().expect("report").attempts().is_empty());
    let recovered = error.report().expect("report").xdp_attempts()[0]
        .outcome()
        .expect("restored");
    assert_eq!(recovered.restoration_attempts().len(), 3);
    assert!(recovered.primary_failure().is_some());
    assert_eq!(
        f.app.get_xdp_dispatcher(xdp_key(&first)).expect("old"),
        before
    );
    let report = f.app.retry_unload(error).expect("separate detach pass");
    assert_eq!(report.xdp_attempts().len(), 2);
    assert_eq!(report.unresolved(), 0);
    assert_eq!(f.app.unload(b).expect("last").unresolved(), 0);
    f.clean();
    foreign.clean();
}

pub(super) fn partial_and_cancellation<S: Store>(backend: S) {
    for failure in [true, false] {
        let f = Fixture::new(backend);
        let a = f.app.load(f.request(true)).expect("load").record.id;
        let b = f.app.load(f.request(true)).expect("load").record.id;
        let first = f.app.attach_xdp(xdp(a)).expect("attach");
        let second = f.app.attach_xdp(xdp(a)).expect("another link");
        f.app.attach_xdp(xdp(b)).expect("survivor");
        let token = Cancellation::new();
        if failure {
            f.store.fail_after(StorePoint::XdpReplace, 1);
        } else {
            f.kernel.cancel_at(Point::Switch, &token);
        }
        let result = f.app.unload_with_cancellation(a, &token);
        let report = if failure {
            let error = result.expect_err("second member failure");
            assert_eq!(f.app.get(a).expect("program retained").links.len(), 1);
            let chain = f.app.get_xdp_dispatcher(xdp_key(&first)).expect("chain");
            assert!(!chain.members().iter().any(|m| m.member.id == first.id));
            assert!(chain.members().iter().any(|m| m.member.id == second.id));
            assert_eq!(error.report().expect("report").xdp_attempts().len(), 2);
            f.store.set(None);
            f.app.retry_unload(error).expect("continue remaining link")
        } else {
            assert!(token.is_cancelled());
            result.expect("admitted unload finishes despite cancellation")
        };
        assert_eq!(report.unresolved(), 0);
        assert_eq!(
            f.app
                .get_xdp_dispatcher(xdp_key(&first))
                .expect("survivor")
                .members()
                .len(),
            1
        );
        f.kernel.clear();
        assert_eq!(f.app.unload(b).expect("last").unresolved(), 0);
        f.clean();
    }
    for point in [
        Point::RemoveOuter,
        Point::RemoveExtension,
        Point::RemoveDispatcher,
        Point::RemoveRevision,
    ] {
        let f = Fixture::new(backend);
        let a = f.app.load(f.request(true)).expect("load").record.id;
        f.app.attach_xdp(xdp(a)).expect("attach");
        f.kernel.fail(point, Phase::Before);
        let error = f.app.unload(a).expect_err("last detach failure");
        assert!(error.report().expect("report").attempts().is_empty());
        f.kernel.clear();
        assert_eq!(
            f.app
                .retry_unload(error)
                .expect("retry last detach")
                .unresolved(),
            0
        );
        f.clean();
    }
}

pub(super) fn post_detach<S: Store>(backend: S) {
    for record_failure in [false, true] {
        let f = Fixture::new(backend);
        let a = f.app.load(f.request(true)).expect("load").record.id;
        let b = f.app.load(f.request(true)).expect("load").record.id;
        let first = f.app.attach_xdp(xdp(a)).expect("attach");
        f.app.attach_xdp(xdp(b)).expect("survivor");
        if record_failure {
            f.store.set(Some(StorePoint::DeleteProgram));
        } else {
            f.kernel.fail(Point::RemoveProgram, Phase::Before);
        }
        let mut error = f
            .app
            .unload(a)
            .expect_err("program teardown after XDP success");
        assert!(
            error.report().expect("report").xdp_attempts()[0]
                .outcome()
                .is_ok()
        );
        let count = error.report().expect("report").attempts().len();
        if !record_failure {
            let incoming = f
                .app
                .attach_xdp(xdp(a))
                .expect("another writer attaches while retry is retained");
            error = f
                .app
                .retry_unload(error)
                .expect_err("new prerequisite blocks program unpin");
            assert_eq!(error.kind(), bpfman_runtime::UnloadErrorKind::InvalidState);
            assert_eq!(error.report().expect("report").attempts().len(), count);
            assert!(
                f.app
                    .get_link(incoming.id)
                    .expect("new link intact")
                    .pin_present
            );
            f.app
                .detach_xdp(incoming.id)
                .expect("handle new link explicitly");
        }
        f.store.set(None);
        f.kernel.clear();
        let report = f
            .app
            .retry_unload(error)
            .expect("resume only program teardown");
        assert_eq!(report.xdp_attempts().len(), 1);
        assert_eq!(report.unresolved(), 0);
        assert_eq!(
            f.app
                .get_xdp_dispatcher(xdp_key(&first))
                .expect("survivor")
                .members()
                .len(),
            1
        );
        assert_eq!(f.app.unload(b).expect("last").unresolved(), 0);
        f.clean();
    }
    let f = Fixture::new(backend);
    let a = f.app.load(f.request(true)).expect("load").record.id;
    let b = f.app.load(f.request(true)).expect("load").record.id;
    let first = f.app.attach_xdp(xdp(a)).expect("first interface");
    f.app.attach_xdp(xdp(b)).expect("first survivor");
    let second_request = |id| {
        let mut r = xdp(id);
        r.interface = "fake1".parse().expect("interface");
        r
    };
    let second = f
        .app
        .attach_xdp(second_request(a))
        .expect("second interface");
    f.app
        .attach_xdp(second_request(b))
        .expect("second survivor");
    let runtime = RuntimeDirectory::open_existing(f.layout.clone())
        .expect("runtime")
        .expect("exists");
    runtime
        .with_writer(
            bpfman_lock::AcquireOptions {
                timeout: Duration::from_secs(1),
                cancelled: None,
            },
            |_| {
                assert_eq!(
                    f.app
                        .list_xdp_dispatchers()
                        .expect("listing bypasses writer lock")
                        .len(),
                    2
                );
            },
        )
        .expect("writer");
    let report = f.app.unload(a).expect("two dispatcher prerequisites");
    assert_eq!(report.xdp_attempts().len(), 2);
    assert_eq!(report.unresolved(), 0);
    for key in [xdp_key(&first), xdp_key(&second)] {
        let snapshot = f.app.get_xdp_dispatcher(key).expect("survivor");
        assert_eq!(snapshot.members().len(), 1);
        assert_eq!(snapshot.members()[0].member.program_id, b);
    }
    assert_eq!(f.app.unload(b).expect("both last members").unresolved(), 0);
    f.clean();
}
