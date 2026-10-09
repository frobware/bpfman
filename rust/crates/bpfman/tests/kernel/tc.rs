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

pub(super) fn clsact(network: &Network) -> bool {
    tc(network, &["qdisc", "show", "dev", "in0"])
        .as_array()
        .expect("qdiscs")
        .iter()
        .any(|q| q["kind"] == "clsact")
}

pub(super) fn request(id: NonZeroU32, network: &Network) -> TcAttach {
    TcAttach {
        program_id: id,
        interface: "in0".parse().expect("interface"),
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

pub(super) fn traffic(network: &Network, local: u32) {
    let output = network.probe(&["packets", "64"]);
    let counts: Vec<u32> = serde_json::from_slice(&output).expect("capture counts");
    assert_eq!(counts, vec![0, local, 0]);
}

pub(super) fn exercise<S>(backend: S)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + XdpReplacementStore + TcStore + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
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
    assert!(!clsact(&network));
    assert_eq!(count(&c, id), 0);

    // A conflicting classic ingress qdisc is never adopted or deleted.
    tc(&network, &["qdisc", "add", "dev", "in0", "ingress"]);
    let error = app
        .attach_tc(request(id, &network))
        .expect_err("classic ingress conflict");
    assert_eq!(error.unresolved(), 0, "{error:?}");
    assert!(app.list_link_records().expect("links").is_empty());
    assert!(
        tc(&network, &["qdisc", "show", "dev", "in0"])
            .as_array()
            .expect("qdiscs")
            .iter()
            .any(|q| q["kind"] == "ingress")
    );
    tc(&network, &["qdisc", "del", "dev", "in0", "ingress"]);

    // Failed publication compensates the exact filter and bpfman-created clsact.
    faults.set(Some(Point::TcCommit));
    let error = app
        .attach_tc(request(id, &network))
        .expect_err("publication fault");
    assert_eq!(error.unresolved(), 0, "{error:?}");
    assert!(!clsact(&network));
    assert!(app.list_link_records().expect("links").is_empty());
    traffic(&network, 3);
    faults.set(None);

    // A failed cleanup retains ownership and the original publication failure.
    faults.set(Some(Point::TcCommitWithBlockedCleanup));
    let error = app
        .attach_tc(request(id, &network))
        .expect_err("publication and cleanup fault");
    assert_eq!(error.unresolved(), 1, "{error:?}");
    assert!(!clsact(&network));
    assert!(
        app.list_link_records()
            .expect("no committed link")
            .is_empty()
    );
    let tc_dir = c.layout.root().join("fs/tc-ingress");
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
        app.attach_tc_with_cancellation(request(id, &network), &cancelled)
            .expect_err("cancelled attach")
            .unresolved(),
        0
    );
    assert!(!clsact(&network));
    let late = bpfman_runtime::Cancellation::new();
    faults.cancel_at(Point::TcCommit, &late);
    let record = app
        .attach_tc_with_cancellation(request(id, &network), &late)
        .expect("commit determines outcome despite late cancellation");
    assert!(late.is_cancelled());
    let LinkDetails::Tc(details) = &record.details else {
        panic!("TC details");
    };
    assert_eq!(details.filter_priority, 50);
    assert_ne!(details.filter_handle.get(), 0);
    assert!(clsact(&network));
    let filters = tc(&network, &["filter", "show", "dev", "in0", "ingress"]);
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
    traffic(&network, 0);
    assert_eq!(count(&c, id) - before, 3);
    let observed = app.get_link(record.id).expect("observed TC link");
    assert!(observed.kernel.is_some() && observed.pin_present);
    assert_eq!(observed.record, record);
    let cancelled = bpfman_runtime::Cancellation::new();
    cancelled.cancel();
    assert!(
        app.attach_tc_with_cancellation(request(id, &network), &cancelled)
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
    traffic(&network, 0);

    let marker = c.layout.root().join("tc").join(format!(
        "dispatcher_{}_{}_1",
        details.key.nsid, details.key.ifindex
    ));
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
    traffic(&network, 0);
    std::fs::write(&marker, &evidence).expect("restore ownership evidence");

    // The exact tuple alone cannot authorize removal after a foreign program replaces it.
    let handle = details.filter_handle.to_string();
    tc(
        &network,
        &[
            "filter",
            "replace",
            "dev",
            "in0",
            "ingress",
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
    traffic(&network, 3);
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
            "in0",
            "ingress",
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
    traffic(&network, 0);

    // Foreign BPF filters share priority 50 with distinct exact handles.
    for direction in ["ingress", "egress"] {
        tc(
            &network,
            &[
                "filter",
                "add",
                "dev",
                "in0",
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
    assert!(clsact(&network), "foreign filters retain clsact");
    assert!(
        tc(&network, &["filter", "show", "dev", "in0", "ingress"])
            .as_array()
            .expect("filters")
            .iter()
            .all(|f| f["options"]["prog"]["id"] != details.dispatcher_id.get())
    );
    assert!(
        !tc(&network, &["filter", "show", "dev", "in0", "ingress"])
            .as_array()
            .expect("foreign ingress")
            .is_empty()
    );
    assert!(
        !tc(&network, &["filter", "show", "dev", "in0", "egress"])
            .as_array()
            .expect("foreign egress")
            .is_empty()
    );
    faults.set(None);
    let report = app.retry_tc_cleanup(error).expect("explicit retry");
    assert_eq!(report.unresolved(), 0);
    assert!(app.list_link_records().expect("links").is_empty());
    traffic(&network, 3);
    tc(&network, &["qdisc", "del", "dev", "in0", "clsact"]);

    // An existing empty clsact is borrowed and survives a successful reopened detach.
    tc(&network, &["qdisc", "add", "dev", "in0", "clsact"]);
    let record = app.attach_tc(request(id, &network)).expect("borrow clsact");
    drop(app);
    let app = Bpfman::new(
        ActiveStore::open(backend.clone(), &c.layout, TIMEOUT).expect("reopen"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    app.detach_tc(record.id).expect("detach borrowed");
    assert!(clsact(&network));
    assert!(
        tc(&network, &["filter", "show", "dev", "in0", "ingress"])
            .as_array()
            .expect("filters")
            .is_empty()
    );
    tc(&network, &["qdisc", "del", "dev", "in0", "clsact"]);

    // A newly created, unused clsact is reclaimed after a reopened detach.
    let record = app.attach_tc(request(id, &network)).expect("owned clsact");
    drop(app);
    let app = Bpfman::new(
        ActiveStore::open(backend, &c.layout, TIMEOUT).expect("reopen"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    app.detach_tc(record.id).expect("detach owned");
    assert!(!clsact(&network));
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
