//! Reopened attached unload: exact filters, namespace scope, clsact and retained retries.
#![allow(clippy::panic)]
use super::{
    faults::{Faults, Point},
    support::*,
    tc::{clsact, count, request, tc, traffic},
    xdp_delivery::Network,
};
use bpfman_model::{LinkDetails, ProgramSpec};
use bpfman_runtime::{ActiveStore, Bpfman, Cancellation, PreparedProgram, UnloadErrorKind};
use bpfman_store::*;

pub(super) fn exercise<S>(backend: S)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + XdpReplacementStore + TcStore + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    for ownership in ["owned", "borrowed", "foreign"] {
        let c = Context::new();
        let first = Network::new();
        let second = Network::new();
        let faults = Faults::new(backend.clone());
        let app = Bpfman::new(
            ActiveStore::open(faults.clone(), &c.layout, TIMEOUT).expect("store"),
            bpfman_kernel_aya::Kernel,
            TIMEOUT,
        );
        let id = app
            .load(
                PreparedProgram::new(
                    &bpfman_kernel_aya::Kernel,
                    &fixture("tc_ingress.bpf.o"),
                    ProgramSpec::Tc("tc_ingress".try_into().expect("symbol")),
                    Default::default(),
                )
                .expect("prepare"),
            )
            .expect("load")
            .record
            .id;
        if ownership == "borrowed" {
            for network in [&first, &second] {
                tc(network, &["qdisc", "add", "dev", "in0", "clsact"]);
            }
        }
        let a = app.attach_tc(request(id, &first)).expect("first namespace");
        let b = app
            .attach_tc(request(id, &second))
            .expect("second namespace");
        let LinkDetails::Tc(details) = &b.details else {
            panic!("TC");
        };
        let marker = c.layout.root().join("tc").join(format!(
            "dispatcher_{}_{}_1",
            details.key.nsid, details.key.ifindex
        ));
        let evidence = std::fs::read(&marker).expect("ownership");
        std::fs::write(&marker, [9]).expect("malformed later attachment evidence");
        let error = app
            .unload(id)
            .expect_err("all attachments preflight before effects");
        assert!(error.report().is_none());
        assert!(std::path::Path::new(&a.pin_path).exists());
        assert!(std::path::Path::new(&b.pin_path).exists());
        traffic(&first, 0);
        traffic(&second, 0);
        std::fs::write(&marker, evidence).expect("restore this test's evidence");

        let mut foreign = Vec::new();
        if ownership == "foreign" {
            for (network, record) in [(&first, &a), (&second, &b)] {
                let LinkDetails::Tc(d) = &record.details else {
                    panic!("TC");
                };
                for direction in ["ingress", "egress"] {
                    tc(
                        network,
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
                    let rows = tc(network, &["filter", "show", "dev", "in0", direction]);
                    foreign.push(
                        rows.as_array()
                            .expect("filter list")
                            .iter()
                            .filter(|f| {
                                f["options"]["prog"]["id"]
                                    .as_u64()
                                    .is_some_and(|id| id != u64::from(d.dispatcher_id.get()))
                            })
                            .cloned()
                            .collect::<Vec<_>>(),
                    );
                }
            }
            assert!(
                foreign.iter().all(|f| f.len() == 1),
                "foreign filter identities: {foreign:?}"
            );
        }
        drop(app);
        let app = Bpfman::new(
            ActiveStore::open(faults.clone(), &c.layout, TIMEOUT).expect("reopen"),
            bpfman_kernel_aya::Kernel,
            TIMEOUT,
        );
        faults.fail_after(Point::TcDelete, 1);
        let error = app.unload(id).expect_err("second record deletion fails");
        let report = error.report().expect("retained program prerequisites");
        assert!(report.attempts().is_empty());
        assert_eq!(report.tc_attempts().len(), 2);
        assert!(report.tc_attempts()[0].outcome().is_ok());
        assert_eq!(
            report.tc_attempts()[1]
                .outcome()
                .expect_err("record")
                .unresolved(),
            1
        );
        assert!(c.layout.program_pin_path(id).exists());
        assert!(c.layout.map_directory_path(id).exists());
        assert_eq!(
            app.list_link_records()
                .expect("only failed intent")
                .as_slice(),
            std::slice::from_ref(&b)
        );
        let before = count(&c, id);
        traffic(&first, 3);
        traffic(&second, 3);
        assert_eq!(
            count(&c, id),
            before,
            "detached traffic no longer executes extension"
        );
        for network in [&first, &second] {
            assert_eq!(clsact(network), ownership != "owned");
        }
        let token = Cancellation::new();
        token.cancel();
        let error = app
            .retry_unload_with_cancellation(error, &token)
            .expect_err("cancelled retry admission");
        assert_eq!(error.kind(), UnloadErrorKind::Cancelled);
        assert_eq!(
            error.report().expect("report").tc_attempts()[1]
                .outcome()
                .expect_err("retained")
                .attempts()
                .len(),
            3
        );
        let other = Context::new();
        let foreign_app = Bpfman::new(
            ActiveStore::open(Faults::new(backend.clone()), &other.layout, TIMEOUT)
                .expect("foreign runtime"),
            bpfman_kernel_aya::Kernel,
            TIMEOUT,
        );
        let error = foreign_app
            .retry_unload(error)
            .expect_err("foreign runtime cannot consume receipts");
        assert_eq!(error.kind(), UnloadErrorKind::InvalidState);
        drop(app);
        let app = Bpfman::new(
            ActiveStore::open(faults.clone(), &c.layout, TIMEOUT).expect("reopen for retry"),
            bpfman_kernel_aya::Kernel,
            TIMEOUT,
        );
        faults.set(None);
        // A new writer can attach while the program is retained. The original
        // unload cannot silently remove a program under that new attachment.
        let error = if ownership == "owned" {
            let new = app
                .attach_tc(request(id, &first))
                .expect("new attachment while recovery retained");
            let error = app
                .retry_unload(error)
                .expect_err("new prerequisite blocks program removal");
            assert_eq!(error.kind(), UnloadErrorKind::InvalidState);
            assert!(error.report().expect("report").attempts().is_empty());
            traffic(&first, 0);
            app.detach_tc(new.id)
                .expect("explicitly handle new attachment");
            error
        } else {
            error
        };
        faults.set(Some(Point::DeleteProgram));
        let token = Cancellation::new();
        faults.cancel_at(Point::DeleteProgram, &token);
        let error = app
            .retry_unload_with_cancellation(error, &token)
            .expect_err("program record failure after detaches");
        assert!(
            token.is_cancelled(),
            "admitted cleanup reaches program record despite late cancellation"
        );
        assert!(
            error
                .report()
                .expect("report")
                .tc_attempts()
                .iter()
                .all(|a| a.outcome().is_ok())
        );
        assert!(!c.layout.program_pin_path(id).exists());
        faults.set(None);
        let report = app
            .retry_unload(error)
            .expect("finish program record and GC");
        assert_eq!(report.unresolved(), 0);
        assert_eq!(
            report.tc_attempts()[0]
                .outcome()
                .expect("first completed once")
                .attempts()
                .len(),
            3
        );
        assert_eq!(
            report.tc_attempts()[1]
                .outcome()
                .expect("record recovered")
                .attempts()
                .len(),
            4
        );
        c.absent(id);
        c.no_artifacts();
        assert_eq!(
            bpfman_kernel::ProgramObservations::program(&bpfman_kernel_aya::Kernel, id)
                .expect_err("extension released")
                .kind(),
            bpfman_kernel::ErrorKind::Missing
        );
        for network in [&first, &second] {
            traffic(network, 3);
            assert_eq!(clsact(network), ownership != "owned");
        }
        if ownership == "foreign" {
            let mut index = 0;
            for network in [&first, &second] {
                for direction in ["ingress", "egress"] {
                    assert_eq!(
                        tc(network, &["filter", "show", "dev", "in0", direction])
                            .as_array()
                            .expect("foreign filters")
                            .iter()
                            .filter(|f| f["options"]["prog"]["id"].is_u64())
                            .cloned()
                            .collect::<Vec<_>>(),
                        foreign[index]
                    );
                    index += 1;
                }
            }
        }
    }
}
