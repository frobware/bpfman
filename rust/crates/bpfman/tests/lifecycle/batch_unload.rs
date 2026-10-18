//! Batch admission, independent failures and retained cleanup with a real store.
use super::*;
use bpfman_runtime::{ErrorKind, UnloadErrorKind};
use std::num::NonZeroU32;

pub(super) fn recovery<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let blocked = f.app.load(f.request(false)).expect("load").record.id;
    let independent = f.app.load(f.request(false)).expect("load").record.id;
    f.app
        .attach_tracepoint(tracepoint(blocked))
        .expect("attach");
    let missing = NonZeroU32::new(u32::MAX).expect("missing ID");
    f.kernel.fail(Point::RemoveLink, Phase::Before);

    let outcomes = f
        .app
        .unload_many(&[missing, blocked, independent, missing])
        .expect("batch admitted");
    assert_eq!(
        outcomes.iter().map(|r| r.id).collect::<Vec<_>>(),
        [missing, blocked, independent, missing]
    );
    let mut outcomes = outcomes.into_iter();
    assert!(
        outcomes
            .next()
            .expect("missing")
            .outcome
            .expect_err("missing")
            .is_program_missing()
    );
    let error = outcomes
        .next()
        .expect("blocked")
        .outcome
        .expect_err("link failure");
    assert!(!error.is_program_missing());
    assert!(error.report().expect("retained cleanup").unresolved() > 0);
    assert_eq!(
        outcomes
            .next()
            .expect("independent")
            .outcome
            .expect("continued after failure")
            .unresolved(),
        0
    );
    assert!(
        outcomes
            .next()
            .expect("missing again")
            .outcome
            .expect_err("not deduplicated")
            .is_program_missing()
    );
    assert!(f.app.get(blocked).expect("blocked program retained").links[0].pin_present);
    assert!(f.app.get(independent).is_err());

    f.kernel.clear();
    assert_eq!(
        f.app
            .retry_unload(error)
            .expect("consuming retry")
            .unresolved(),
        0
    );
    f.clean();

    // Post-record failures remain successful outcomes with separately retryable
    // warnings. Keeping all outcomes must retain each program's cleanup receipts.
    let f = Fixture::new(backend);
    let a = f.app.load(f.request(false)).expect("load").record.id;
    let b = f.app.load(f.request(false)).expect("load").record.id;
    f.kernel.fail(Point::RemoveMap, Phase::Before);
    let results = f.app.unload_many(&[a, b]).expect("batch");
    f.kernel.clear();
    for result in results {
        let report = result.outcome.expect("unloaded with warnings");
        assert!(report.unresolved() > 0);
        assert!(report.attempts().iter().any(|a| a.outcome.is_err()));
        assert_eq!(
            f.app
                .retry_unload_cleanup(report)
                .expect("retry warnings")
                .unresolved(),
            0
        );
    }
    f.clean();
}

pub(super) fn cancellation<S: Store>(backend: S) {
    for point in [Point::ObserveUnload, Point::RemoveProgram] {
        let f = Fixture::new(backend);
        let a = f.app.load(f.request(false)).expect("load").record.id;
        let b = f.app.load(f.request(false)).expect("load").record.id;
        f.app.attach_tracepoint(tracepoint(a)).expect("attach");
        let token = Cancellation::new();
        f.kernel.cancel_at(point, &token);

        let mut outcomes = f
            .app
            .unload_many_with_cancellation(&[a, b], &token)
            .expect("admitted")
            .into_iter();
        let first = outcomes.next().expect("first").outcome;
        if point == Point::ObserveUnload {
            let error = first.expect_err("cancelled preflight");
            assert_eq!(error.kind(), UnloadErrorKind::Cancelled);
            assert!(error.report().is_none());
            assert!(f.app.get(a).expect("unchanged").links[0].pin_present);
        } else {
            assert_eq!(first.expect("finish destructive pass").unresolved(), 0);
            assert!(f.app.get(a).is_err());
        }
        let error = outcomes
            .next()
            .expect("second")
            .outcome
            .expect_err("not started");
        assert_eq!(error.kind(), UnloadErrorKind::Cancelled);
        assert!(!error.is_program_missing());
        assert!(error.report().is_none());
        assert!(f.app.get(b).is_ok());

        f.kernel.clear();
        if point == Point::ObserveUnload {
            assert_eq!(f.app.unload(a).expect("cleanup").unresolved(), 0);
        }
        assert_eq!(f.app.unload(b).expect("cleanup").unresolved(), 0);
        f.clean();
    }
}

pub(super) fn admission<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let a = f.app.load(f.request(false)).expect("load").record.id;
    let b = f.app.load(f.request(false)).expect("load").record.id;
    let runtime = RuntimeDirectory::open_existing(f.layout.clone())
        .expect("runtime")
        .expect("existing");
    let app = Bpfman::new(
        ActiveStore::open(f.store.clone(), &f.layout, Duration::from_secs(1)).expect("active"),
        f.kernel.clone(),
        Duration::from_millis(1),
    );
    let before = f.kernel.events();
    let error = std::thread::scope(|threads| {
        let (ready_send, ready) = std::sync::mpsc::sync_channel(0);
        let (release, released) = std::sync::mpsc::channel();
        let runtime = &runtime;
        let holder = threads.spawn(move || {
            runtime
                .with_writer(
                    bpfman_lock::AcquireOptions {
                        timeout: Duration::from_secs(1),
                        cancelled: None,
                    },
                    |_| {
                        ready_send.send(()).expect("ready");
                        let _ = released.recv();
                    },
                )
                .expect("hold writer");
        });
        ready
            .recv_timeout(Duration::from_secs(1))
            .expect("writer held");
        let error = app.unload_many(&[a, b]).err().expect("contended writer");
        release.send(()).expect("release writer");
        holder.join().expect("holder");
        error
    });
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    let token = Cancellation::new();
    token.cancel();
    let error = app
        .unload_many_with_cancellation(&[a, b], &token)
        .err()
        .expect("cancelled admission");
    assert_eq!(error.kind(), ErrorKind::Cancelled);
    assert_eq!(f.kernel.events(), before);
    assert_eq!(f.app.list(&Default::default()).expect("unchanged").len(), 2);

    for result in app.unload_many(&[a, b]).expect("admitted") {
        assert_eq!(result.outcome.expect("cleanup").unresolved(), 0);
    }
    f.clean();
}
