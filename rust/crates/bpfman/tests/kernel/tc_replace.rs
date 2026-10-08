//! Actual legacy filter replacement, signed continuation and reopened member unload.
#![allow(clippy::panic)]
use super::{
    faults::{Faults, Point},
    support::*,
    tc::{clsact, count, request, tc, traffic},
    xdp_delivery::Network,
};
use bpfman_model::{LinkDetails, ProgramSpec};
use bpfman_runtime::{ActiveStore, Bpfman, PreparedProgram};
use bpfman_store::*;

pub(super) fn exercise<S>(backend: S)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + XdpReplacementStore + TcStore + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    for ownership in ["owned", "borrowed", "foreign"] {
        let c = Context::new();
        let network = Network::new();
        if ownership == "borrowed" {
            tc(&network, &["qdisc", "add", "dev", "in0", "clsact"]);
        }
        let faults = Faults::new(backend.clone());
        let app = Bpfman::new(
            ActiveStore::open(faults.clone(), &c.layout, TIMEOUT).expect("store"),
            bpfman_kernel_aya::Kernel,
            TIMEOUT,
        );
        let load = |action: i32| {
            app.load(
                PreparedProgram::new(
                    &bpfman_kernel_aya::Kernel,
                    &fixture("tc_ingress.bpf.o"),
                    ProgramSpec::Tc("tc_ingress".try_into().expect("symbol")),
                    Default::default(),
                )
                .expect("prepare")
                .with_globals(
                    &bpfman_kernel_aya::Kernel,
                    [("tc_action".into(), action.to_ne_bytes().to_vec())].into(),
                )
                .expect("globals"),
            )
            .expect("load")
            .record
            .id
        };
        let a = load(-1);
        let b = load(2);
        let d = load(0);
        let mut r = request(a, &network);
        r.priority = 50;
        r.proceed_on = 1u32.try_into().expect("UNSPEC bit");
        let first = app.attach_tc(r).expect("UNSPEC first");
        let LinkDetails::Tc(details) = &first.details else {
            panic!("TC details");
        };
        let key = details.key;
        let handle = details.filter_handle;
        if ownership == "foreign" {
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
        }
        let mut r = request(b, &network);
        r.priority = 60;
        app.attach_tc(r).expect("second member");
        let s = app.get_tc_dispatcher(key).expect("snapshot");
        assert_eq!(s.members().len(), 2);
        assert_eq!(s.members()[0].details.filter_handle, handle);
        let before = [count(&c, a), count(&c, b), count(&c, d)];
        traffic(&network, 0);
        assert_eq!(
            [
                count(&c, a) - before[0],
                count(&c, b) - before[1],
                count(&c, d) - before[2]
            ],
            [3, 3, 0]
        );
        let old_dir = c
            .layout
            .tc_revision_path(key, s.members()[0].details.revision);
        faults.set(Some(Point::TcReplace));
        let error = app
            .attach_tc(request(d, &network))
            .expect_err("atomic publication failure");
        assert_eq!(error.unresolved(), 0, "{error:?}");
        assert_eq!(error.restoration_attempts().len(), 1);
        assert_eq!(
            app.get_tc_dispatcher(key).expect("old membership restored"),
            s
        );
        let before = [count(&c, a), count(&c, b), count(&c, d)];
        traffic(&network, 0);
        assert_eq!(
            [
                count(&c, a) - before[0],
                count(&c, b) - before[1],
                count(&c, d) - before[2]
            ],
            [3, 3, 0]
        );
        assert!(old_dir.exists());
        faults.set(None);
        let stop = app
            .attach_tc(request(d, &network))
            .expect("higher priority OK stops chain");
        assert!(!old_dir.exists(), "retired revision removed");
        let before = [count(&c, a), count(&c, b), count(&c, d)];
        traffic(&network, 3);
        assert_eq!(
            [
                count(&c, a) - before[0],
                count(&c, b) - before[1],
                count(&c, d) - before[2]
            ],
            [0, 0, 3]
        );
        if ownership == "owned" {
            // Retirement tries independent extension removals even after earlier
            // failures, and retains the native dispatcher until every EXT pin is gone.
            let old = app.get_tc_dispatcher(key).expect("three members");
            let old_directory = c
                .layout
                .tc_revision_path(key, old.members()[0].details.revision);
            faults.set(Some(Point::TcReplaceWithBlockedRetirement));
            let mut extra = request(d, &network);
            extra.priority = 100;
            let error = app
                .attach_tc(extra)
                .expect_err("committed retirement failure");
            assert!(error.committed_snapshot().is_some());
            assert_eq!(error.unresolved(), 1);
            assert!(old_directory.join("link_0").is_dir());
            assert!(old_directory.join("link_1").is_dir());
            assert!(
                !old_directory.join("link_2").exists(),
                "later independent extension cleanup still runs"
            );
            assert!(
                old_directory.join("dispatcher").exists(),
                "native pin depends on all extension removals"
            );
            for slot in 0..2 {
                let pin = old_directory.join(format!("link_{slot}"));
                remove_injected_directory(&pin);
                std::fs::rename(old_directory.join(format!("held_link_{slot}")), &pin)
                    .expect("restore exact original pin");
            }
            faults.set(None);
            let report = app
                .retry_tc_cleanup(error)
                .expect("retire only unresolved old pins");
            assert!(report.committed_snapshot().is_some());
            assert!(!old_directory.exists());
        }
        let observed = app.get_link(first.id).expect("surviving freplace identity");
        assert!(observed.pin_present && observed.kernel.is_some());
        assert_eq!(observed.record.metadata, first.metadata);
        drop(app);
        let app = Bpfman::new(
            ActiveStore::open(faults.clone(), &c.layout, TIMEOUT).expect("reopen"),
            bpfman_kernel_aya::Kernel,
            TIMEOUT,
        );
        assert_eq!(app.unload(d).expect("unload stops member").unresolved(), 0);
        assert!(app.get_link(stop.id).is_err());
        let before = [count(&c, a), count(&c, b)];
        traffic(&network, 0);
        assert_eq!([count(&c, a) - before[0], count(&c, b) - before[1]], [3, 3]);
        app.detach_tc(first.id).expect("non-last detach");
        let before = [count(&c, a), count(&c, b)];
        traffic(&network, 0);
        assert_eq!([count(&c, a) - before[0], count(&c, b) - before[1]], [0, 3]);
        assert_eq!(app.unload(a).expect("detached a").unresolved(), 0);
        drop(app);
        let app = Bpfman::new(
            ActiveStore::open(backend.clone(), &c.layout, TIMEOUT).expect("reopen last"),
            bpfman_kernel_aya::Kernel,
            TIMEOUT,
        );
        assert_eq!(app.unload(b).expect("last attached member").unresolved(), 0);
        assert_eq!(clsact(&network), ownership != "owned");
        if ownership == "foreign" {
            for direction in ["ingress", "egress"] {
                assert_eq!(
                    tc(&network, &["filter", "show", "dev", "in0", direction])
                        .as_array()
                        .expect("filters")
                        .iter()
                        .filter(|f| f["options"]["prog"]["id"].is_number())
                        .count(),
                    1
                );
            }
        }
        traffic(&network, 3);
        c.no_artifacts();
    }
}

// Repair only empty directories injected by this test, never production deletion.
#[allow(clippy::disallowed_methods)]
fn remove_injected_directory(path: &std::path::Path) {
    std::fs::remove_dir(path).expect("repair injected entry");
}
