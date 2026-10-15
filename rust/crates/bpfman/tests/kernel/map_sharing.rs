//! Native identity, compatibility, owner-first unload and consuming map recovery.
use super::{
    faults::{Faults, Point},
    support::*,
};
use bpfman_fs::RuntimeDirectory;
use bpfman_kernel::ProgramObservations;
use bpfman_model::{ObservedProgram, ProgramSpec};
use bpfman_runtime::{ActiveStore, Bpfman, KprobeAttach, PreparedProgram};
use bpfman_store::*;
use std::{collections::BTreeMap, num::NonZeroU32};

fn prepared(object: &str, spec: ProgramSpec) -> PreparedProgram {
    PreparedProgram::new(
        &bpfman_kernel_aya::Kernel,
        &fixture(object),
        spec,
        Default::default(),
    )
    .expect("prepare")
}
fn tracepoint(shared: bool) -> PreparedProgram {
    prepared(
        if shared {
            "tracepoint_counter_pinned.bpf.o"
        } else {
            "tracepoint_counter.bpf.o"
        },
        ProgramSpec::Tracepoint(NAME.try_into().expect("symbol")),
    )
}
fn probe(name: &str) -> PreparedProgram {
    prepared(
        "multi_prog_kprobe_kmod_counter.bpf.o",
        ProgramSpec::Kprobe(name.try_into().expect("symbol")),
    )
}
fn map(program: &ObservedProgram) -> u32 {
    program
        .maps
        .iter()
        .find(|m| m.kernel.name == "tracepoint_stat")
        .or_else(|| program.maps.first())
        .expect("map")
        .kernel
        .id
}
fn absent(id: NonZeroU32) {
    assert_eq!(
        bpfman_kernel_aya::Kernel
            .program(id)
            .expect_err("released program")
            .kind(),
        bpfman_kernel::ErrorKind::Missing
    );
}

pub(super) fn exercise<S>(backend: S)
where
    S: OpenStore
        + CommitLoad
        + UnloadStore
        + LinkStore
        + XdpReplacementStore
        + TcStore
        + Clone
        + 'static,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    let c = Context::new();
    let store = Faults::new(backend);
    let app = Bpfman::new(
        ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("store"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    let shared = c.layout.shared_maps_path().join("tracepoint_stats_map");

    // A verifier failure after map creation must consume the new name pin.
    let error = app
        .load(prepared(
            "shared_map_contract.bpf.o",
            ProgramSpec::Kprobe("shared_map_bad".try_into().expect("name")),
        ))
        .expect_err("verifier refusal");
    assert_eq!(error.unresolved(), 0);
    assert!(!shared.exists());
    c.no_artifacts();

    store.set(Some(Point::Commit));
    let error = app
        .load(tracepoint(true))
        .expect_err("new name pin, failed commit");
    assert_eq!(error.unresolved(), 0);
    assert!(!shared.exists());
    store.set(None);
    c.no_artifacts();

    let first = app.load(tracepoint(true)).expect("named owner");
    let first_id = first.record.id;
    let map_id = map(&first);
    assert!(shared.exists());
    let held_map = aya::maps::MapData::from_pin(&shared).expect("external map holder");
    assert_eq!(held_map.info().expect("typed map").id(), map_id);
    assert!(
        bpfman_kernel_aya::Kernel
            .map(map_id)
            .expect("map")
            .btf_id
            .is_some()
    );
    store.set(Some(Point::Commit));
    let error = app
        .load(tracepoint(true))
        .expect_err("existing name pin, failed commit");
    assert_eq!(error.unresolved(), 0);
    store.set(None);
    assert_eq!(
        aya::maps::MapInfo::from_pin(&shared)
            .expect("preserved map")
            .id(),
        map_id
    );

    let error = app
        .load(prepared(
            "shared_map_contract.bpf.o",
            ProgramSpec::Kprobe("shared_map_good".try_into().expect("name")),
        ))
        .expect_err("incompatible type and dimensions");
    assert_eq!(error.unresolved(), 0);
    assert_eq!(
        app.list(&Default::default()).expect("no new program").len(),
        1
    );
    assert_eq!(
        aya::maps::MapInfo::from_pin(&shared)
            .expect("unchanged")
            .id(),
        map_id
    );

    let second = app.load(tracepoint(true)).expect("second named user");
    assert_eq!(map(&second), map_id);
    assert_ne!(second.record.map_set, first.record.map_set);
    let control = app.load(tracepoint(false)).expect("private control");
    assert_ne!(map(&control), map_id);
    assert_eq!(
        app.unload(first_id)
            .expect("first named unload")
            .unresolved(),
        0
    );
    absent(first_id);
    assert!(shared.exists());

    // A swapped shared pin must be refused during preflight, before even the
    // otherwise valid program pin or private aliases can be removed.
    let private_pin = c
        .layout
        .map_directory_path(control.record.id)
        .join("tracepoint_stats_map");
    let saved = c.layout.root().join("fs/saved-shared-map");
    let swap = || {
        std::fs::rename(&shared, &saved).expect("save");
        std::fs::rename(&private_pin, &shared).expect("replace");
        std::fs::rename(&saved, &private_pin).expect("swap");
    };
    swap();
    let error = app
        .unload(second.record.id)
        .expect_err("wrong shared identity");
    assert!(error.report().is_none());
    assert!(c.layout.program_pin_path(second.record.id).exists());
    swap();

    store.set(Some(Point::DeleteProgram));
    let error = app
        .unload(second.record.id)
        .expect_err("program record blocks maps");
    assert!(shared.exists());
    store.set(None);
    let joined = app
        .load(tracepoint(false).with_map_owner(second.record.id))
        .expect("new map-set user before retry");
    assert_eq!(map(&joined), map_id);
    let reopened = Bpfman::new(
        ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("reopen"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    assert_eq!(
        reopened
            .retry_unload(error)
            .expect("record-only retry transfers map ownership")
            .unresolved(),
        0
    );
    absent(second.record.id);
    assert!(shared.exists());
    assert_eq!(
        map(&reopened.get(joined.record.id).expect("joined program")),
        map_id
    );
    assert_eq!(
        reopened
            .unload(joined.record.id)
            .expect("last named user")
            .unresolved(),
        0
    );
    assert!(!shared.exists());
    assert!(
        held_map.info().is_ok(),
        "external map reference remains valid"
    );
    drop(held_map);
    assert_eq!(
        app.unload(control.record.id)
            .expect("private control unload")
            .unresolved(),
        0
    );
    c.no_artifacts();

    // Every public ELF map is pinned even if this selection references only
    // one. A different selection can join the same map set and unload last.
    let owner = app.load(probe("mkp_a")).expect("owner");
    let runtime = RuntimeDirectory::open_existing(c.layout.clone())
        .expect("runtime")
        .expect("exists");
    let pins = bpfman_kernel_aya::Kernel
        .map_pins(&runtime, owner.record.id)
        .expect("complete set");
    let ids: BTreeMap<_, _> = pins.into_iter().map(|pin| (pin.name, pin.id)).collect();
    assert!(ids.contains_key("mkp_a_count") && ids.contains_key("mkp_b_count"));
    let dependent = app
        .load(probe("mkp_b").with_map_owner(owner.record.id))
        .expect("different selection joins");
    let attach = |program_id| KprobeAttach {
        program_id,
        function: "bpfman_e2e_target_0".parse().expect("target"),
        offset: 0,
        metadata: Default::default(),
    };
    app.attach_kprobe(attach(owner.record.id))
        .expect("owner attach");
    app.attach_kprobe(attach(dependent.record.id))
        .expect("dependent attach");
    assert_eq!(
        app.unload(owner.record.id)
            .expect("attached owner unload first")
            .unresolved(),
        0
    );
    absent(owner.record.id);
    assert!(c.layout.map_directory_path(owner.record.id).exists());
    let late = app
        .load(probe("mkp_a").with_map_owner(owner.record.id))
        .expect("join after original program removed");
    assert_eq!(
        app.unload(dependent.record.id)
            .expect("attached dependent")
            .unresolved(),
        0
    );
    absent(dependent.record.id);
    assert_eq!(
        app.unload(late.record.id)
            .expect("last set user")
            .unresolved(),
        0
    );
    assert!(!c.layout.map_directory_path(owner.record.id).exists());
    c.no_artifacts();
}
