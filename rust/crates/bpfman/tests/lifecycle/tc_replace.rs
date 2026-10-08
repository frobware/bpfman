//! Complete revision execution through the production interpreter and real stores.
use super::*;
use bpfman_model::LinkDetails;
use bpfman_runtime::TcAttach;
use std::num::NonZeroU32;

fn load<S: Store>(f: &Fixture<S>, name: &str) -> NonZeroU32 {
    f.app
        .load(
            PreparedProgram::new(
                &f.kernel,
                &f.temp.path().join("source.o"),
                ProgramSpec::Tc(name.try_into().expect("symbol")),
                Default::default(),
            )
            .expect("prepare"),
        )
        .expect("load")
        .record
        .id
}

fn request(id: NonZeroU32, priority: u32) -> TcAttach {
    TcAttach {
        program_id: id,
        interface: "fake0".parse().expect("interface"),
        netns: Default::default(),
        priority,
        proceed_on: Default::default(),
        metadata: [("member".into(), id.to_string())].into(),
    }
}

fn snapshot<S: Store>(f: &Fixture<S>) -> bpfman_model::TcDispatcherSnapshot {
    f.app
        .list_tc_dispatchers()
        .expect("dispatchers")
        .into_iter()
        .next()
        .expect("snapshot")
}

pub(super) fn lifecycle<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let a = load(&f, "zulu");
    let b = load(&f, "alpha");
    let first = f.app.attach_tc(request(a, 50)).expect("first");
    let second = f.app.attach_tc(request(b, 25)).expect("second");
    let s = snapshot(&f);
    assert_eq!(
        s.members().iter().map(|m| m.member.id).collect::<Vec<_>>(),
        [second.id, first.id]
    );
    assert_eq!(s.members()[0].details.revision.get(), 2);
    assert_ne!(s.members()[1].member.state, first.state);
    assert_eq!(s.members()[1].member.metadata, first.metadata);
    for m in s.members() {
        assert!(
            f.app
                .get_link(m.member.id)
                .expect("observe slot")
                .pin_present
        );
    }
    f.app.detach_tc(second.id).expect("survivor");
    let s = snapshot(&f);
    assert_eq!(s.members()[0].member.id, first.id);
    assert_eq!(s.members()[0].details.slot.index(), 0);
    assert_eq!(s.members()[0].details.revision.get(), 3);
    // A new equal-priority member precedes attached members, as in Go.
    let new = f.app.attach_tc(request(b, 50)).expect("equal priority");
    assert_eq!(snapshot(&f).members()[0].member.id, new.id);
    for _ in 2..10 {
        f.app.attach_tc(request(b, 60)).expect("fill capacity");
    }
    let before = snapshot(&f);
    let offset = f.kernel.events().len();
    assert!(f.app.attach_tc(request(a, 0)).is_err());
    assert_eq!(snapshot(&f), before);
    assert!(!f.kernel.events()[offset..].contains(&Point::StageTc));
    assert_eq!(
        f.app
            .unload(b)
            .expect("unload repeated members on one dispatcher")
            .unresolved(),
        0
    );
    assert_eq!(snapshot(&f).members()[0].member.id, first.id);
    assert_eq!(f.app.unload(a).expect("last member unload").unresolved(), 0);
    f.clean();
}

pub(super) fn failures<S: Store>(backend: S) {
    for point in [Point::StageTc, Point::SwitchTc, Point::RemoveTcStage] {
        for phase in [Phase::Before, Phase::After] {
            if point == Point::RemoveTcStage && phase == Phase::After {
                continue;
            }
            let f = Fixture::new(backend);
            let a = load(&f, "a");
            let b = load(&f, "b");
            f.app.attach_tc(request(a, 50)).expect("first");
            let before = snapshot(&f);
            f.kernel.fail(point, phase);
            if point == Point::RemoveTcStage {
                f.store.set(Some(StorePoint::TcReplace));
            }
            let error = f.app.attach_tc(request(b, 25)).expect_err("fault");
            assert_eq!(snapshot(&f), before);
            f.kernel.clear();
            f.store.set(None);
            let report = f
                .app
                .retry_tc_cleanup(error)
                .expect("recover only compensation");
            assert!(report.primary_failure().is_some());
            assert_eq!(snapshot(&f), before);
            assert_eq!(f.app.unload(a).expect("first").unresolved(), 0);
            assert_eq!(f.app.unload(b).expect("second").unresolved(), 0);
            f.clean();
        }
    }
    // Successful publication is final even when retirement needs another pass.
    let f = Fixture::new(backend);
    let a = load(&f, "a");
    let b = load(&f, "b");
    f.app.attach_tc(request(a, 50)).expect("first");
    f.kernel.fail(Point::RemoveTcStage, Phase::Before);
    let error = f.app.attach_tc(request(b, 25)).expect_err("retirement");
    assert!(error.committed_snapshot().is_some());
    assert_eq!(snapshot(&f).members().len(), 2);
    f.kernel.clear();
    let report = f.app.retry_tc_cleanup(error).expect("retire");
    assert!(report.committed_snapshot().is_some());
    assert_eq!(f.app.unload(a).expect("a").unresolved(), 0);
    assert_eq!(f.app.unload(b).expect("b").unresolved(), 0);
    f.clean();
}

pub(super) fn restoration<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let a = load(&f, "a");
    let b = load(&f, "b");
    let first = f.app.attach_tc(request(a, 50)).expect("first");
    let before = snapshot(&f);
    f.store.set(Some(StorePoint::TcReplace));
    f.kernel.fail(Point::RestoreTc, Phase::Before);
    let offset = f.kernel.events().len();
    let error = f
        .app
        .attach_tc(request(b, 25))
        .expect_err("publication and restoration");
    assert_eq!(error.restoration_attempts().len(), 1);
    assert!(error.attempts().is_empty());
    assert_eq!(snapshot(&f), before);
    assert!(!f.kernel.events()[offset..].contains(&Point::RemoveTcStage));
    let cancelled = Cancellation::new();
    cancelled.cancel();
    let error = f
        .app
        .retry_tc_cleanup_with_cancellation(error, &cancelled)
        .expect_err("cancelled admission");
    assert_eq!(error.restoration_attempts().len(), 1);
    let error = f
        .app
        .retry_tc_cleanup(error)
        .expect_err("second failed restoration");
    assert_eq!(error.restoration_attempts().len(), 2);
    let other = Fixture::new(backend);
    let error = other
        .app
        .retry_tc_cleanup(error)
        .expect_err("foreign runtime");
    assert!(error.unresolved() > 0);
    let runtime = RuntimeDirectory::open_or_create(f.layout.clone()).expect("same root");
    let foreign = Bpfman::new(
        ActiveStore::open(f.store.clone(), &f.layout, Duration::from_secs(1)).expect("same store"),
        FakeKernel::new(&runtime),
        Duration::from_secs(1),
    );
    let error = foreign
        .retry_tc_cleanup(error)
        .expect_err("foreign kernel at original root");
    assert!(error.unresolved() > 0);
    f.kernel.clear();
    f.store.set(None);
    let report = f
        .app
        .retry_tc_cleanup(error)
        .expect("restored then cleaned");
    assert_eq!(report.restoration_attempts().len(), 5);
    assert!(report.primary_failure().is_some());
    assert_eq!(snapshot(&f), before);
    f.app.detach_tc(first.id).expect("detach");
    assert_eq!(f.app.unload(a).expect("a").unresolved(), 0);
    assert_eq!(f.app.unload(b).expect("b").unresolved(), 0);
    f.clean();
    other.clean();
}

pub(super) fn cancellation<S: Store>(backend: S) {
    for point in [Point::StageTc, Point::SwitchTc] {
        let f = Fixture::new(backend);
        let a = load(&f, "a");
        let b = load(&f, "b");
        f.app.attach_tc(request(a, 50)).expect("first");
        let before = snapshot(&f);
        let c = Cancellation::new();
        f.kernel.cancel_at(point, &c);
        let error = f
            .app
            .attach_tc_with_cancellation(request(b, 25), &c)
            .expect_err("cancel before publication");
        assert_eq!(error.kind(), bpfman_runtime::LinkErrorKind::Cancelled);
        assert_eq!(error.unresolved(), 0);
        assert_eq!(snapshot(&f), before);
        f.kernel.clear();
        assert_eq!(f.app.unload(a).expect("a").unresolved(), 0);
        assert_eq!(f.app.unload(b).expect("b").unresolved(), 0);
        f.clean();
    }
    let f = Fixture::new(backend);
    let a = load(&f, "a");
    let b = load(&f, "b");
    f.app.attach_tc(request(a, 50)).expect("first");
    let c = Cancellation::new();
    f.store.cancel_at(StorePoint::TcReplace, &c);
    f.app
        .attach_tc_with_cancellation(request(b, 25), &c)
        .expect("in-flight publication decides outcome");
    assert!(c.is_cancelled());
    assert_eq!(snapshot(&f).members().len(), 2);
    assert_eq!(f.app.unload(a).expect("a").unresolved(), 0);
    assert_eq!(f.app.unload(b).expect("b").unresolved(), 0);
    f.clean();
}

pub(super) fn unload<S: Store>(backend: S) {
    for restoration in [false, true] {
        let f = Fixture::new(backend);
        let a = load(&f, "a");
        let b = load(&f, "b");
        let first = f.app.attach_tc(request(a, 50)).expect("first");
        f.app.attach_tc(request(b, 60)).expect("survivor");
        f.app.attach_tc(request(a, 70)).expect("repeated member");
        f.store.set(Some(StorePoint::TcReplace));
        if restoration {
            f.kernel.fail(Point::RestoreTc, Phase::Before);
        }
        let error = f
            .app
            .unload(a)
            .expect_err("replacement blocks program teardown");
        assert!(f.app.get(a).is_ok());
        assert_eq!(snapshot(&f).members().len(), 3);
        if restoration {
            assert!(
                error.report().expect("report").tc_attempts()[0]
                    .outcome()
                    .expect_err("restoration")
                    .restoration_attempts()
                    .len()
                    == 1
            );
        }
        f.store.set(None);
        f.kernel.clear();
        let report = if restoration {
            // Restoration recovery cannot repeat the failed forward detach inline.
            let error = f
                .app
                .retry_unload(error)
                .expect_err("recovery retains forward work");
            f.app.retry_unload(error).expect("separate forward pass")
        } else {
            // The original pass already completed compensation; this is a new forward budget.
            f.app.retry_unload(error).expect("separate forward pass")
        };
        assert_eq!(report.unresolved(), 0);
        let s = snapshot(&f);
        assert_eq!(s.members().len(), 1);
        assert_eq!(s.members()[0].member.program_id, b);
        assert!(!s.members().iter().any(|m| m.member.id == first.id));
        assert!(matches!(s.members()[0].member.details, LinkDetails::Tc(_)));
        assert_eq!(f.app.unload(b).expect("last").unresolved(), 0);
        f.clean();
    }
    let f = Fixture::new(backend);
    let a = load(&f, "a");
    f.app.attach_tc(request(a, 50)).expect("first interface");
    let mut other = request(a, 50);
    other.interface = "fake1".parse().expect("interface");
    f.app.attach_tc(other).expect("second interface");
    // Admission reads three inventories; the first independent link then succeeds.
    // A failed fresh read for the second must retain it and preserve prior progress.
    f.store.fail_after(StorePoint::ReadLinks, 4);
    let error = f.app.unload(a).expect_err("second-link inventory failure");
    let attempts = error.report().expect("retained report").tc_attempts();
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        attempts[0]
            .outcome()
            .expect("first complete")
            .attempts()
            .len(),
        3
    );
    assert_eq!(
        attempts[1]
            .outcome()
            .expect_err("second blocked")
            .unresolved(),
        0
    );
    f.store.set(None);
    let report = f
        .app
        .retry_unload(error)
        .expect("retry only remaining link");
    assert_eq!(report.unresolved(), 0);
    assert_eq!(
        report.tc_attempts()[0]
            .outcome()
            .expect("still complete")
            .attempts()
            .len(),
        3
    );
    f.clean();
}
