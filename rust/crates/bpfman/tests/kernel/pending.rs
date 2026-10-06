//! Recover failed attachment intent through unload, using only domain contracts.

use super::{
    faults::{Faults, Point, restore_blocked_pin},
    links::{assert_gone, request},
    support::*,
};
use bpfman_core::{LinkCleanupKind, UnloadKind};
use bpfman_model::LinkState;
use bpfman_runtime::{ActiveStore, Bpfman, Cancellation, PreparedProgram, UnloadErrorKind};
use bpfman_store::{CommitLoad, LinkReader, LinkStore, OpenStore, UnloadStore};

pub(super) fn pinned<S>(backend: S)
where
    S: OpenStore + CommitLoad + LinkStore + UnloadStore + bpfman_store::XdpReplacementStore + Clone,
    S::Reader: LinkReader,
{
    let c = Context::new();
    let store = Faults::new(backend);
    let app = Bpfman::new(
        ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("startup"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    let loaded = app
        .load(
            PreparedProgram::new(
                &bpfman_kernel_aya::Kernel,
                &fixture("tracepoint_counter.bpf.o"),
                bpfman_model::ProgramSpec::Tracepoint(NAME.try_into().expect("symbol")),
                Default::default(),
            )
            .expect("request"),
        )
        .expect("load");
    let id = loaded.record.id;

    // The entire failed attach is compensated, but an obstructed pin prevents
    // kernel cleanup. Its dependent pending record must remain recoverable.
    store.set(Some(Point::FinaliseWithBlockedPin));
    let attach_error = app
        .attach_tracepoint(request(id))
        .expect_err("failed finalisation and undo");
    let report = attach_error.report().expect("compensation report");
    let pending_id = report.link_id();

    assert!(report.primary_failure().is_some());
    assert_eq!(report.unresolved(), 2);
    assert_eq!(report.attempts().len(), 1);
    assert_eq!(report.attempts()[0].kind, LinkCleanupKind::RemovePin);
    assert!(report.attempts()[0].outcome.is_err());
    assert_eq!(store.count(Point::DeleteLink), 0);
    assert_eq!(
        app.list_link_records().expect("intent")[0].state,
        LinkState::Pending
    );

    let error = app
        .unload(id)
        .expect_err("malformed pin blocks fresh preflight");

    assert_eq!(error.kind(), UnloadErrorKind::InvalidState);
    assert!(error.report().is_none());
    assert_eq!(store.count(Point::DeleteLink), 0);
    c.present(id);

    c.writer(|writer| restore_blocked_pin(writer, pending_id));
    store.set(None);
    let kernel_id = c.writer(|writer| {
        writer
            .observe_link_pin(&bpfman_kernel_aya::Kernel, pending_id, id, None)
            .expect("validated pending pin")
            .expect("pin")
            .kernel_id()
    });
    let mut other_request = request(id);
    other_request.target = "syscalls/sys_enter_tkill".parse().expect("other target");
    let other = app
        .attach_tracepoint(other_request)
        .expect("unrelated successful attachment");

    // Simulate a caller that no longer has the original in-memory report.
    // Unload must recover persisted intent, without finalising it first.
    drop(attach_error);
    let observed = app.get_link(pending_id).expect("pending observation");

    assert_eq!(observed.record.state, LinkState::Pending);
    assert!(observed.pin_present);
    assert!(observed.kernel.is_none(), "no fabricated stored kernel ID");

    let cancellation = Cancellation::new();
    store.cancel_at(Point::ObserveLink, &cancellation);
    let error = app
        .unload_with_cancellation(id, &cancellation)
        .expect_err("cancel preflight");

    assert_eq!(error.kind(), UnloadErrorKind::Cancelled);
    assert!(error.report().is_none());
    assert!(app.get_link(pending_id).expect("preserved pin").pin_present);
    c.present(id);

    store.set(Some(Point::DeleteLink));
    let error = app
        .unload(id)
        .expect_err("pending record deletion fails after unpin");
    let report = error.report().expect("unload report");

    assert_eq!(report.attempts().len(), 2);
    assert_eq!(report.attempts()[0].kind, UnloadKind::LinkPin(pending_id));
    assert!(report.attempts()[0].outcome.is_ok());
    assert_eq!(
        report.attempts()[1].kind,
        UnloadKind::LinkRecord(pending_id)
    );
    assert!(report.attempts()[1].outcome.is_err());
    assert_gone(kernel_id);
    assert_eq!(
        app.list_link_records().expect("both records remain").len(),
        2
    );
    assert!(
        app.get_link(other.id)
            .expect("later link remains attached")
            .pin_present
    );
    c.present(id);

    let error = app
        .retry_unload(error)
        .expect_err("unchanged deletion failure");
    let report = error.report().expect("retained receipts and history");

    assert_eq!(report.attempts().len(), 3);
    assert_eq!(report.attempts()[2].id, report.attempts()[1].id);
    assert!(report.attempts()[2].outcome.is_err());

    let cancellation = Cancellation::new();
    cancellation.cancel();
    let error = app
        .retry_unload_with_cancellation(error, &cancellation)
        .expect_err("cancel retry admission");

    assert_eq!(error.kind(), UnloadErrorKind::Cancelled);
    assert_eq!(
        error.report().expect("history retained").attempts().len(),
        3
    );

    store.set(None);
    let cancellation = Cancellation::new();
    store.cancel_at(Point::DeleteLink, &cancellation);
    let report = app
        .retry_unload_with_cancellation(error, &cancellation)
        .expect("finish admitted teardown");

    assert!(cancellation.is_cancelled());
    assert_eq!(report.unresolved(), 0);
    assert_eq!(
        report
            .attempts()
            .iter()
            .filter(|a| a.outcome.is_err())
            .count(),
        2
    );
    assert!(report.attempts()[3..].iter().all(|a| a.outcome.is_ok()));
    assert!(app.list_link_records().expect("links gone").is_empty());
    assert!(
        app.list(&Default::default())
            .expect("program gone")
            .is_empty()
    );
    c.absent(id);
    c.no_artifacts();
}

pub(super) fn unpinned<S>(backend: S)
where
    S: OpenStore + CommitLoad + LinkStore + UnloadStore + bpfman_store::XdpReplacementStore + Clone,
    S::Reader: LinkReader,
{
    // Cover intent whose kernel acquisition never ran, and intent left after
    // successful kernel compensation followed by a failed record deletion.
    for acquired in [false, true] {
        let c = Context::new();
        let store = Faults::new(backend.clone());
        let app = Bpfman::new(
            ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("startup"),
            bpfman_kernel_aya::Kernel,
            TIMEOUT,
        );
        let loaded = app
            .load(
                PreparedProgram::new(
                    &bpfman_kernel_aya::Kernel,
                    &fixture("tracepoint_counter.bpf.o"),
                    bpfman_model::ProgramSpec::Tracepoint(NAME.try_into().expect("symbol")),
                    Default::default(),
                )
                .expect("request"),
            )
            .expect("load");
        let id = loaded.record.id;
        let cancellation = Cancellation::new();

        if acquired {
            store.set_many(&[Point::FinaliseLink, Point::DeleteLink]);
        } else {
            store.set(Some(Point::DeleteLink));
            store.cancel_at(Point::CreateLink, &cancellation);
        }

        let error = app
            .attach_tracepoint_with_cancellation(request(id), &cancellation)
            .expect_err("failed request retains only pending intent");
        let report = error.report().expect("compensation report");
        let pending_id = report.link_id();

        assert!(report.primary_failure().is_some());
        assert_eq!(report.unresolved(), 1);
        assert_eq!(report.attempts().len(), if acquired { 2 } else { 1 });
        if acquired {
            assert_eq!(report.attempts()[0].kind, LinkCleanupKind::RemovePin);
            assert!(report.attempts()[0].outcome.is_ok());
        }
        assert!(report.attempts().last().expect("deletion").outcome.is_err());
        assert!(
            !app.get_link(pending_id)
                .expect("pending without pin")
                .pin_present
        );
        assert_eq!(
            app.list_link_records().expect("intent")[0].state,
            LinkState::Pending
        );
        c.present(id);

        drop(error);
        store.set(None);
        let report = app
            .unload(id)
            .expect("unload cleans unpinned pending intent");

        assert_eq!(report.unresolved(), 0);
        assert_eq!(
            report.attempts()[0].kind,
            UnloadKind::LinkRecord(pending_id)
        );
        assert!(report.attempts().iter().all(|a| a.outcome.is_ok()));
        assert!(app.list_link_records().expect("links gone").is_empty());
        assert!(
            app.list(&Default::default())
                .expect("program gone")
                .is_empty()
        );
        c.absent(id);
        c.no_artifacts();
    }
}
