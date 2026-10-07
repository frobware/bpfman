//! TX/REDIRECT delivery, proceed-on continuation, and restoration on private veths.
#![allow(clippy::panic)]

use super::{
    faults::{Faults, Point},
    support::*,
};
use bpfman_model::*;
use bpfman_runtime::{ActiveStore, Bpfman, PreparedProgram, XdpAttach};
use bpfman_store::*;
use std::{num::NonZeroU32, process::Command};

fn ip(args: &[&str]) -> Vec<u8> {
    let output = Command::new("ip").args(args).output().expect("ip");
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

pub(super) struct Network(String);

impl Network {
    pub(super) fn new() -> Self {
        let network = Self(format!("bpfman-delivery-{}", std::process::id()));
        ip(&["netns", "add", &network.0]);
        for (a, b) in [("in0", "source0"), ("out0", "sink0")] {
            ip(&[
                "-n", &network.0, "link", "add", a, "type", "veth", "peer", "name", b,
            ]);
        }
        for (iface, mac) in [
            ("source0", "02:00:00:00:00:01"),
            ("in0", "02:00:00:00:00:02"),
            ("sink0", "02:00:00:00:00:03"),
            ("out0", "02:00:00:00:00:04"),
        ] {
            ip(&[
                "-n", &network.0, "link", "set", "dev", iface, "address", mac, "up",
            ]);
        }
        network
    }

    pub(super) fn links(&self, iface: &str) -> serde_json::Value {
        serde_json::from_slice(&ip(&[
            "-n", &self.0, "-j", "-d", "link", "show", "dev", iface,
        ]))
        .expect("link JSON")
    }

    pub(super) fn namespace(&self) -> NetworkNamespace {
        format!("/run/netns/{}", self.0).parse().expect("namespace")
    }

    pub(super) fn probe(&self, args: &[&str]) -> Vec<u8> {
        let probe = std::path::PathBuf::from(
            std::env::var_os("BPFMAN_SHELL_BIN_DIR").expect("Make test binary directory"),
        )
        .join("xdp-delivery-probe");
        let mut command = vec![
            "netns",
            "exec",
            &self.0,
            "timeout",
            "5s",
            probe.to_str().expect("probe path"),
        ];
        command.extend_from_slice(args);
        ip(&command)
    }

    fn packets(&self, expected: [u32; 3]) {
        let output = self.probe(&[]);
        let counts: [u32; 3] = serde_json::from_slice(&output).expect("capture counts");
        assert_eq!(counts, expected, "[returned, local, redirected]");
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        let output = Command::new("ip").args(["netns", "del", &self.0]).output();
        if !std::thread::panicking() {
            assert!(output.expect("namespace cleanup").status.success());
        }
    }
}

fn count(c: &Context, id: NonZeroU32, key: u32) -> u64 {
    let map = aya::maps::MapData::from_pin(c.layout.map_directory_path(id).join("delivery_stats"))
        .expect("delivery map");
    let map: aya::maps::PerCpuArray<_, u64> = aya::maps::Map::PerCpuArray(map)
        .try_into()
        .expect("per CPU array");
    map.get(&key, 0).expect("counter").iter().sum()
}

pub(super) fn traffic(
    c: &Context,
    network: &Network,
    ids: [NonZeroU32; 2],
    execution: [u64; 2],
    delivery: [u32; 3],
) {
    let before = [count(c, ids[0], 0), count(c, ids[1], 1)];
    network.packets(delivery);
    let after = [count(c, ids[0], 0), count(c, ids[1], 1)];
    assert_eq!(
        [after[0] - before[0], after[1] - before[1]],
        execution,
        "exact marked-frame execution counts"
    );
}

fn scenario<S>(backend: S, mode: XdpMode, action_code: u32, proceed: bool, keep_action: bool)
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
    let out_index = u32::try_from(
        network.links("out0")[0]["ifindex"]
            .as_u64()
            .expect("ifindex"),
    )
    .expect("u32 ifindex");
    let action = load(
        "xdp_delivery.bpf.o",
        "delivery",
        [
            ("delivery_action".into(), action_code.to_ne_bytes().to_vec()),
            ("delivery_ifindex".into(), out_index.to_ne_bytes().to_vec()),
        ]
        .into(),
    );
    let tail = load("xdp_delivery.bpf.o", "delivery_tail", Default::default());
    let observer = load("xdp_pass.bpf.o", "pass", Default::default());
    let ids = [action, tail];
    let netns = network.namespace();
    let request = |id, iface: &str, requested_mode, priority, proceed_on| XdpAttach {
        program_id: id,
        interface: iface.parse().expect("interface"),
        netns: netns.clone(),
        mode: requested_mode,
        priority,
        proceed_on,
        metadata: Default::default(),
    };
    // Native veth transmission needs a receiving peer with XDP/NAPI enabled.
    let observers: Vec<_> = ["source0", "sink0"]
        .into_iter()
        .map(|iface| {
            app.attach_xdp(request(
                observer,
                iface,
                XdpMode::Drv,
                50,
                Default::default(),
            ))
            .expect("receiving peer")
            .id
        })
        .collect();
    let forward = if action_code == 3 {
        [3, 0, 0]
    } else {
        [0, 0, 3]
    };
    let single_delivery = if proceed { [0, 3, 0] } else { forward };
    let chain_delivery = if proceed { [0, 0, 0] } else { forward };
    let mask = if proceed {
        (XdpProceedOn::default().mask() | (1 << action_code))
            .try_into()
            .expect("action mask")
    } else {
        Default::default()
    };
    let first = app
        .attach_xdp(request(action, "in0", mode, 50, mask))
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
    traffic(&c, &network, ids, [3, 0], single_delivery);

    faults.set(Some(Point::XdpReplace));
    let error = app
        .attach_xdp(request(tail, "in0", mode, 60, Default::default()))
        .expect_err("attach publication failure");
    assert_eq!(error.unresolved(), 0);
    assert_eq!(error.restoration_attempts().len(), 1);
    assert!(error.restoration_attempts()[0].is_ok());
    assert_eq!(app.get_xdp_dispatcher(details.key).expect("restored"), old);
    traffic(&c, &network, ids, [3, 0], single_delivery);
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
    let both_execution = [3, if proceed { 3 } else { 0 }];
    traffic(&c, &network, ids, both_execution, chain_delivery);

    let (removed, survivor) = if keep_action {
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
    traffic(&c, &network, ids, both_execution, chain_delivery);
    faults.set(None);

    app.detach_xdp(removed).expect("remove member");
    let remaining = app.get_xdp_dispatcher(details.key).expect("survivor");
    assert_eq!(remaining.members().len(), 1);
    assert_eq!(remaining.members()[0].member.id, survivor);
    assert_eq!(remaining.members()[0].outer_link_id, outer_id);
    let (execution, delivery) = if keep_action {
        ([3, 0], single_delivery)
    } else {
        ([0, 3], [0, 0, 0])
    };
    traffic(&c, &network, ids, execution, delivery);
    app.detach_xdp(survivor).expect("last detach");
    assert!(app.get_xdp_dispatcher(details.key).is_err());
    traffic(&c, &network, ids, [0, 0], [0, 3, 0]);

    for id in observers {
        app.detach_xdp(id).expect("remove receiving peer");
    }
    for id in [action, tail, observer] {
        assert_eq!(app.unload(id).expect("unload").unresolved(), 0);
    }
    assert!(app.list_link_records().expect("links").is_empty());
    assert!(app.list_xdp_dispatchers().expect("dispatchers").is_empty());
    c.no_artifacts();
}

pub(super) fn exercise<S>(backend: S, mode: XdpMode)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + XdpReplacementStore + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    for action_code in [3, 4] {
        for proceed in [false, true] {
            for keep_action in [false, true] {
                scenario(backend.clone(), mode, action_code, proceed, keep_action);
            }
        }
    }
}
