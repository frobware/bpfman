//! DEVMAP packet forwarding and pinned map lifetime across dispatcher revisions.
#![allow(clippy::panic)]

use super::{
    faults::{Faults, Point},
    support::*,
    xdp_delivery::{Network, traffic},
};
use bpfman_model::*;
use bpfman_runtime::{ActiveStore, Bpfman, PreparedProgram, XdpAttach};
use bpfman_store::*;
use std::{num::NonZeroU32, path::PathBuf};

struct Targets {
    pin: PathBuf,
    id: u32,
    map: aya::maps::xdp::DevMap<aya::maps::MapData>,
}

impl Targets {
    fn open(c: &Context, program: NonZeroU32) -> Self {
        let pin = c
            .layout
            .map_directory_path(program)
            .join("delivery_targets");
        let data = aya::maps::MapData::from_pin(&pin).expect("DEVMAP pin");
        let id = data.info().expect("DEVMAP info").id();
        let map = aya::maps::Map::DevMap(data).try_into().expect("DEVMAP");
        let targets = Self { pin, id, map };
        let info = aya::programs::ProgramInfo::from_pin(c.layout.program_pin_path(program))
            .expect("managed extension");
        assert!(
            info.map_ids()
                .expect("maps")
                .expect("map IDs")
                .contains(&id)
        );
        targets.check(None);
        targets
    }

    fn check(&self, expected: Option<u32>) {
        assert_eq!(
            aya::maps::MapInfo::from_pin(&self.pin)
                .expect("current pin")
                .id(),
            self.id,
            "replacement must preserve the published map identity"
        );
        match expected {
            Some(index) => {
                let value = self.map.get(0, 0).expect("target");
                assert_eq!(
                    value.if_index, index,
                    "target contents must survive rebuilding"
                );
                assert_eq!(value.prog_id, None, "no egress program in this slice");
            }
            None => assert!(matches!(
                self.map.get(0, 0),
                Err(aya::maps::MapError::KeyNotFound)
            )),
        }
    }

    fn set(&self, network: &Network, interface: &str) -> u32 {
        // Resolve the netdevice in the private namespace, never in the caller's.
        network.probe(&[
            "devmap-set",
            self.pin.to_str().expect("pin path"),
            interface,
        ]);
        let index = u32::try_from(
            network.links(interface)[0]["ifindex"]
                .as_u64()
                .expect("index"),
        )
        .expect("u32 index");
        self.check(Some(index));
        index
    }

    fn delete(&self, network: &Network) {
        network.probe(&["devmap-delete", self.pin.to_str().expect("pin path")]);
        self.check(None);
    }
}

fn map_released(id: u32) {
    // Program reclamation drops used maps after an RCU grace period and workqueue pass.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match aya::maps::MapInfo::from_id(id) {
            Err(aya::maps::MapError::SyscallError(error))
                if error.io_error.kind() == std::io::ErrorKind::NotFound =>
            {
                return;
            }
            Ok(_) => {}
            Err(error) => panic!("unexpected map observation: {error}"),
        }
        assert!(
            std::time::Instant::now() < deadline,
            "unloaded DEVMAP {id} leaked"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn scenario<S>(backend: S, mode: XdpMode, fallback: u32, proceed: bool, keep_redirect: bool)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + XdpReplacementStore + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    let c = Context::new();
    let network = Network::new();
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
    let redirect = load(
        "xdp_devmap.bpf.o",
        "devmap_delivery",
        [("devmap_fallback".into(), fallback.to_ne_bytes().to_vec())].into(),
    );
    let tail = load("xdp_delivery.bpf.o", "delivery_tail", Default::default());
    let observer = load("xdp_pass.bpf.o", "pass", Default::default());
    let ids = [redirect, tail];
    let targets = Targets::open(&c, redirect);
    let request = |id, interface: &str, requested_mode, priority, proceed_on| XdpAttach {
        program_id: id,
        interface: interface.parse().expect("interface"),
        netns: network.namespace(),
        mode: requested_mode,
        priority,
        proceed_on,
        metadata: Default::default(),
    };
    let observers: Vec<_> = ["source0", "sink0"]
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
    let mask = if proceed {
        (XdpProceedOn::default().mask() | (1 << 4))
            .try_into()
            .expect("REDIRECT mask")
    } else {
        Default::default()
    };
    let first = app
        .attach_xdp(request(redirect, "in0", mode, 50, mask))
        .expect("first attachment");
    assert_eq!(
        network.links("in0")[0]["xdp"]["attached"][0]["mode"],
        if mode == XdpMode::Drv { 1 } else { 2 },
        "explicit mode must execute without fallback"
    );
    let LinkDetails::Xdp(details) = &first.details else {
        panic!("XDP link");
    };
    let old = app.get_xdp_dispatcher(details.key).expect("snapshot");
    let outer_id = old.members()[0].outer_link_id;
    let missing_delivery = if fallback == 2 { [0, 3, 0] } else { [0, 0, 0] };
    let single_delivery = if proceed { [0, 3, 0] } else { [0, 0, 3] };
    let chain_delivery = if proceed { [0, 0, 0] } else { [0, 0, 3] };
    let chain_execution = [3, if proceed { 3 } else { 0 }];
    traffic(&c, &network, ids, [3, 0], missing_delivery);

    let out_index = targets.set(&network, "out0");
    traffic(&c, &network, ids, [3, 0], single_delivery);
    // The next packet must use a live map update without rebuilding the dispatcher.
    targets.set(&network, "in0");
    traffic(
        &c,
        &network,
        ids,
        [3, 0],
        if proceed { [0, 3, 0] } else { [3, 0, 0] },
    );
    targets.set(&network, "out0");
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
    targets.check(Some(out_index));
    traffic(&c, &network, ids, [3, 0], single_delivery);
    faults.set(None);

    let second = app
        .attach_xdp(request(tail, "in0", mode, 60, Default::default()))
        .expect("tail");
    let chain = app.get_xdp_dispatcher(details.key).expect("chain");
    assert_eq!(
        chain
            .members()
            .iter()
            .map(|member| member.member.id)
            .collect::<Vec<_>>(),
        [first.id, second.id]
    );
    assert_eq!(chain.members()[0].outer_link_id, outer_id);
    targets.check(Some(out_index));
    traffic(&c, &network, ids, chain_execution, chain_delivery);

    targets.delete(&network);
    // PASS fallback reaches the tail; DROP fallback stops before it.
    traffic(
        &c,
        &network,
        ids,
        [3, if fallback == 2 { 3 } else { 0 }],
        [0, 0, 0],
    );
    targets.set(&network, "in0");
    traffic(
        &c,
        &network,
        ids,
        chain_execution,
        if proceed { [0, 0, 0] } else { [3, 0, 0] },
    );
    targets.set(&network, "out0");

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
    targets.check(Some(out_index));
    traffic(&c, &network, ids, chain_execution, chain_delivery);
    faults.set(None);

    app.detach_xdp(removed).expect("remove member");
    let remaining = app.get_xdp_dispatcher(details.key).expect("survivor");
    assert_eq!(remaining.members().len(), 1);
    assert_eq!(remaining.members()[0].member.id, survivor);
    assert_eq!(remaining.members()[0].outer_link_id, outer_id);
    targets.check(Some(out_index));
    if keep_redirect {
        traffic(&c, &network, ids, [3, 0], single_delivery);
        targets.delete(&network);
        traffic(&c, &network, ids, [3, 0], missing_delivery);
        targets.set(&network, "out0");
        traffic(&c, &network, ids, [3, 0], single_delivery);
    } else {
        traffic(&c, &network, ids, [0, 3], [0, 0, 0]);
    }
    app.detach_xdp(survivor).expect("last detach");
    assert!(app.get_xdp_dispatcher(details.key).is_err());
    targets.check(Some(out_index));
    traffic(&c, &network, ids, [0, 0], [0, 3, 0]);

    for id in observers {
        app.detach_xdp(id).expect("remove receiving peer");
    }
    assert_eq!(app.unload(tail).expect("unload tail").unresolved(), 0);
    targets.check(Some(out_index));
    assert_eq!(
        app.unload(redirect).expect("unload redirect").unresolved(),
        0
    );
    assert!(
        !targets.pin.exists(),
        "unload must remove the owned map pin"
    );
    let map_id = targets.id;
    drop(targets);
    map_released(map_id);
    assert_eq!(
        app.unload(observer).expect("unload observer").unresolved(),
        0
    );
    assert!(app.list(&Default::default()).expect("programs").is_empty());
    assert!(app.list_link_records().expect("links").is_empty());
    assert!(app.list_xdp_dispatchers().expect("dispatchers").is_empty());
    c.no_artifacts();
}

pub(super) fn exercise<S>(backend: S, mode: XdpMode)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + XdpReplacementStore + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    for fallback in [1, 2] {
        for proceed in [false, true] {
            for keep_redirect in [false, true] {
                scenario(backend.clone(), mode, fallback, proceed, keep_redirect);
            }
        }
    }
}
