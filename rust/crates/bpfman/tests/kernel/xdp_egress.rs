//! Native DEVMAP/DEVMAP_HASH egress loading, packet execution, and kernel-held references.
#![allow(clippy::panic)]

use super::{
    faults::{Faults, Point},
    support::*,
    xdp_delivery::{Frames, Network},
    xdp_devmap::Targets,
};
use bpfman_kernel::ProgramObservations;
use bpfman_model::*;
use bpfman_runtime::{ActiveStore, Bpfman, PreparedProgram, XdpAttach};
use bpfman_store::*;
use std::num::NonZeroU32;

#[derive(Clone, Copy)]
enum Kind {
    Array,
    Hash,
}

impl Kind {
    fn object(self, frames: Frames) -> &'static str {
        match (self, frames) {
            (Self::Array, _) => frames.devmap_object(),
            (Self::Hash, Frames::Linear) => "xdp_devmap_hash.bpf.o",
            (Self::Hash, Frames::MultiBuffer) => "xdp_devmap_hash_frags.bpf.o",
        }
    }

    fn key(self) -> u32 {
        match self {
            Self::Array => 0,
            Self::Hash => 0x8000_0001,
        }
    }

    fn open(self, c: &Context, program: NonZeroU32) -> Targets {
        match self {
            Self::Array => Targets::open(c, program),
            Self::Hash => Targets::open_hash(c, program, 2),
        }
    }

    fn check_empty(self, targets: &Targets) {
        match self {
            Self::Array => targets.check_entries(&[None]),
            Self::Hash => targets.check_keyed_entries(&[(self.key(), None), (u32::MAX, None)]),
        }
    }
}

struct Counters(aya::maps::PerCpuArray<aya::maps::MapData, u64>);

impl Counters {
    fn open(c: &Context, id: NonZeroU32) -> Self {
        let data =
            aya::maps::MapData::from_pin(c.layout.map_directory_path(id).join("delivery_stats"))
                .expect("egress counters");
        Self(
            aya::maps::Map::PerCpuArray(data)
                .try_into()
                .expect("counters"),
        )
    }

    fn values(&self, frames: Frames) -> Vec<u64> {
        let keys = if frames == Frames::MultiBuffer {
            vec![0, 1, 2, 3, 7]
        } else {
            vec![0, 1]
        };
        keys.into_iter()
            .map(|key| self.0.get(&key, 0).expect("counter").iter().sum())
            .collect()
    }
}

fn program_released(id: NonZeroU32) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match bpfman_kernel_aya::Kernel.program(id) {
            Err(error) if error.kind() == bpfman_kernel::ErrorKind::Missing => {
                return;
            }
            Ok(_) => {}
            Err(error) => panic!("unexpected program observation: {error}"),
        }
        assert!(
            std::time::Instant::now() < deadline,
            "DEVMAP egress {id} leaked"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn exercise_kind<S>(backend: S, mode: XdpMode, frames: Frames, kind: Kind)
where
    S: OpenStore
        + CommitLoad
        + UnloadStore
        + LinkStore
        + XdpReplacementStore
        + bpfman_store::TcStore
        + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    let c = Context::new();
    let network = Network::new();
    frames.configure(&network);
    let egress_object = match frames {
        Frames::Linear => "xdp_devmap_egress.bpf.o",
        Frames::MultiBuffer => "xdp_devmap_egress_frags.bpf.o",
    };
    let prepare = |object, symbol: &str| {
        PreparedProgram::new(
            &bpfman_kernel_aya::Kernel,
            &fixture(object),
            ProgramSpec::Xdp(symbol.try_into().expect("symbol")),
            Default::default(),
        )
    };
    // CPUMAP must not silently become a dispatcher extension.
    assert!(prepare(egress_object, "cpumap_egress").is_err());
    assert!(
        !c.layout.root().exists(),
        "unsupported role has no runtime effects"
    );
    let faults = Faults::new(backend.clone());
    let app = Bpfman::new(
        ActiveStore::open(faults.clone(), &c.layout, TIMEOUT).expect("store"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    // Native XDP takes the same ownership/compensation path as other programs.
    faults.set(Some(Point::Commit));
    let failure = app
        .load(prepare(egress_object, "devmap_egress").expect("prepare"))
        .expect_err("load publication failure");
    assert_eq!(failure.unresolved(), 0);
    c.no_artifacts();
    faults.set(None);
    let load = |object, symbol: &str, globals| {
        app.load(
            prepare(object, symbol)
                .expect("prepare")
                .with_globals(&bpfman_kernel_aya::Kernel, globals)
                .expect("globals"),
        )
        .expect("load")
        .record
        .id
    };
    let redirect = load(kind.object(frames), "devmap_delivery", Default::default());
    let tail = load(
        frames.delivery_object(),
        "delivery_tail",
        Default::default(),
    );
    let (object, symbol) = frames.observer();
    let observer = load(object, symbol, Default::default());
    let index = |interface: &str| {
        u32::try_from(
            network.links(interface)[0]["ifindex"]
                .as_u64()
                .expect("ifindex"),
        )
        .expect("u32")
    };
    let ingress_index = index("in0");
    let out_index = index("out0");
    let egress = |action: u32| {
        load(
            egress_object,
            "devmap_egress",
            [
                ("egress_action".into(), action.to_ne_bytes().to_vec()),
                (
                    "expected_ingress".into(),
                    ingress_index.to_ne_bytes().to_vec(),
                ),
                ("expected_egress".into(), out_index.to_ne_bytes().to_vec()),
            ]
            .into(),
        )
    };
    let pass = egress(2);
    let drop = egress(1);
    let incompatible = load(
        match frames {
            Frames::Linear => "xdp_devmap_egress_frags.bpf.o",
            Frames::MultiBuffer => "xdp_devmap_egress.bpf.o",
        },
        "devmap_egress",
        Default::default(),
    );
    let pass_counters = Counters::open(&c, pass);
    let drop_counters = Counters::open(&c, drop);
    for id in [pass, drop] {
        let info = aya::programs::ProgramInfo::from_pin(c.layout.program_pin_path(id))
            .expect("egress program");
        assert_eq!(info.program_type(), aya::programs::ProgramType::Xdp.into());
        assert_eq!(info.id(), id.get());
        assert!(app.get(id).expect("managed egress").record.links.is_empty());
    }
    let request = |id, interface: &str, requested_mode, priority| XdpAttach {
        program_id: id,
        interface: interface.parse().expect("interface"),
        netns: network.namespace(),
        mode: requested_mode,
        priority,
        proceed_on: Default::default(),
        metadata: Default::default(),
    };
    // Reopening preserves the role without a new persistence field.
    let reopened = Bpfman::new(
        ActiveStore::open(backend, &c.layout, TIMEOUT).expect("reopen"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    let failure = reopened
        .attach_xdp(request(pass, "in0", mode, 50))
        .expect_err("egress cannot attach to interface");
    assert_eq!(failure.unresolved(), 0);
    assert!(app.list_link_records().expect("links").is_empty());
    assert!(app.list_xdp_dispatchers().expect("dispatchers").is_empty());
    assert!(names(&c.layout.root().join("fs/xdp")).is_empty());
    let receiving = app
        .attach_xdp(request(observer, "sink0", XdpMode::Drv, 50))
        .expect("receiver");
    let first = app
        .attach_xdp(request(redirect, "in0", mode, 50))
        .expect("redirect");
    assert_eq!(
        network.links("in0")[0]["xdp"]["attached"][0]["mode"],
        if mode == XdpMode::Drv { 1 } else { 2 }
    );
    let LinkDetails::Xdp(details) = &first.details else {
        panic!("XDP link")
    };
    let original = app.get_xdp_dispatcher(details.key).expect("snapshot");
    let targets = kind.open(&c, redirect);
    let traffic = |execution, delivery, pass_runs, drop_runs| {
        let before_pass = pass_counters.values(frames);
        let before_drop = drop_counters.values(frames);
        frames.traffic(
            &c,
            &network,
            [redirect, tail, observer],
            execution,
            delivery,
        );
        for (counters, before, expected) in [
            (&pass_counters, before_pass, pass_runs),
            (&drop_counters, before_drop, drop_runs),
        ] {
            for (after, before) in counters.values(frames).into_iter().zip(before) {
                assert_eq!(
                    after - before,
                    expected,
                    "egress execution, fragments, payload, and ingress/egress context"
                );
            }
        }
    };
    targets.reject_egress(&c, &network, redirect);
    kind.check_empty(&targets);
    if matches!(kind, Kind::Hash) {
        targets.set_at(&network, u32::MAX, "in0");
    }
    let check = |expected| match kind {
        Kind::Array => match expected {
            Some(program) => targets.check_egress(out_index, program),
            None => targets.check_entries(&[None]),
        },
        Kind::Hash => {
            targets.check_hash_egress(expected.map(|program| (out_index, program)), ingress_index)
        }
    };
    check(None);
    traffic([3, 0], [0, 3, 0], 0, 0);
    assert_eq!(targets.set_egress(&c, &network, pass), out_index);
    check(Some(pass));
    traffic([3, 0], [0, 0, 3], 3, 0);
    targets.reject_egress(&c, &network, tail);
    check(Some(pass));
    // The first native egress initializes hash ownership. Later updates must
    // retain fragment compatibility even though load-time ingress checks omit
    // DEVMAP_HASH on Linux 6.18.54 (include/linux/bpf.h: map_type_contains_progs).
    targets.reject_egress(&c, &network, incompatible);
    check(Some(pass));
    traffic([3, 0], [0, 0, 3], 3, 0);
    targets.set_egress(&c, &network, drop);
    check(Some(drop));
    traffic([3, 0], [0, 0, 0], 0, 3);
    assert_eq!(
        app.get_xdp_dispatcher(details.key)
            .expect("unchanged revision"),
        original
    );

    faults.set(Some(Point::XdpReplace));
    let failure = app
        .attach_xdp(request(tail, "in0", mode, 60))
        .expect_err("publication rollback");
    assert_eq!(failure.unresolved(), 0);
    assert_eq!(failure.restoration_attempts().len(), 1);
    assert!(failure.restoration_attempts()[0].is_ok());
    assert_eq!(
        app.get_xdp_dispatcher(details.key).expect("restored"),
        original
    );
    check(Some(drop));
    traffic([3, 0], [0, 0, 0], 0, 3);
    faults.set(None);
    let second = app
        .attach_xdp(request(tail, "in0", mode, 60))
        .expect("tail");
    let chain = app.get_xdp_dispatcher(details.key).expect("chain");
    assert_eq!(chain.members().len(), 2);
    assert_eq!(
        chain.members()[0].outer_link_id,
        original.members()[0].outer_link_id
    );
    check(Some(drop));
    traffic([3, 0], [0, 0, 0], 0, 3);
    targets.set_egress(&c, &network, pass);
    check(Some(pass));
    traffic([3, 0], [0, 0, 3], 3, 0);
    faults.set(Some(Point::XdpReplace));
    let failure = app.detach_xdp(second.id).expect_err("detach rollback");
    assert_eq!(failure.unresolved(), 0);
    assert_eq!(failure.restoration_attempts().len(), 1);
    assert!(failure.restoration_attempts()[0].is_ok());
    assert_eq!(
        app.get_xdp_dispatcher(details.key).expect("restored chain"),
        chain
    );
    check(Some(pass));
    traffic([3, 0], [0, 0, 3], 3, 0);
    faults.set(None);
    app.detach_xdp(second.id).expect("surviving redirect");
    check(Some(pass));
    traffic([3, 0], [0, 0, 3], 3, 0);

    // Unload removes owned pins/record, but the DEVMAP keeps its program alive.
    faults.set(Some(Point::DeleteProgram));
    let failure = app.unload(pass).expect_err("native XDP teardown failure");
    assert!(!c.layout.program_pin_path(pass).exists());
    check(Some(pass));
    traffic([3, 0], [0, 0, 3], 3, 0);
    faults.set(None);
    assert_eq!(
        app.retry_unload(failure)
            .expect("retry native teardown")
            .unresolved(),
        0
    );
    c.absent(pass);
    assert!(app.get(pass).is_err());
    assert!(
        bpfman_kernel_aya::Kernel.program(pass).is_ok(),
        "map-held program survives unpinning"
    );
    check(Some(pass));
    traffic([3, 0], [0, 0, 3], 3, 0);
    targets.delete_at(&network, kind.key());
    check(None);
    program_released(pass);
    traffic([3, 0], [0, 3, 0], 0, 0);

    targets.set_egress(&c, &network, drop);
    check(Some(drop));
    assert_eq!(app.unload(drop).expect("unpin DROP").unresolved(), 0);
    c.absent(drop);
    check(Some(drop));
    traffic([3, 0], [0, 0, 0], 0, 3);
    app.detach_xdp(first.id).expect("last detach");
    check(Some(drop));
    traffic([0, 0], [0, 3, 0], 0, 0);
    assert_eq!(
        app.unload(redirect).expect("unload map owner").unresolved(),
        0
    );
    assert!(
        bpfman_kernel_aya::Kernel.program(drop).is_ok(),
        "retained map FD still owns egress"
    );
    targets.assert_unloaded();
    program_released(drop);
    assert_eq!(
        app.unload(incompatible)
            .expect("incompatible cleanup")
            .unresolved(),
        0
    );
    program_released(incompatible);
    assert_eq!(app.unload(tail).expect("tail cleanup").unresolved(), 0);
    app.detach_xdp(receiving.id).expect("receiver detach");
    assert_eq!(
        app.unload(observer).expect("receiver cleanup").unresolved(),
        0
    );
    assert!(app.list(&Default::default()).expect("programs").is_empty());
    assert!(app.list_link_records().expect("links").is_empty());
    assert!(app.list_xdp_dispatchers().expect("dispatchers").is_empty());
    c.no_artifacts();
}

/// Preserve the upstream-Aya boundary while proving jumbo unicast still works.
pub(super) fn fragments_boundary<S>(backend: S, mode: XdpMode)
where
    S: OpenStore
        + CommitLoad
        + UnloadStore
        + LinkStore
        + XdpReplacementStore
        + bpfman_store::TcStore
        + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    let c = Context::new();
    let network = Network::new();
    let frames = Frames::MultiBuffer;
    frames.configure(&network);
    let app = Bpfman::new(
        ActiveStore::open(backend, &c.layout, TIMEOUT).expect("store"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    let load = |object, symbol: &str| {
        app.load(
            PreparedProgram::new(
                &bpfman_kernel_aya::Kernel,
                &fixture(object),
                ProgramSpec::Xdp(symbol.try_into().expect("symbol")),
                Default::default(),
            )
            .expect("prepare"),
        )
        .expect("load")
        .record
        .id
    };
    let redirect = load(frames.devmap_object(), "devmap_delivery");
    let tail = load(frames.delivery_object(), "delivery_tail");
    let (object, symbol) = frames.observer();
    let observer = load(object, symbol);
    let egress = load("xdp_devmap_egress_frags.bpf.o", "devmap_egress");
    let counters = Counters::open(&c, egress);
    assert_eq!(
        aya::programs::ProgramInfo::from_pin(c.layout.program_pin_path(egress))
            .expect("egress")
            .program_type(),
        aya::programs::ProgramType::Xdp.into()
    );
    let request = |id, interface: &str, requested_mode| XdpAttach {
        program_id: id,
        interface: interface.parse().expect("interface"),
        netns: network.namespace(),
        mode: requested_mode,
        priority: 50,
        proceed_on: Default::default(),
        metadata: Default::default(),
    };
    assert_eq!(
        app.attach_xdp(request(egress, "in0", mode))
            .expect_err("egress role")
            .unresolved(),
        0
    );
    let receiver = app
        .attach_xdp(request(observer, "sink0", XdpMode::Drv))
        .expect("receiver");
    let link = app
        .attach_xdp(request(redirect, "in0", mode))
        .expect("redirect");
    assert_eq!(
        network.links("in0")[0]["xdp"]["attached"][0]["mode"],
        if mode == XdpMode::Drv { 1 } else { 2 }
    );
    let LinkDetails::Xdp(details) = &link.details else {
        panic!("XDP link")
    };
    let before = app.get_xdp_dispatcher(details.key).expect("revision");
    let targets = Targets::open(&c, redirect);
    // TODO: Replace this boundary with positive egress fragment/context proof
    // when upstream Aya preserves BPF_F_XDP_HAS_FRAGS on extensions. Do not
    // substitute a linear egress program: it cannot safely receive jumbo frames.
    targets.reject_egress(&c, &network, egress);
    targets.check_entries(&[None]);
    frames.traffic(&c, &network, [redirect, tail, observer], [3, 0], [0, 3, 0]);
    let index = targets.set_at(&network, 0, "out0");
    frames.traffic(&c, &network, [redirect, tail, observer], [3, 0], [0, 0, 3]);
    targets.reject_egress(&c, &network, egress);
    targets.check_entries(&[Some(index)]);
    assert_eq!(
        app.get_xdp_dispatcher(details.key).expect("unchanged"),
        before
    );
    frames.traffic(&c, &network, [redirect, tail, observer], [3, 0], [0, 0, 3]);
    assert!(
        counters.values(frames).iter().all(|&count| count == 0),
        "rejected egress never executes"
    );
    assert_eq!(app.unload(egress).expect("egress cleanup").unresolved(), 0);
    program_released(egress);
    app.detach_xdp(link.id).expect("last detach");
    assert_eq!(
        app.unload(redirect)
            .expect("map owner cleanup")
            .unresolved(),
        0
    );
    targets.assert_unloaded();
    assert_eq!(app.unload(tail).expect("tail cleanup").unresolved(), 0);
    app.detach_xdp(receiver.id).expect("receiver detach");
    assert_eq!(
        app.unload(observer).expect("observer cleanup").unresolved(),
        0
    );
    assert!(app.list(&Default::default()).expect("programs").is_empty());
    assert!(app.list_link_records().expect("links").is_empty());
    assert!(app.list_xdp_dispatchers().expect("dispatchers").is_empty());
    c.no_artifacts();
}

pub(super) fn exercise<S>(backend: S, mode: XdpMode)
where
    S: OpenStore
        + CommitLoad
        + UnloadStore
        + LinkStore
        + XdpReplacementStore
        + bpfman_store::TcStore
        + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    exercise_kind(backend, mode, Frames::Linear, Kind::Array);
}

pub(super) fn exercise_hash<S>(backend: S, mode: XdpMode)
where
    S: OpenStore
        + CommitLoad
        + UnloadStore
        + LinkStore
        + XdpReplacementStore
        + bpfman_store::TcStore
        + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    exercise_kind(backend, mode, Frames::Linear, Kind::Hash);
}

pub(super) fn fragments_hash<S>(backend: S, mode: XdpMode)
where
    S: OpenStore
        + CommitLoad
        + UnloadStore
        + LinkStore
        + XdpReplacementStore
        + bpfman_store::TcStore
        + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    exercise_kind(backend, mode, Frames::MultiBuffer, Kind::Hash);
}
