//! Fragment-aware publication restoration and the mixed dispatcher ABI.
//! Ordinary PASS, stopping, mixed membership and survivor traffic run in the
//! parallel TestXDP_PASS_{Drv,Skb}_MultiBuffer scripts.
#![allow(clippy::panic)]
use super::{
    faults::{Faults, Point},
    support::*,
};
use bpfman_model::*;
use bpfman_runtime::{ActiveStore, Bpfman, PreparedProgram, XdpAttach};
use bpfman_store::*;
use std::{num::NonZeroU32, process::Command};

fn ip(args: &[&str], success: bool) {
    let output = Command::new("ip").args(args).output().expect("ip");
    assert_eq!(
        output.status.success(),
        success,
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Network {
    receiver: String,
    sender: String,
}

impl Network {
    fn new() -> Self {
        let network = Self {
            receiver: format!("bpfman-frags-{}-r", std::process::id()),
            sender: format!("bpfman-frags-{}-s", std::process::id()),
        };
        for ns in [&network.receiver, &network.sender] {
            ip(&["netns", "add", ns], true);
        }
        ip(
            &[
                "-n",
                &network.receiver,
                "link",
                "add",
                "frags0",
                "type",
                "veth",
                "peer",
                "name",
                "frags1",
            ],
            true,
        );
        ip(
            &[
                "-n",
                &network.receiver,
                "link",
                "set",
                "frags1",
                "netns",
                &network.sender,
            ],
            true,
        );
        for (ns, iface, addr) in [
            (&network.receiver, "frags0", "198.19.0.1/24"),
            (&network.sender, "frags1", "198.19.0.2/24"),
        ] {
            ip(&["-n", ns, "addr", "add", addr, "dev", iface], true);
            ip(
                &["-n", ns, "link", "set", "dev", iface, "mtu", "9000", "up"],
                true,
            );
        }
        network
    }

    fn mtu(&self, mtu: &str) {
        for (ns, iface) in [(&self.receiver, "frags0"), (&self.sender, "frags1")] {
            ip(&["-n", ns, "link", "set", "dev", iface, "mtu", mtu], true);
        }
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        for ns in [&self.receiver, &self.sender] {
            let output = Command::new("ip").args(["netns", "del", ns]).output();
            if !std::thread::panicking() {
                assert!(output.expect("namespace cleanup").status.success());
            }
        }
    }
}

fn counters(c: &Context, id: NonZeroU32) -> [u64; 4] {
    let map =
        aya::maps::MapData::from_pin(c.layout.map_directory_path(id).join("frags_probe_stats"))
            .expect("probe map");
    let map: aya::maps::PerCpuArray<_, u64> = aya::maps::Map::PerCpuArray(map)
        .try_into()
        .expect("per CPU");
    std::array::from_fn(|key| map.get(&(key as u32), 0).expect("counter").iter().sum())
}

fn packets(c: &Context, peer: &str, active: &[NonZeroU32], inactive: &[NonZeroU32]) {
    let before: Vec<_> = active
        .iter()
        .chain(inactive)
        .map(|&id| (id, counters(c, id)))
        .collect();
    let output = Command::new("ip")
        .args([
            "netns",
            "exec",
            peer,
            "ping",
            "-I",
            "frags1",
            "-M",
            "do",
            "-s",
            "8000",
            "-p",
            "a5",
            "-c",
            "3",
            "-i",
            "0.05",
            "-W",
            "1",
            "198.19.0.1",
        ])
        .output()
        .expect("jumbo ping");
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for (index, (id, before)) in before.into_iter().enumerate() {
        let after = counters(c, id);
        if index < active.len() {
            // These count only multi-buffer frames, not incidental ARP/control packets.
            for key in 1..4 {
                assert!(
                    after[key] >= before[key] + 3,
                    "{id}: metric {key}: {before:?} -> {after:?}"
                );
            }
        } else {
            assert_eq!(&after[1..], &before[1..], "inactive {id}");
        }
    }
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
    for keep_first in [false, true] {
        let c = Context::new();
        let network = Network::new();
        let name: InterfaceName = "frags0".parse().expect("interface");
        let peer = &network.sender;
        let netns: NetworkNamespace = format!("/run/netns/{}", network.receiver)
            .parse()
            .expect("namespace");
        let faults = Faults::new(backend.clone());
        let app = Bpfman::new(
            ActiveStore::open(faults.clone(), &c.layout, TIMEOUT).expect("store"),
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
        let a = load("xdp_frags_probe.bpf.o", "frags_probe");
        let b = load("xdp_frags_probe.bpf.o", "frags_probe");
        let normal = load("xdp_counter.bpf.o", "xdp_stats");
        let request = |program_id, proceed_on| XdpAttach {
            program_id,
            interface: name.clone(),
            netns: netns.clone(),
            mode,
            priority: 50,
            proceed_on,
            metadata: Default::default(),
        };
        let first = app
            .attach_xdp(request(a, Default::default()))
            .expect("first jumbo attachment");
        let LinkDetails::Xdp(details) = &first.details else {
            panic!("XDP");
        };
        let output = Command::new("ip")
            .args([
                "-n",
                &network.receiver,
                "-j",
                "-d",
                "link",
                "show",
                "dev",
                name.as_str(),
            ])
            .output()
            .expect("mode");
        assert!(output.status.success());
        let links: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON");
        assert_eq!(
            links[0]["xdp"]["attached"][0]["mode"],
            if mode == XdpMode::Drv { 1 } else { 2 },
            "must use the explicitly requested {mode:?} mode"
        );
        let old = app.get_xdp_dispatcher(details.key).expect("snapshot");
        let outer_id = old.members()[0].outer_link_id;

        // Successful switching followed by failed publication restores a fragment-aware target.
        faults.set(Some(Point::XdpReplace));
        let error = app
            .attach_xdp(request(b, Default::default()))
            .expect_err("publication failure");
        assert_eq!(error.unresolved(), 0);
        assert!(error.restoration_attempts()[0].is_ok());
        assert_eq!(app.get_xdp_dispatcher(details.key).expect("restored"), old);
        packets(&c, peer, &[a], &[b]);
        faults.set(None);
        let second = app
            .attach_xdp(request(b, Default::default()))
            .expect("two frags members");
        let chain = app.get_xdp_dispatcher(details.key).expect("chain");

        // Native veth rejects a non-fragment dispatcher while the peer has jumbo MTU.
        if mode == XdpMode::Drv {
            let error = app
                .attach_xdp(request(normal, Default::default()))
                .expect_err("incompatible jumbo chain");
            assert_eq!(error.unresolved(), 0);
            assert_eq!(
                app.get_xdp_dispatcher(details.key).expect("unchanged"),
                chain
            );
        }

        // On a normal-MTU interface mixed membership is valid and disables fragments.
        network.mtu("1500");
        let mixed = app
            .attach_xdp(request(normal, Default::default()))
            .expect("mixed chain");
        let mixed_snapshot = app.get_xdp_dispatcher(details.key).expect("mixed snapshot");
        let dispatcher = aya::programs::ProgramInfo::from_pin(
            c.layout
                .xdp_program_path(details.key, mixed_snapshot.members()[0].details.revision),
        )
        .expect("dispatcher");
        let config_map = dispatcher
            .map_ids()
            .expect("maps")
            .expect("map IDs")
            .into_iter()
            .find(|&id| {
                aya::maps::MapInfo::from_id(id)
                    .expect("map info")
                    .value_size()
                    == 124
            })
            .expect("configuration map");
        let config: aya::maps::Array<_, [u8; 124]> =
            aya::maps::Map::Array(aya::maps::MapData::from_id(config_map).expect("map"))
                .try_into()
                .expect("configuration array");
        let bytes = config.get(&0, 0).expect("configuration bytes");
        assert_eq!(bytes[3], 0, "mixed dispatcher disables fragments");
        for member in mixed_snapshot.members() {
            let slot = member.details.slot.index();
            let offset = 84 + slot * 4;
            let expected = if member.member.program_id == normal {
                0u32
            } else {
                32u32
            };
            assert_eq!(&bytes[offset..offset + 4], &expected.to_ne_bytes());
        }
        app.detach_xdp(mixed.id)
            .expect("return to frags-only chain");
        network.mtu("9000");

        let (removed, survivor) = if keep_first {
            (second.id, first.id)
        } else {
            (first.id, second.id)
        };
        let before = app.get_xdp_dispatcher(details.key).expect("before detach");
        faults.set(Some(Point::XdpReplace));
        let error = app
            .detach_xdp(removed)
            .expect_err("detach publication failure");
        assert_eq!(error.unresolved(), 0);
        assert!(error.restoration_attempts()[0].is_ok());
        assert_eq!(
            app.get_xdp_dispatcher(details.key).expect("restored chain"),
            before
        );
        packets(&c, peer, &[a, b], &[]);
        faults.set(None);
        app.detach_xdp(removed).expect("detach one");
        assert_eq!(
            app.get_xdp_dispatcher(details.key)
                .expect("survivor")
                .members()[0]
                .outer_link_id,
            outer_id
        );
        app.detach_xdp(survivor).expect("last detach");
        assert!(app.get_xdp_dispatcher(details.key).is_err());
        for id in [a, b, normal] {
            assert_eq!(app.unload(id).expect("unload").unresolved(), 0);
        }
        c.no_artifacts();
    }
}
