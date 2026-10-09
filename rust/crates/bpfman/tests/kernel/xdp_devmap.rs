//! DEVMAP publication-failure restoration with real packet and map-lifetime evidence.
//! Ordinary forwarding/update/survivor behaviour lives in e2e/xdp-delivery.bpfman.
#![allow(clippy::panic)]

use super::{
    faults::{Faults, Point},
    support::*,
    xdp_delivery::{Frames, Network},
};
use bpfman_model::*;
use bpfman_runtime::{ActiveStore, Bpfman, PreparedProgram, XdpAttach};
use bpfman_store::*;
use std::{num::NonZeroU32, path::PathBuf};

const HASH_KEY: u32 = 0x8000_0001;
const SPARE_KEY: u32 = u32::MAX;

#[derive(Clone, Copy)]
enum Kind {
    Array,
    Hash,
}

impl Kind {
    fn key(self) -> u32 {
        match self {
            Self::Array => 0,
            Self::Hash => HASH_KEY,
        }
    }

    fn object(self, frames: Frames) -> &'static str {
        match (self, frames) {
            (Self::Array, _) => frames.devmap_object(),
            (Self::Hash, Frames::Linear) => "xdp_devmap_hash.bpf.o",
            (Self::Hash, Frames::MultiBuffer) => "xdp_devmap_hash_frags.bpf.o",
        }
    }
}

enum TargetMap {
    Array(aya::maps::xdp::DevMap<aya::maps::MapData>),
    Hash(aya::maps::xdp::DevMapHash<aya::maps::MapData>),
}

impl TargetMap {
    fn get(&self, key: u32) -> Result<(u32, Option<NonZeroU32>), aya::maps::MapError> {
        match self {
            Self::Array(map) => map.get(key, 0).map(|value| (value.if_index, value.prog_id)),
            Self::Hash(map) => map.get(key, 0).map(|value| (value.if_index, value.prog_id)),
        }
    }
}

pub(super) struct Targets {
    pin: PathBuf,
    id: u32,
    key: u32,
    map: TargetMap,
}

impl Targets {
    pub(super) fn open(c: &Context, program: NonZeroU32) -> Self {
        Self::open_kind(c, program, Kind::Array)
    }

    pub(super) fn open_hash(c: &Context, program: NonZeroU32, capacity: u32) -> Self {
        let targets = Self::open_kind(c, program, Kind::Hash);
        assert_eq!(
            aya::maps::MapInfo::from_pin(&targets.pin)
                .expect("hash info")
                .max_entries(),
            capacity,
            "capacity differs from key range"
        );
        targets
    }

    fn open_kind(c: &Context, program: NonZeroU32, kind: Kind) -> Self {
        let pin = c
            .layout
            .map_directory_path(program)
            .join("delivery_targets");
        let data = aya::maps::MapData::from_pin(&pin).expect("DEVMAP pin");
        let info = data.info().expect("map info");
        let id = info.id();
        let map = match kind {
            Kind::Array => {
                assert_eq!(info.map_type().expect("type"), aya::maps::MapType::DevMap);
                TargetMap::Array(aya::maps::Map::DevMap(data).try_into().expect("DEVMAP"))
            }
            Kind::Hash => {
                assert_eq!(
                    info.map_type().expect("type"),
                    aya::maps::MapType::DevMapHash
                );
                let map: aya::maps::xdp::DevMapHash<_> = aya::maps::Map::DevMapHash(data)
                    .try_into()
                    .expect("DEVMAP_HASH");
                assert_eq!(map.keys().count(), 0, "initially empty hash");
                TargetMap::Hash(map)
            }
        };
        let targets = Self {
            pin,
            id,
            key: kind.key(),
            map,
        };
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
        self.check_at(self.key, expected);
    }

    fn check_at(&self, key: u32, expected: Option<u32>) {
        self.check_value(key, expected.map(|index| (index, None)));
    }

    fn check_value(&self, key: u32, expected: Option<(u32, Option<NonZeroU32>)>) {
        assert_eq!(
            aya::maps::MapInfo::from_pin(&self.pin)
                .expect("current pin")
                .id(),
            self.id,
            "replacement must preserve the published map identity"
        );
        match expected {
            Some((index, program)) => {
                let (if_index, prog_id) = self.map.get(key).expect("target");
                assert_eq!(if_index, index, "target contents must survive rebuilding");
                assert_eq!(prog_id, program, "egress program identity");
            }
            None => assert!(matches!(
                self.map.get(key),
                Err(aya::maps::MapError::KeyNotFound)
            )),
        }
    }

    pub(super) fn check_egress(&self, index: u32, program: NonZeroU32) {
        self.check_value(self.key, Some((index, Some(program))));
    }

    pub(super) fn check_hash_egress(&self, expected: Option<(u32, NonZeroU32)>, spare: u32) {
        self.check_value(
            self.key,
            expected.map(|(index, program)| (index, Some(program))),
        );
        self.check_at(SPARE_KEY, Some(spare));
        let mut keys = vec![SPARE_KEY];
        if expected.is_some() {
            keys.push(self.key);
        }
        self.check_hash_keys(&keys);
    }

    pub(super) fn set_egress(&self, c: &Context, network: &Network, program: NonZeroU32) -> u32 {
        network.probe(&[
            "devmap-egress",
            self.pin.to_str().expect("map pin"),
            "out0",
            c.layout
                .program_pin_path(program)
                .to_str()
                .expect("program pin"),
            &self.key.to_string(),
        ]);
        let index = u32::try_from(
            network.links("out0")[0]["ifindex"]
                .as_u64()
                .expect("ifindex"),
        )
        .expect("u32");
        self.check_egress(index, program);
        index
    }

    pub(super) fn reject_egress(&self, c: &Context, network: &Network, program: NonZeroU32) {
        let output = network.probe_output(&[
            "devmap-egress",
            self.pin.to_str().expect("map pin"),
            "out0",
            c.layout
                .program_pin_path(program)
                .to_str()
                .expect("program pin"),
            &self.key.to_string(),
        ]);
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("invalid argument"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn set(&self, network: &Network, interface: &str) -> u32 {
        self.set_at(network, self.key, interface)
    }

    pub(super) fn set_at(&self, network: &Network, key: u32, interface: &str) -> u32 {
        // Resolve the netdevice in the private namespace, never in the caller's.
        network.probe(&[
            "devmap-set",
            self.pin.to_str().expect("pin path"),
            interface,
            &key.to_string(),
        ]);
        let index = u32::try_from(
            network.links(interface)[0]["ifindex"]
                .as_u64()
                .expect("index"),
        )
        .expect("u32 index");
        self.check_at(key, Some(index));
        index
    }

    pub(super) fn delete_at(&self, network: &Network, key: u32) {
        network.probe(&[
            "devmap-delete",
            self.pin.to_str().expect("pin path"),
            &key.to_string(),
        ]);
        self.check_at(key, None);
    }

    pub(super) fn id(&self) -> u32 {
        self.id
    }

    pub(super) fn check_entries(&self, expected: &[Option<u32>]) {
        let TargetMap::Array(map) = &self.map else {
            panic!("array map required");
        };
        assert_eq!(map.len() as usize, expected.len(), "complete map contents");
        for (key, &value) in expected.iter().enumerate() {
            self.check_at(key as u32, value);
        }
    }

    pub(super) fn check_keyed_entries(&self, expected: &[(u32, Option<u32>)]) {
        for &(key, value) in expected {
            self.check_at(key, value);
        }
        match &self.map {
            TargetMap::Array(map) => {
                assert_eq!(
                    map.len() as usize,
                    expected.len(),
                    "complete array contents"
                );
                let mut keys: Vec<_> = expected.iter().map(|&(key, _)| key).collect();
                keys.sort_unstable();
                assert_eq!(keys, (0..map.len()).collect::<Vec<_>>());
            }
            TargetMap::Hash(_) => {
                let populated: Vec<_> = expected
                    .iter()
                    .filter_map(|&(key, value)| value.map(|_| key))
                    .collect();
                self.check_hash_keys(&populated);
            }
        }
    }

    fn check_hash_keys(&self, expected: &[u32]) {
        let TargetMap::Hash(map) = &self.map else {
            panic!("hash map required");
        };
        let mut keys: Vec<_> = map.keys().map(|key| key.expect("hash key")).collect();
        let mut expected = expected.to_vec();
        keys.sort_unstable();
        expected.sort_unstable();
        assert_eq!(keys, expected, "complete hash contents");
    }

    fn check_hash(&self, expected: Option<u32>, spare: u32) {
        self.check_keyed_entries(&[(HASH_KEY, expected), (SPARE_KEY, Some(spare))]);
    }

    pub(super) fn assert_unloaded(self) {
        assert!(!self.pin.exists(), "unload must remove the owned map pin");
        let id = self.id;
        drop(self);
        map_released(id);
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

fn scenario<S>(
    backend: S,
    mode: XdpMode,
    proceed: bool,
    keep_redirect: bool,
    frames: Frames,
    kind: Kind,
) where
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
        kind.object(frames),
        "devmap_delivery",
        [("devmap_fallback".into(), 2u32.to_ne_bytes().to_vec())].into(),
    );
    let tail = load(
        frames.delivery_object(),
        "delivery_tail",
        Default::default(),
    );
    let (object, symbol) = frames.observer();
    let observer = load(object, symbol, Default::default());
    let ids = [redirect, tail, observer];
    let traffic = |execution, delivery| frames.traffic(&c, &network, ids, execution, delivery);
    let targets = match kind {
        Kind::Array => Targets::open(&c, redirect),
        Kind::Hash => Targets::open_hash(&c, redirect, 2),
    };
    // The unused entry must neither redirect a missing key nor disappear on rebuild.
    let spare = match kind {
        Kind::Array => None,
        Kind::Hash => Some(targets.set_at(&network, SPARE_KEY, "in0")),
    };
    let check = |expected| match spare {
        Some(index) => targets.check_hash(expected, index),
        None => targets.check(expected),
    };
    check(None);
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
    let single_delivery = if proceed { [0, 3, 0] } else { [0, 0, 3] };
    let chain_delivery = if proceed { [0, 0, 0] } else { [0, 0, 3] };
    let chain_execution = [3, if proceed { 3 } else { 0 }];

    let out_index = targets.set(&network, "out0");
    check(Some(out_index));
    if spare.is_some() {
        // Two arbitrary keys fill capacity two. A third must fail in the kernel,
        // while replacement of either existing entry remains permitted.
        let output = network.probe_output(&[
            "devmap-set",
            targets.pin.to_str().expect("pin"),
            "out0",
            "7",
        ]);
        assert_eq!(output.status.code(), Some(1));
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("argument list too long"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        targets.check_at(7, None);
        check(Some(out_index));
    }
    faults.set(Some(Point::XdpReplace));
    let error = app
        .attach_xdp(request(tail, "in0", mode, 60, Default::default()))
        .expect_err("attach publication failure");
    assert_eq!(error.unresolved(), 0);
    assert_eq!(error.restoration_attempts().len(), 1);
    assert!(error.restoration_attempts()[0].is_ok());
    assert_eq!(app.get_xdp_dispatcher(details.key).expect("restored"), old);
    check(Some(out_index));
    traffic([3, 0], single_delivery);
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
    check(Some(out_index));
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
    check(Some(out_index));
    traffic(chain_execution, chain_delivery);
    faults.set(None);

    app.detach_xdp(removed).expect("remove member");
    let remaining = app.get_xdp_dispatcher(details.key).expect("survivor");
    assert_eq!(remaining.members().len(), 1);
    assert_eq!(remaining.members()[0].member.id, survivor);
    assert_eq!(remaining.members()[0].outer_link_id, outer_id);
    check(Some(out_index));
    app.detach_xdp(survivor).expect("last detach");
    assert!(app.get_xdp_dispatcher(details.key).is_err());
    check(Some(out_index));

    for id in observers {
        app.detach_xdp(id).expect("remove receiving peer");
    }
    assert_eq!(app.unload(tail).expect("unload tail").unresolved(), 0);
    check(Some(out_index));
    assert_eq!(
        app.unload(redirect).expect("unload redirect").unresolved(),
        0
    );
    targets.assert_unloaded();
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
    S: OpenStore
        + CommitLoad
        + UnloadStore
        + LinkStore
        + XdpReplacementStore
        + bpfman_store::TcStore
        + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    exercise_kind(backend, mode, frames, Kind::Array);
}

pub(super) fn exercise_hash<S>(backend: S, mode: XdpMode, frames: Frames)
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
    exercise_kind(backend, mode, frames, Kind::Hash);
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
    // Every restoration probe uses a populated target. The missing-key
    // fallback is therefore unobservable here; DROP/PASS fallback execution
    // belongs to the parallel scripts. Keep both masks and removal choices,
    // plus every store, mode, frame shape and map kind in this kernel contract.
    for proceed in [false, true] {
        for keep_redirect in [false, true] {
            scenario(backend.clone(), mode, proceed, keep_redirect, frames, kind);
        }
    }
}
