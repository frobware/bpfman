//! Batch teardown through the production runtime with real persistence.
use super::*;
use bpfman_runtime::{DetachError, DetachReport, ErrorKind, LinkErrorKind};
use std::num::NonZeroU64;

pub(super) fn recovery<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let a = f.app.load(f.request(false)).expect("load").record.id;
    let b = f.app.load(f.request(true)).expect("load XDP").record.id;
    let blocked = f.app.attach_tracepoint(tracepoint(a)).expect("attach").id;
    let independent = f.app.attach_xdp(xdp(b)).expect("attach XDP").id;
    let missing = NonZeroU64::new(u64::MAX).expect("missing");
    f.kernel.fail(Point::RemoveLink, Phase::Before);

    let results = f
        .app
        .detach_many(&[missing, blocked, independent, missing])
        .expect("admitted");
    assert_eq!(
        results.iter().map(|r| r.id).collect::<Vec<_>>(),
        [missing, blocked, independent, missing]
    );
    let mut results = results.into_iter();
    assert!(
        results
            .next()
            .expect("missing")
            .outcome
            .expect_err("missing")
            .is_link_missing()
    );
    let error = results
        .next()
        .expect("blocked")
        .outcome
        .expect_err("cleanup failure");
    assert!(!error.is_link_missing());
    let DetachError::Standalone(error) = error else {
        panic!("standalone receipt");
    };
    assert!(error.report().expect("owned work").unresolved() > 0);
    let DetachReport::Xdp(report) = results
        .next()
        .expect("independent")
        .outcome
        .expect("continued")
    else {
        panic!("XDP report");
    };
    assert_eq!(report.unresolved(), 0);
    assert!(
        results
            .next()
            .expect("missing again")
            .outcome
            .expect_err("duplicate")
            .is_link_missing()
    );
    assert!(f.app.get_link(blocked).expect("blocked").pin_present);
    assert!(f.app.get_link(independent).is_err());
    assert!(f.app.get(a).is_ok());
    assert!(f.app.get(b).is_ok());

    f.kernel.clear();
    // Reports can outlive the original application and be consumed on reopen.
    let reopened = Bpfman::new(
        ActiveStore::open(f.store.clone(), &f.layout, Duration::from_secs(1)).expect("reopen"),
        f.kernel.clone(),
        Duration::from_secs(1),
    );
    assert_eq!(
        reopened
            .retry_link_cleanup(error)
            .expect("retry")
            .unresolved(),
        0
    );
    for result in reopened.unload_many(&[a, b]).expect("cleanup") {
        assert_eq!(result.outcome.expect("unload").unresolved(), 0);
    }
    f.clean();
}

pub(super) fn cancellation<S: Store>(backend: S) {
    for destructive in [false, true] {
        let f = Fixture::new(backend);
        let a = f.app.load(f.request(false)).expect("load").record.id;
        let b = f.app.load(f.request(false)).expect("load").record.id;
        let first = f.app.attach_tracepoint(tracepoint(a)).expect("attach").id;
        let second = f.app.attach_tracepoint(tracepoint(b)).expect("attach").id;
        let token = Cancellation::new();
        if destructive {
            f.kernel.cancel_at(Point::RemoveLink, &token);
        } else {
            f.store.cancel_at(StorePoint::ReadLinks, &token);
        }
        let mut results = f
            .app
            .detach_many_with_cancellation(&[first, second], &token)
            .expect("admitted")
            .into_iter();
        let first_result = results.next().expect("first").outcome;
        if destructive {
            let DetachReport::Standalone(report) = first_result.expect("finish pass") else {
                panic!("standalone");
            };
            assert_eq!(report.unresolved(), 0);
        } else {
            let error = first_result.expect_err("cancelled observation");
            assert_eq!(error.kind(), LinkErrorKind::Cancelled);
            assert!(!error.is_link_missing());
        }
        let error = results
            .next()
            .expect("second")
            .outcome
            .expect_err("not started");
        assert_eq!(error.kind(), LinkErrorKind::Cancelled);
        assert!(!error.is_link_missing());
        assert!(f.app.get_link(second).expect("unchanged").pin_present);
        assert_eq!(
            f.app.list_link_records().expect("intent").len(),
            if destructive { 1 } else { 2 }
        );
        f.kernel.clear();
        for result in f.app.unload_many(&[a, b]).expect("cleanup") {
            assert_eq!(result.outcome.expect("unload").unresolved(), 0);
        }
        f.clean();
    }
}

pub(super) fn observation_failure<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let program = f.app.load(f.request(false)).expect("load").record.id;
    let id = f
        .app
        .attach_tracepoint(tracepoint(program))
        .expect("attach")
        .id;
    let missing = NonZeroU64::new(u64::MAX).expect("missing");
    f.store.set(Some(StorePoint::ReadLinks));
    for result in f.app.detach_many(&[id, missing]).expect("admitted") {
        let error = result.outcome.expect_err("read failure");
        assert_eq!(error.kind(), LinkErrorKind::Unavailable);
        assert!(!error.is_link_missing());
    }
    f.store.set(None);
    assert!(f.app.get_link(id).expect("unchanged").pin_present);
    assert_eq!(f.app.unload(program).expect("cleanup").unresolved(), 0);
    f.clean();
}

pub(super) fn admission<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let program = f.app.load(f.request(false)).expect("load").record.id;
    let id = f
        .app
        .attach_tracepoint(tracepoint(program))
        .expect("attach")
        .id;
    let runtime = RuntimeDirectory::open_existing(f.layout.clone())
        .expect("runtime")
        .expect("exists");
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
                .expect("writer");
        });
        ready.recv_timeout(Duration::from_secs(1)).expect("held");
        let error = app.detach_many(&[id, id]).err().expect("contended");
        release.send(()).expect("release");
        holder.join().expect("holder");
        error
    });
    assert_eq!(error.kind(), ErrorKind::TimedOut);
    let token = Cancellation::new();
    token.cancel();
    let error = app
        .detach_many_with_cancellation(&[id, id], &token)
        .err()
        .expect("cancelled");
    assert_eq!(error.kind(), ErrorKind::Cancelled);
    assert_eq!(f.kernel.events(), before);
    assert!(f.app.get_link(id).expect("unchanged").pin_present);
    assert_eq!(f.app.unload(program).expect("cleanup").unresolved(), 0);
    f.clean();
}

pub(super) fn dispatcher_recovery<S: Store>(backend: S) {
    for tc in [false, true] {
        let f = Fixture::new(backend);
        let program = if tc {
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
        } else {
            f.app.load(f.request(true)).expect("load XDP").record.id
        };
        let independent_program = f.app.load(f.request(false)).expect("load").record.id;
        let independent = f
            .app
            .attach_tracepoint(tracepoint(independent_program))
            .expect("attach")
            .id;
        let blocked = if tc {
            f.app
                .attach_tc(bpfman_runtime::TcAttach {
                    program_id: program,
                    interface: "fake0".parse().expect("interface"),
                    direction: bpfman_model::TcDirection::Ingress,
                    netns: Default::default(),
                    priority: 50,
                    proceed_on: Default::default(),
                    metadata: Default::default(),
                })
                .expect("attach TC")
                .id
        } else {
            f.app.attach_xdp(xdp(program)).expect("attach XDP").id
        };
        f.kernel.fail(
            if tc {
                Point::RemoveTcFilter
            } else {
                Point::RemoveOuter
            },
            Phase::Before,
        );
        let mut results = f
            .app
            .detach_many(&[blocked, independent])
            .expect("admitted")
            .into_iter();
        let error = results
            .next()
            .expect("blocked")
            .outcome
            .expect_err("cleanup failure");
        assert!(!error.is_link_missing());
        let DetachReport::Standalone(report) = results
            .next()
            .expect("independent")
            .outcome
            .expect("continued")
        else {
            panic!("standalone report");
        };
        assert_eq!(report.unresolved(), 0);
        assert!(f.app.get_link(blocked).expect("preserved").pin_present);
        f.kernel.clear();
        let reopened = Bpfman::new(
            ActiveStore::open(f.store.clone(), &f.layout, Duration::from_secs(1)).expect("reopen"),
            f.kernel.clone(),
            Duration::from_secs(1),
        );
        match error {
            DetachError::Tc(error) if tc => {
                assert!(error.unresolved() > 0);
                assert_eq!(
                    reopened
                        .retry_tc_cleanup(error)
                        .expect("TC retry")
                        .unresolved(),
                    0
                );
            }
            DetachError::Xdp(error) if !tc => {
                assert!(error.unresolved() > 0);
                assert_eq!(
                    reopened
                        .retry_xdp_cleanup(error)
                        .expect("XDP retry")
                        .unresolved(),
                    0
                );
            }
            _ => panic!("family receipt"),
        }
        for result in reopened
            .unload_many(&[program, independent_program])
            .expect("cleanup")
        {
            assert_eq!(result.outcome.expect("unload").unresolved(), 0);
        }
        f.clean();
    }
}
