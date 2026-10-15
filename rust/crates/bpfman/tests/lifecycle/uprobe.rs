//! Probe admission, cancellation and consuming recovery through both real stores.
use super::*;
use bpfman_model::{LinkDetails, LinkState};
use bpfman_runtime::UprobeAttach;
use std::num::NonZeroU32;

fn load<S: Store>(f: &Fixture<S>, retprobe: bool) -> NonZeroU32 {
    let name = "entry".try_into().expect("symbol");
    let spec = if retprobe {
        ProgramSpec::Uretprobe(name)
    } else {
        ProgramSpec::Uprobe(name)
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

fn request(id: NonZeroU32, container_pid: u32) -> UprobeAttach {
    UprobeAttach {
        program_id: id,
        target: bpfman_model::UprobeTarget::new(
            "/host/target",
            Some("entry".try_into().expect("symbol")),
            0,
            1234,
        )
        .expect("target")
        .with_container_pid(container_pid)
        .expect("container selector"),
        metadata: [("owner".into(), "probe-contract".into())].into(),
    }
}

pub(super) fn lifecycle<S: Store>(backend: S) {
    for (retprobe, container_pid) in [(false, 0), (true, 0), (false, 9876), (true, 9876)] {
        let f = Fixture::new(backend);
        let id = load(&f, retprobe);
        let link = f
            .app
            .attach_uprobe(request(id, container_pid))
            .expect("attach");
        let observed = f.app.get_link(link.id).expect("observation");
        let LinkDetails::Uprobe(d) = observed.record.details else {
            panic!("probe")
        };
        assert_eq!(d.retprobe(), retprobe);
        assert_eq!(d.container_pid().map_or(0, NonZeroU32::get), container_pid);
        assert_eq!(d.function(), Some("entry"));
        assert_eq!(d.pid().map(NonZeroU32::get), Some(1234));
        assert_eq!(d.target(), "/host/target");
        assert_eq!(d.offset(), 0);
        assert_eq!(observed.record.metadata["owner"], "probe-contract");
        assert!(observed.kernel.is_some() && observed.pin_present);
        assert!(matches!(link.state, LinkState::Attached { .. }));
        let report = f.app.detach(link.id).expect("detach");
        assert_eq!(report.unresolved(), 0);
        assert!(f.app.get(id).expect("program retained").links.is_empty());

        let mut second = request(id, container_pid);
        second.target =
            bpfman_model::UprobeTarget::new("/host/other", None, 42, 0).expect("offset target");
        f.app
            .attach_uprobe(request(id, container_pid))
            .expect("reattach");
        f.app.attach_uprobe(second).expect("independent attachment");
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
            .attach_uprobe(request(wrong, container_pid))
            .expect_err("wrong family");
        assert!(f.app.list_link_records().expect("no intent").is_empty());
        let report = f.app.unload(wrong).expect("unload tracepoint");
        assert_eq!(report.unresolved(), 0);
        f.clean();
    }
}

pub(super) fn recovery<S: Store>(backend: S) {
    for (retprobe, container_pid) in [(false, 0), (true, 0), (false, 9876), (true, 9876)] {
        for (point, phase) in [
            (Point::PrepareLink, Phase::Before),
            (Point::Attach, Phase::Before),
            (Point::PinLink, Phase::Before),
            (Point::PinLink, Phase::After),
        ] {
            let f = Fixture::new(backend);
            let id = load(&f, retprobe);
            f.kernel.fail(point, phase);
            let error = f
                .app
                .attach_uprobe(request(id, container_pid))
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
            let id = load(&f, retprobe);
            f.store.set(Some(point));
            f.app
                .attach_uprobe(request(id, container_pid))
                .expect_err("publication failure");
            f.store.set(None);
            assert!(f.app.list_link_records().expect("compensated").is_empty());
            let report = f.app.unload(id).expect("unload");
            assert_eq!(report.unresolved(), 0);
            f.clean();
        }
        for point in [Point::PrepareLink, Point::Attach, Point::PinLink] {
            let f = Fixture::new(backend);
            let id = load(&f, retprobe);
            let token = Cancellation::new();
            f.kernel.cancel_at(point, &token);
            f.app
                .attach_uprobe_with_cancellation(request(id, container_pid), &token)
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
        let id = load(&f, retprobe);
        f.store.set(Some(StorePoint::FinaliseLink));
        f.kernel.fail(Point::RemoveLink, Phase::Before);
        let error = f
            .app
            .attach_uprobe(request(id, container_pid))
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
        f.app
            .attach_uprobe(request(id, container_pid))
            .expect("first");
        f.app
            .attach_uprobe(request(id, container_pid))
            .expect("second");
        f.app
            .attach_uprobe(request(id, container_pid))
            .expect("third");
        f.store.fail_after(StorePoint::DeleteLink, 1);
        let error = f
            .app
            .unload(id)
            .expect_err("record failure blocks program, not other pins");
        assert!(f.app.get(id).is_ok());
        let remaining = f.app.list_link_records().expect("records");
        assert_eq!(remaining.len(), 2);
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
