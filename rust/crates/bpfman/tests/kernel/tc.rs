//! Real legacy TC ingress execution, qdisc coexistence, publication and explicit retry.
#![allow(clippy::panic)]
use super::{
    faults::{Faults, Point},
    support::*,
    xdp_delivery::Network,
};
use bpfman_model::{LinkDetails, ProgramSpec};
use bpfman_runtime::{ActiveStore, Bpfman, PreparedProgram, TcAttach};
use bpfman_store::*;
use std::{num::NonZeroU32, process::Command};

pub(super) fn tc(network: &Network, args: &[&str]) -> serde_json::Value {
    let selector = network.namespace();
    let mut cmd = Command::new("nsenter");
    cmd.arg(format!("--net={}", selector.as_str()))
        .args(["tc", "-j"])
        .args(args);
    let output = cmd.output().expect("tc");
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    if output.stdout.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&output.stdout).expect("tc JSON")
    }
}

pub(super) fn clsact(network: &Network, interface: &str) -> bool {
    tc(network, &["qdisc", "show", "dev", interface])
        .as_array()
        .expect("qdiscs")
        .iter()
        .any(|q| q["kind"] == "clsact")
}

pub(super) fn request(
    id: NonZeroU32,
    network: &Network,
    hook: bpfman_model::TcDirection,
) -> TcAttach {
    TcAttach {
        direction: hook,
        program_id: id,
        interface: interface(hook).parse().expect("interface"),
        netns: network.namespace(),
        priority: 25,
        proceed_on: Default::default(),
        metadata: [("test".into(), "tc-ingress".into())].into(),
    }
}

pub(super) fn count(c: &Context, id: NonZeroU32) -> u64 {
    let map = aya::maps::MapData::from_pin(c.layout.map_directory_path(id).join("tc_stats"))
        .expect("TC map");
    let map: aya::maps::PerCpuArray<_, u64> = aya::maps::Map::PerCpuArray(map)
        .try_into()
        .expect("TC per CPU map");
    map.get(&0, 0).expect("counter").iter().sum()
}

pub(super) fn traffic(network: &Network, local: u32, hook: bpfman_model::TcDirection) {
    let mode = if hook == bpfman_model::TcDirection::Egress && local == 0 {
        "packets-egress-drop"
    } else {
        "packets"
    };
    let output = network.probe(&[mode, "64"]);
    let counts: Vec<u32> = serde_json::from_slice(&output).expect("capture counts");
    assert_eq!(counts, vec![0, local, 0]);
}

pub(super) fn exercise<S>(backend: S, hook: bpfman_model::TcDirection)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + XdpReplacementStore + TcStore + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    let interface = interface(hook);
    let c = Context::new();
    let network = Network::new();
    let faults = Faults::new(backend.clone());
    let app = Bpfman::new(
        ActiveStore::open(faults.clone(), &c.layout, TIMEOUT).expect("store"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    let prepared = PreparedProgram::new(
        &bpfman_kernel_aya::Kernel,
        &fixture("tc_ingress.bpf.o"),
        ProgramSpec::Tc("tc_ingress".try_into().expect("symbol")),
        Default::default(),
    )
    .expect("prepare TC");
    let loaded = app.load(prepared).expect("load TC");
    let id = loaded.record.id;
    assert!(!clsact(&network, interface));
    assert_eq!(count(&c, id), 0);

    // A conflicting classic ingress qdisc is never adopted or deleted.
    tc(&network, &["qdisc", "add", "dev", interface, "ingress"]);
    let error = app
        .attach_tc(request(id, &network, hook))
        .expect_err("classic ingress conflict");
    assert_eq!(error.unresolved(), 0, "{error:?}");
    assert!(app.list_link_records().expect("links").is_empty());
    assert!(
        tc(&network, &["qdisc", "show", "dev", interface])
            .as_array()
            .expect("qdiscs")
            .iter()
            .any(|q| q["kind"] == "ingress")
    );
    tc(&network, &["qdisc", "del", "dev", interface, "ingress"]);

    // Failed publication compensates the exact filter and bpfman-created clsact.
    faults.set(Some(Point::TcCommit));
    let error = app
        .attach_tc(request(id, &network, hook))
        .expect_err("publication fault");
    assert_eq!(error.unresolved(), 0, "{error:?}");
    assert!(!clsact(&network, interface));
    assert!(app.list_link_records().expect("links").is_empty());
    traffic(&network, 3, hook);
    faults.set(None);

    // A failed cleanup retains ownership and the original publication failure.
    faults.set(Some(Point::TcCommitWithBlockedCleanup));
    let error = app
        .attach_tc(request(id, &network, hook))
        .expect_err("publication and cleanup fault");
    assert_eq!(error.unresolved(), 1, "{error:?}");
    assert!(!clsact(&network, interface));
    assert!(
        app.list_link_records()
            .expect("no committed link")
            .is_empty()
    );
    let tc_dir = c.layout.root().join("fs").join(hook.dispatcher_type());
    let revisions = names(&tc_dir);
    assert_eq!(revisions.len(), 1);
    remove_injected_directory(&tc_dir.join(&revisions[0]).join("blocked_cleanup"));
    faults.set(None);
    let cancelled = bpfman_runtime::Cancellation::new();
    cancelled.cancel();
    let error = app
        .retry_tc_cleanup_with_cancellation(error, &cancelled)
        .expect_err("cancel retry admission");
    assert_eq!(error.kind(), bpfman_runtime::LinkErrorKind::Cancelled);
    assert_eq!(error.unresolved(), 1);
    let recovered = app
        .retry_tc_cleanup(error)
        .expect("retry retained stage only");
    assert!(recovered.primary_failure().is_some());
    assert_eq!(recovered.attempts().len(), 3);
    assert!(names(&tc_dir).is_empty());

    let cancelled = bpfman_runtime::Cancellation::new();
    cancelled.cancel();
    assert_eq!(
        app.attach_tc_with_cancellation(request(id, &network, hook), &cancelled)
            .expect_err("cancelled attach")
            .unresolved(),
        0
    );
    assert!(!clsact(&network, interface));
    let late = bpfman_runtime::Cancellation::new();
    faults.cancel_at(Point::TcCommit, &late);
    let record = app
        .attach_tc_with_cancellation(request(id, &network, hook), &late)
        .expect("commit determines outcome despite late cancellation");
    assert!(late.is_cancelled());
    let LinkDetails::Tc(details) = &record.details else {
        panic!("TC details");
    };
    assert_eq!(details.filter_priority, 50);
    assert_ne!(details.filter_handle.get(), 0);
    assert!(clsact(&network, interface));
    let filters = tc(
        &network,
        &["filter", "show", "dev", interface, hook.as_str()],
    );
    assert_eq!(
        filters
            .as_array()
            .expect("filters")
            .iter()
            .filter(|f| f["options"]["prog"]["id"] == details.dispatcher_id.get())
            .count(),
        1,
        "filters: {filters}"
    );
    let before = count(&c, id);
    traffic(&network, 0, hook);
    assert_eq!(count(&c, id) - before, 3);
    let observed = app.get_link(record.id).expect("observed TC link");
    assert!(observed.kernel.is_some() && observed.pin_present);
    assert_eq!(observed.record, record);
    let cancelled = bpfman_runtime::Cancellation::new();
    cancelled.cancel();
    assert!(
        app.attach_tc_with_cancellation(request(id, &network, hook), &cancelled)
            .is_err()
    );
    assert_eq!(
        app.list_link_records().expect("links"),
        vec![record.clone()]
    );
    let cancelled = bpfman_runtime::Cancellation::new();
    cancelled.cancel();
    let error = app
        .unload_with_cancellation(id, &cancelled)
        .expect_err("cancelled unload admission");
    assert!(error.report().is_none());
    traffic(&network, 0, hook);

    let marker = c.layout.tc_ownership_path(details.key, details.revision);
    let evidence = std::fs::read(&marker).expect("ownership evidence");
    assert_eq!(evidence, vec![1]);
    std::fs::write(&marker, [9]).expect("malformed evidence fixture");
    assert!(app.detach_tc(record.id).is_err());
    assert!(
        app.unload(id)
            .expect_err("malformed ownership refuses unload")
            .report()
            .is_none()
    );
    traffic(&network, 0, hook);
    std::fs::write(&marker, &evidence).expect("restore ownership evidence");

    // The exact tuple alone cannot authorize removal after a foreign program replaces it.
    let handle = details.filter_handle.to_string();
    tc(
        &network,
        &[
            "filter",
            "replace",
            "dev",
            interface,
            hook.as_str(),
            "pref",
            "50",
            "handle",
            &handle,
            "protocol",
            "all",
            "bpf",
            "da",
            "obj",
            fixture("tc_ingress.bpf.o").to_str().expect("fixture"),
            "sec",
            "classifier/foreign",
        ],
    );
    assert!(
        app.detach_tc(record.id).is_err(),
        "foreign program identity must be preserved"
    );
    assert!(
        app.unload(id)
            .expect_err("foreign filter refuses unload")
            .report()
            .is_none()
    );
    assert_eq!(
        app.list_link_records().expect("unchanged intent"),
        vec![record.clone()]
    );
    assert!(std::path::Path::new(&record.pin_path).exists());
    traffic(&network, 3, hook);
    let dispatcher = std::path::Path::new(&record.pin_path)
        .parent()
        .expect("revision")
        .join("dispatcher");
    tc(
        &network,
        &[
            "filter",
            "replace",
            "dev",
            interface,
            hook.as_str(),
            "pref",
            "50",
            "handle",
            &handle,
            "protocol",
            "all",
            "bpf",
            "da",
            "pinned",
            dispatcher.to_str().expect("dispatcher pin"),
        ],
    );
    traffic(&network, 0, hook);

    // Foreign BPF filters share priority 50 with distinct exact handles.
    for direction in ["ingress", "egress"] {
        tc(
            &network,
            &[
                "filter",
                "add",
                "dev",
                interface,
                direction,
                "pref",
                "50",
                "handle",
                "123",
                "protocol",
                "all",
                "bpf",
                "da",
                "obj",
                fixture("tc_ingress.bpf.o").to_str().expect("fixture"),
                "sec",
                "classifier/foreign",
            ],
        );
    }
    // Reopen before detach: exact filter and qdisc evidence survive local handles.
    drop(app);
    let app = Bpfman::new(
        ActiveStore::open(faults.clone(), &c.layout, TIMEOUT).expect("reopen"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    faults.set(Some(Point::TcDelete));
    let error = app
        .detach_tc(record.id)
        .expect_err("conditional delete fault");
    assert_eq!(error.unresolved(), 1, "{error:?}");
    assert!(clsact(&network, interface), "foreign filters retain clsact");
    assert!(
        tc(
            &network,
            &["filter", "show", "dev", interface, hook.as_str()]
        )
        .as_array()
        .expect("filters")
        .iter()
        .all(|f| f["options"]["prog"]["id"] != details.dispatcher_id.get())
    );
    assert!(
        !tc(
            &network,
            &["filter", "show", "dev", interface, hook.as_str()]
        )
        .as_array()
        .expect("foreign ingress")
        .is_empty()
    );
    assert!(
        !tc(&network, &["filter", "show", "dev", interface, "egress"])
            .as_array()
            .expect("foreign egress")
            .is_empty()
    );
    faults.set(None);
    let report = app.retry_tc_cleanup(error).expect("explicit retry");
    assert_eq!(report.unresolved(), 0);
    assert!(app.list_link_records().expect("links").is_empty());
    traffic(&network, 3, hook);
    tc(&network, &["qdisc", "del", "dev", interface, "clsact"]);

    // An existing empty clsact is borrowed and survives a successful reopened detach.
    tc(&network, &["qdisc", "add", "dev", interface, "clsact"]);
    let record = app
        .attach_tc(request(id, &network, hook))
        .expect("borrow clsact");
    drop(app);
    let app = Bpfman::new(
        ActiveStore::open(backend.clone(), &c.layout, TIMEOUT).expect("reopen"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    app.detach_tc(record.id).expect("detach borrowed");
    assert!(clsact(&network, interface));
    assert!(
        tc(
            &network,
            &["filter", "show", "dev", interface, hook.as_str()]
        )
        .as_array()
        .expect("filters")
        .is_empty()
    );
    tc(&network, &["qdisc", "del", "dev", interface, "clsact"]);

    // A newly created, unused clsact is reclaimed after a reopened detach.
    let record = app
        .attach_tc(request(id, &network, hook))
        .expect("owned clsact");
    drop(app);
    let app = Bpfman::new(
        ActiveStore::open(backend, &c.layout, TIMEOUT).expect("reopen"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    app.detach_tc(record.id).expect("detach owned");
    assert!(!clsact(&network, interface));
    let report = app.unload(id).expect("unload detached TC");
    assert_eq!(report.unresolved(), 0);
    c.absent(id);
    c.no_artifacts();
}

// Only repair the empty directory injected by this test; never production deletion.
#[allow(clippy::disallowed_methods)]
fn remove_injected_directory(path: &std::path::Path) {
    std::fs::remove_dir(path).expect("repair injected cleanup failure");
}

// The packet probe transmits on source0 and receives on in0.
// Both hooks therefore inspect the same test frames with unchanged assertions.
pub(super) fn interface(hook: bpfman_model::TcDirection) -> &'static str {
    match hook {
        bpfman_model::TcDirection::Ingress => "in0",
        bpfman_model::TcDirection::Egress => "source0",
    }
}
