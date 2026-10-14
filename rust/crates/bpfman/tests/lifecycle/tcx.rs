//! Native ordering and shared standalone recovery with both real stores.
use super::*;
use bpfman_model::{LinkDetails, TcxLink};
use bpfman_runtime::TcxAttach;
use std::num::NonZeroU32;

fn load<S: Store>(f: &Fixture<S>) -> NonZeroU32 {
    f.app
        .load(
            PreparedProgram::new(
                &f.kernel,
                &f.temp.path().join("source.o"),
                ProgramSpec::Tcx("entry".try_into().expect("symbol")),
                Default::default(),
            )
            .expect("prepare TCX"),
        )
        .expect("load TCX")
        .record
        .id
}

fn request(id: NonZeroU32, priority: u32) -> TcxAttach {
    TcxAttach {
        program_id: id,
        interface: "fake0".parse().expect("interface"),
        direction: bpfman_model::TcDirection::Ingress,
        netns: Default::default(),
        priority,
        metadata: [("owner".into(), "tcx-contract".into())].into(),
    }
}

pub(super) fn ordering<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let ids: Vec<_> = (0..5).map(|_| load(&f)).collect();
    let priorities = [25, 0, 75, 0, 25];
    let records: Vec<_> = ids
        .iter()
        .zip(priorities)
        .map(|(&id, p)| f.app.attach_tcx(request(id, p)).expect("attach"))
        .collect();
    let LinkDetails::Tcx(TcxLink { key, .. }) = records[0].details else {
        panic!("TCX")
    };
    assert_eq!(
        f.kernel.tcx_programs(key),
        [ids[1], ids[3], ids[0], ids[4], ids[2]]
    );
    let positions = [2, 0, 4, 1, 3];
    for (record, position) in records.iter().zip(positions) {
        let observed = f.app.get_link(record.id).expect("fresh rank");
        let LinkDetails::Tcx(d) = observed.record.details else {
            panic!("TCX")
        };
        assert_eq!(d.position, position);
        assert!(observed.kernel.is_some());
    }
    let before = f.app.list_link_records().expect("snapshot");
    let error = f
        .app
        .attach_tcx(request(ids[0], 10))
        .expect_err("duplicate");
    assert!(
        format!("{:#}", anyhow::Error::new(error)).contains(&format!("as link {}", records[0].id))
    );
    assert_eq!(f.app.list_link_records().expect("unchanged"), before);
    let mut egress = request(ids[0], 0);
    egress.direction = bpfman_model::TcDirection::Egress;
    let other = f.app.attach_tcx(egress).expect("independent hook");
    let mut namespace = request(ids[0], 0);
    namespace.netns = "/fake/netns/a".parse().expect("namespace");
    let scoped = f.app.attach_tcx(namespace).expect("independent namespace");
    let _report = f.app.unload(ids[0]).expect("attached unload all scopes");
    for record in [&records[0], &other, &scoped] {
        assert!(f.app.get_link(record.id).is_err());
    }
    assert_eq!(f.kernel.tcx_programs(key), [ids[1], ids[3], ids[4], ids[2]]);
    for id in &ids[1..] {
        let _report = f.app.unload(*id).expect("survivor unload");
    }
    f.clean();
}

pub(super) fn recovery<S: Store>(backend: S) {
    for (point, phase) in [
        (Point::PrepareLink, Phase::Before),
        (Point::Attach, Phase::Before),
        (Point::PinLink, Phase::Before),
        (Point::PinLink, Phase::After),
    ] {
        let f = Fixture::new(backend);
        let id = load(&f);
        f.kernel.fail(point, phase);
        let error = f
            .app
            .attach_tcx(request(id, 0))
            .expect_err("injected attach failure");
        assert_eq!(error.report().map_or(0, |r| r.unresolved()), 0);
        f.kernel.clear();
        assert!(f.app.list_link_records().expect("compensated").is_empty());
        let _report = f.app.unload(id).expect("program retained");
        f.clean();
    }
    {
        let point = Point::RemoveLink;
        let f = Fixture::new(backend);
        let id = load(&f);
        f.store.set(Some(StorePoint::FinaliseLink));
        f.kernel.fail(point, Phase::Before);
        let error = f
            .app
            .attach_tcx(request(id, 0))
            .expect_err("publication and cleanup failure");
        assert_eq!(
            error
                .report()
                .expect("retained pin and intent")
                .unresolved(),
            2
        );
        f.store.set(None);
        f.kernel.clear();
        let _report = f.app.retry_link_cleanup(error).expect("consuming retry");
        assert!(f.app.list_link_records().expect("compensated").is_empty());
        let link = f
            .app
            .attach_tcx(request(id, 0))
            .expect("attach after retry");
        f.kernel.fail(Point::RemoveLink, Phase::Before);
        let error = f
            .app
            .unload(id)
            .expect_err("pin failure blocks program teardown");
        assert!(f.app.get(id).is_ok());
        assert!(f.app.get_link(link.id).expect("pin retained").pin_present);
        f.kernel.clear();
        let app = Bpfman::new(
            ActiveStore::open(f.store.clone(), &f.layout, Duration::from_secs(1)).expect("reopen"),
            f.kernel.clone(),
            Duration::from_secs(1),
        );
        let _report = app.retry_unload(error).expect("reopened consuming retry");
        f.clean();
    }
    let f = Fixture::new(backend);
    let id = load(&f);
    let link = f.app.attach_tcx(request(id, 0)).expect("attach");
    f.store.set(Some(StorePoint::DeleteLink));
    let error = f
        .app
        .detach(link.id)
        .expect_err("record failure retains receipt");
    assert!(
        !f.app
            .get_link(link.id)
            .expect("record survives")
            .pin_present
    );
    f.store.set(None);
    let _report = f.app.retry_link_cleanup(error).expect("record-only retry");
    let cancellation = Cancellation::new();
    f.kernel.cancel_at(Point::PinLink, &cancellation);
    f.app
        .attach_tcx_with_cancellation(request(id, 0), &cancellation)
        .expect_err("precommit cancellation compensates");
    f.kernel.clear();
    assert!(
        f.app
            .list_link_records()
            .expect("no cancelled intent")
            .is_empty()
    );
    let _report = f.app.unload(id).expect("unload");
    f.clean();
}
