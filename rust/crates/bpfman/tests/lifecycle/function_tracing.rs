//! Probe admission, cancellation and consuming recovery through real stores.
use super::*;
use bpfman_model::{LinkDetails, LinkState};
use bpfman_runtime::TracingAttach;
use std::num::NonZeroU32;

fn load<S: Store>(f: &Fixture<S>, kind: bpfman_model::TracingKind) -> NonZeroU32 {
    let name = "entry".try_into().expect("symbol");
    let target = if kind == bpfman_model::TracingKind::Lsm {
        "file_open"
    } else {
        "bpfman_test_kfunc_0"
    }
    .try_into()
    .expect("target");
    let spec = match kind {
        bpfman_model::TracingKind::Fentry => ProgramSpec::Fentry { name, target },
        bpfman_model::TracingKind::Fexit => ProgramSpec::Fexit { name, target },
        bpfman_model::TracingKind::Lsm => ProgramSpec::Lsm { name, hook: target },
    };
    f.app
        .load(
            PreparedProgram::new(
                &f.kernel,
                &f.temp.path().join("source.o"),
                spec,
                Default::default(),
            )
            .expect("prepare"),
        )
        .expect("load")
        .record
        .id
}

fn request(id: NonZeroU32, kind: bpfman_model::TracingKind) -> TracingAttach {
    TracingAttach {
        program_id: id,
        kind,
        metadata: [("owner".into(), "probe-contract".into())].into(),
    }
}

pub(super) fn lifecycle<S: Store>(backend: S) {
    for kind in [
        bpfman_model::TracingKind::Fentry,
        bpfman_model::TracingKind::Fexit,
        bpfman_model::TracingKind::Lsm,
    ] {
        let f = Fixture::new(backend);
        let id = load(&f, kind);
        let link = f.app.attach_tracing(request(id, kind)).expect("attach");
        let observed = f.app.get_link(link.id).expect("observation");
        let LinkDetails::Tracing(d) = observed.record.details else {
            panic!("probe")
        };
        assert_eq!(d.kind, kind);
        assert_eq!(
            d.function.as_str(),
            if kind == bpfman_model::TracingKind::Lsm {
                "file_open"
            } else {
                "bpfman_test_kfunc_0"
            }
        );
        assert_eq!(observed.record.metadata["owner"], "probe-contract");
        assert!(observed.kernel.is_some() && observed.pin_present);
        assert!(matches!(link.state, LinkState::Attached { .. }));
        let report = f.app.detach(link.id).expect("detach");
        assert_eq!(report.unresolved(), 0);
        assert!(f.app.get(id).expect("program retained").links.is_empty());

        f.app.attach_tracing(request(id, kind)).expect("reattach");
        let error = f
            .app
            .attach_tracing(request(id, kind))
            .expect_err("duplicate kernel refusal");
        assert_eq!(error.report().expect("compensation").unresolved(), 0);
        assert_eq!(
            f.app.list_link_records().expect("original retained").len(),
            1
        );
        let app = Bpfman::new(
            ActiveStore::open(f.store.clone(), &f.layout, Duration::from_secs(1)).expect("reopen"),
            f.kernel.clone(),
            Duration::from_secs(1),
        );
        let report = app.unload(id).expect("reopened attached unload");
        assert_eq!(report.unresolved(), 0);
        f.clean();

        let wrong = f.app.load(f.request(false)).expect("tracepoint").record.id;
        f.app
            .attach_tracing(request(wrong, kind))
            .expect_err("wrong family");
        assert!(f.app.list_link_records().expect("no intent").is_empty());
        let report = f.app.unload(wrong).expect("unload tracepoint");
        assert_eq!(report.unresolved(), 0);
        f.clean();
    }
}

pub(super) fn recovery<S: Store>(backend: S) {
    for kind in [
        bpfman_model::TracingKind::Fentry,
        bpfman_model::TracingKind::Fexit,
        bpfman_model::TracingKind::Lsm,
    ] {
        for (point, phase) in [
            (Point::PrepareLink, Phase::Before),
            (Point::Attach, Phase::Before),
            (Point::PinLink, Phase::Before),
            (Point::PinLink, Phase::After),
        ] {
            let f = Fixture::new(backend);
            let id = load(&f, kind);
            f.kernel.fail(point, phase);
            let error = f
                .app
                .attach_tracing(request(id, kind))
                .expect_err("acquisition failure");
            assert_eq!(error.report().map_or(0, |r| r.unresolved()), 0);
            f.kernel.clear();
            assert!(f.app.list_link_records().expect("compensated").is_empty());
            let report = f.app.unload(id).expect("program retained");
            assert_eq!(report.unresolved(), 0);
            f.clean();
        }
        for point in [StorePoint::CreateLink, StorePoint::FinaliseLink] {
            let f = Fixture::new(backend);
            let id = load(&f, kind);
            f.store.set(Some(point));
            f.app
                .attach_tracing(request(id, kind))
                .expect_err("publication failure");
            f.store.set(None);
            assert!(f.app.list_link_records().expect("compensated").is_empty());
            let report = f.app.unload(id).expect("unload");
            assert_eq!(report.unresolved(), 0);
            f.clean();
        }
        for point in [Point::PrepareLink, Point::Attach, Point::PinLink] {
            let f = Fixture::new(backend);
            let id = load(&f, kind);
            let token = Cancellation::new();
            f.kernel.cancel_at(point, &token);
            f.app
                .attach_tracing_with_cancellation(request(id, kind), &token)
                .expect_err("cancelled");
            f.kernel.clear();
            assert!(
                f.app
                    .list_link_records()
                    .expect("cancelled intent removed")
                    .is_empty()
            );
            let report = f.app.unload(id).expect("unload");
            assert_eq!(report.unresolved(), 0);
            f.clean();
        }

        let f = Fixture::new(backend);
        let id = load(&f, kind);
        f.store.set(Some(StorePoint::FinaliseLink));
        f.kernel.fail(Point::RemoveLink, Phase::Before);
        let error = f
            .app
            .attach_tracing(request(id, kind))
            .expect_err("publication plus cleanup failure");
        assert_eq!(
            error
                .report()
                .expect("retained pin and intent")
                .unresolved(),
            2
        );
        f.store.set(None);
        f.kernel.clear();
        let report = f.app.retry_link_cleanup(error).expect("consuming retry");
        assert_eq!(report.unresolved(), 0);
        f.app.attach_tracing(request(id, kind)).expect("first");
        f.store.set(Some(StorePoint::DeleteLink));
        let error = f.app.unload(id).expect_err("record failure blocks program");
        assert!(f.app.get(id).is_ok());
        let remaining = f.app.list_link_records().expect("records");
        assert_eq!(remaining.len(), 1);
        for link in remaining {
            assert!(!f.app.get_link(link.id).expect("record only").pin_present);
        }
        f.store.set(None);
        // A record-only retry must never acquire/remove the completed pins again.
        f.kernel.fail(Point::RemoveLink, Phase::Before);
        let app = Bpfman::new(
            ActiveStore::open(f.store.clone(), &f.layout, Duration::from_secs(1)).expect("reopen"),
            f.kernel.clone(),
            Duration::from_secs(1),
        );
        let report = app.retry_unload(error).expect("reopened record-only retry");
        assert_eq!(report.unresolved(), 0);
        f.kernel.clear();
        f.clean();
    }
}
