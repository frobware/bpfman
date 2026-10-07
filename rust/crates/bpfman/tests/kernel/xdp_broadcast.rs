//! DEVMAP fan-out, ingress exclusion, and restoration of the complete packet path.
#![allow(clippy::panic)]

use super::{
    faults::{Faults, Point},
    support::*,
    xdp_delivery::{Frames, Network, count},
    xdp_devmap::Targets,
};
use bpfman_model::*;
use bpfman_runtime::{ActiveStore, Bpfman, PreparedProgram, TracepointAttach, XdpAttach};
use bpfman_store::*;

fn scenario<S>(backend: S, mode: XdpMode, frames: Frames, exclude: bool, keep_redirect: bool)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + XdpReplacementStore + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    let c = Context::new();
    let network = Network::broadcast(frames);
    let faults = Faults::new(backend);
    let app = Bpfman::new(
        ActiveStore::open(faults.clone(), &c.layout, TIMEOUT).expect("store"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    let load = |object, symbol: &str, globals| {
        app.load(
            PreparedProgram::new(
                &bpfman_kernel_aya::Kernel,
                &fixture(object),
                ProgramSpec::Xdp(symbol.try_into().expect("symbol")),
                Default::default(),
            )
            .expect("prepare")
            .with_globals(&bpfman_kernel_aya::Kernel, globals)
            .expect("globals"),
        )
        .expect("load")
        .record
        .id
    };
    // Linux UAPI: BROADCAST is bit 3; EXCLUDE_INGRESS is bit 4.
    let flags: u32 = (1 << 3) | if exclude { 1 << 4 } else { 0 };
    let redirect = load(
        if frames == Frames::MultiBuffer {
            "xdp_devmap_broadcast_frags.bpf.o"
        } else {
            "xdp_devmap_broadcast.bpf.o"
        },
        "devmap_delivery",
        [("devmap_flags".into(), flags.to_ne_bytes().to_vec())].into(),
    );
    let tail = load(
        frames.delivery_object(),
        "delivery_tail",
        Default::default(),
    );
    let (object, symbol) = frames.observer();
    let observer = load(object, symbol, Default::default());
    let ids = [redirect, tail, observer];
    let targets = Targets::open(&c, redirect);
    targets.check_entries(&[None; 3]);
    let format = std::fs::read_to_string("/sys/kernel/tracing/events/xdp/xdp_redirect_err/format")
        .expect("XDP redirect error tracepoint format");
    let format: String = format.chars().filter(|c| !c.is_whitespace()).collect();
    for field in [
        "field:intifindex;offset:16;size:4;",
        "field:interr;offset:20;size:4;",
        "field:u32map_id;offset:28;size:4;",
    ] {
        assert!(
            format.contains(field),
            "unsupported tracepoint field: {field}"
        );
    }
    let input_index = u32::try_from(network.links("in0")[0]["ifindex"].as_u64().expect("index"))
        .expect("u32 index");
    let errors = app
        .load(
            PreparedProgram::new(
                &bpfman_kernel_aya::Kernel,
                &fixture("xdp_redirect_error.bpf.o"),
                ProgramSpec::Tracepoint("redirect_error".try_into().expect("symbol")),
                Default::default(),
            )
            .expect("prepare error observation")
            .with_globals(
                &bpfman_kernel_aya::Kernel,
                [
                    (
                        "selected_map_id".into(),
                        targets.id().to_ne_bytes().to_vec(),
                    ),
                    (
                        "selected_ifindex".into(),
                        input_index.to_ne_bytes().to_vec(),
                    ),
                ]
                .into(),
            )
            .expect("error filters"),
        )
        .expect("load error observation")
        .record
        .id;
    app.attach_tracepoint(TracepointAttach {
        program_id: errors,
        target: "xdp/xdp_redirect_err".parse().expect("target"),
        metadata: Default::default(),
    })
    .expect("observe redirect errors");
    let traffic = |execution, forward: [u32; 4]| {
        let eligible = forward
            .iter()
            .enumerate()
            .filter(|(i, n)| *i != 1 && **n > 0)
            .count();
        // Linux 6.18.54 explicitly rejects cloning non-linear SKBs/XDP frames.
        // Keep proving real fragment input and require the exact kernel errno;
        // empty maps consume packets, and one eligible target still forwards.
        let rejected = frames == Frames::MultiBuffer && eligible > 1;
        let delivery = if rejected { [0; 4] } else { forward };
        let before = [count(&c, errors, 0), count(&c, errors, 1)];
        frames.traffic(&c, &network, ids, execution, delivery);
        let after = [count(&c, errors, 0), count(&c, errors, 1)];
        assert_eq!(
            [after[0] - before[0], after[1] - before[1]],
            [if rejected { 3 } else { 0 }; 2],
            "all rejected fan-out frames must report EOPNOTSUPP; no other redirect errors"
        );
    };
    let request = |id, interface: &str, requested_mode, priority, proceed_on| XdpAttach {
        program_id: id,
        interface: interface.parse().expect("interface"),
        netns: network.namespace(),
        mode: requested_mode,
        priority,
        proceed_on,
        metadata: Default::default(),
    };
    let observers: Vec<_> = ["source0", "sink0", "sink1"]
        .into_iter()
        .map(|interface| {
            app.attach_xdp(request(
                observer,
                interface,
                XdpMode::Drv,
                50,
                Default::default(),
            ))
            .expect("receiving peer")
            .id
        })
        .collect();
    let first = app
        .attach_xdp(request(redirect, "in0", mode, 50, Default::default()))
        .expect("broadcast attachment");
    assert_eq!(
        network.links("in0")[0]["xdp"]["attached"][0]["mode"],
        if mode == XdpMode::Drv { 1 } else { 2 },
        "explicit ingress mode must execute without fallback"
    );
    let LinkDetails::Xdp(details) = &first.details else {
        panic!("XDP link");
    };
    let old = app.get_xdp_dispatcher(details.key).expect("snapshot");
    let outer_id = old.members()[0].outer_link_id;
    let returned = if exclude { 0 } else { 3 };
    let forward = [returned, 0, 3, 3];

    // Broadcast ignores key 99 and PASS fallback: an empty map consumes packets.
    traffic([3, 0], [0; 4]);
    let input = targets.set_at(&network, 0, "in0");
    traffic([3, 0], [returned, 0, 0, 0]);
    let output0 = targets.set_at(&network, 1, "out0");
    let output1 = targets.set_at(&network, 2, "out1");
    let entries = [Some(input), Some(output0), Some(output1)];
    targets.check_entries(&entries);
    traffic([3, 0], forward);

    targets.delete_at(&network, 1);
    targets.check_entries(&[Some(input), None, Some(output1)]);
    traffic([3, 0], [returned, 0, 0, 3]);
    targets.set_at(&network, 2, "out0");
    targets.check_entries(&[Some(input), None, Some(output0)]);
    traffic([3, 0], [returned, 0, 3, 0]);
    targets.delete_at(&network, 2);
    targets.set_at(&network, 1, "out1");
    targets.check_entries(&[Some(input), Some(output1), None]);
    traffic([3, 0], [returned, 0, 0, 3]);
    targets.set_at(&network, 1, "out0");
    targets.set_at(&network, 2, "out1");
    assert_eq!(
        app.get_xdp_dispatcher(details.key).expect("same revision"),
        old
    );

    faults.set(Some(Point::XdpReplace));
    let error = app
        .attach_xdp(request(tail, "in0", mode, 60, Default::default()))
        .expect_err("attach publication failure");
    assert_eq!(error.unresolved(), 0);
    assert_eq!(error.restoration_attempts().len(), 1);
    assert!(error.restoration_attempts()[0].is_ok());
    assert_eq!(app.get_xdp_dispatcher(details.key).expect("restored"), old);
    targets.check_entries(&entries);
    traffic([3, 0], forward);
    faults.set(None);

    let second = app
        .attach_xdp(request(tail, "in0", mode, 60, Default::default()))
        .expect("tail attachment");
    let chain = app.get_xdp_dispatcher(details.key).expect("chain");
    assert_eq!(
        chain
            .members()
            .iter()
            .map(|m| m.member.id)
            .collect::<Vec<_>>(),
        [first.id, second.id]
    );
    assert_eq!(chain.members()[0].outer_link_id, outer_id);
    targets.check_entries(&entries);
    traffic([3, 0], forward);
    for key in 0..3 {
        targets.delete_at(&network, key);
    }
    targets.check_entries(&[None; 3]);
    // Even with no targets, REDIRECT terminates before the DROP member.
    traffic([3, 0], [0; 4]);
    for (key, iface) in [(0, "in0"), (1, "out0"), (2, "out1")] {
        targets.set_at(&network, key, iface);
    }

    let (removed, survivor) = if keep_redirect {
        (second.id, first.id)
    } else {
        (first.id, second.id)
    };
    faults.set(Some(Point::XdpReplace));
    let error = app
        .detach_xdp(removed)
        .expect_err("detach publication failure");
    assert_eq!(error.unresolved(), 0);
    assert_eq!(error.restoration_attempts().len(), 1);
    assert!(error.restoration_attempts()[0].is_ok());
    assert_eq!(
        app.get_xdp_dispatcher(details.key).expect("restored"),
        chain
    );
    targets.check_entries(&entries);
    traffic([3, 0], forward);
    faults.set(None);

    app.detach_xdp(removed).expect("remove member");
    let remaining = app.get_xdp_dispatcher(details.key).expect("survivor");
    assert_eq!(remaining.members().len(), 1);
    assert_eq!(remaining.members()[0].member.id, survivor);
    assert_eq!(remaining.members()[0].outer_link_id, outer_id);
    targets.check_entries(&entries);
    if keep_redirect {
        traffic([3, 0], forward);
        targets.delete_at(&network, 2);
        traffic([3, 0], [returned, 0, 3, 0]);
        targets.set_at(&network, 2, "out1");
        traffic([3, 0], forward);
    } else {
        traffic([0, 3], [0; 4]);
    }
    app.detach_xdp(survivor).expect("last detach");
    assert!(app.get_xdp_dispatcher(details.key).is_err());
    targets.check_entries(&entries);
    traffic([0, 0], [0, 3, 0, 0]);

    // Explicit REDIRECT continuation suppresses all copies. An exhausted chain
    // returns PASS; adding DROP must stop local delivery too, even with an empty map.
    let mask = (XdpProceedOn::default().mask() | (1 << 4))
        .try_into()
        .expect("REDIRECT mask");
    let continuing = app
        .attach_xdp(request(redirect, "in0", mode, 50, mask))
        .expect("continuing redirect");
    traffic([3, 0], [0, 3, 0, 0]);
    let stopping = app
        .attach_xdp(request(tail, "in0", mode, 60, Default::default()))
        .expect("stopping tail");
    targets.check_entries(&entries);
    traffic([3, 3], [0; 4]);
    for key in 0..3 {
        targets.delete_at(&network, key);
    }
    traffic([3, 3], [0; 4]);
    for (key, iface) in [(0, "in0"), (1, "out0"), (2, "out1")] {
        targets.set_at(&network, key, iface);
    }
    app.detach_xdp(stopping.id).expect("remove stopping tail");
    traffic([3, 0], [0, 3, 0, 0]);
    app.detach_xdp(continuing.id)
        .expect("remove continuing redirect");
    traffic([0, 0], [0, 3, 0, 0]);

    for id in observers {
        app.detach_xdp(id).expect("remove receiving peer");
    }
    assert_eq!(app.unload(tail).expect("unload tail").unresolved(), 0);
    targets.check_entries(&entries);
    assert_eq!(
        app.unload(redirect).expect("unload redirect").unresolved(),
        0
    );
    targets.assert_unloaded();
    assert_eq!(
        app.unload(errors)
            .expect("unload error observation")
            .unresolved(),
        0
    );
    assert_eq!(
        app.unload(observer).expect("unload observer").unresolved(),
        0
    );
    assert!(app.list(&Default::default()).expect("programs").is_empty());
    assert!(app.list_link_records().expect("links").is_empty());
    assert!(app.list_xdp_dispatchers().expect("dispatchers").is_empty());
    c.no_artifacts();
}

pub(super) fn exercise<S>(backend: S, mode: XdpMode, frames: Frames)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + XdpReplacementStore + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    for exclude in [false, true] {
        for keep_redirect in [false, true] {
            scenario(backend.clone(), mode, frames, exclude, keep_redirect);
        }
    }
}
