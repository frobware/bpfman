//! One outside-in attachment scenario, using both stores and real kernel links.

use super::{
    faults::{Faults, Point, restore_blocked_pin},
    support::*,
};
use bpfman_core::LinkCleanupKind;
use bpfman_model::LinkState;
use bpfman_runtime::{
    ActiveStore, Bpfman, Cancellation, LinkErrorKind, PreparedProgram, TracepointAttach,
};
use bpfman_store::{CommitLoad, LinkReader, LinkStore, OpenStore, UnloadStore};
use std::{num::NonZeroU32, process::Command};

pub(super) fn request(id: NonZeroU32) -> TracepointAttach {
    TracepointAttach {
        program_id: id,
        target: "syscalls/sys_enter_kill".parse().expect("target"),
        metadata: [("owner".into(), "attachment-test".into())].into(),
    }
}

fn counter(c: &Context, program: NonZeroU32) -> u64 {
    let map = aya::maps::MapData::from_pin(
        c.layout
            .map_directory_path(program)
            .join("tracepoint_stats_map"),
    )
    .expect("map");
    let map = aya::maps::Map::PerCpuArray(map);
    let values: aya::maps::PerCpuArray<_, u64> = map.try_into().expect("per-CPU counter");
    values.get(&0, 0).expect("counter").iter().sum()
}

fn fire() {
    // Aya's tracepoint attachment, like Go's cilium/ebpf implementation, opens
    // a perf event on CPU 0. Exercise that CPU explicitly for deterministic traffic.
    let output = Command::new("taskset")
        .args(["-c", "0", "sh", "-c", "kill -0 $$"])
        .output()
        .expect("fire tracepoint");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) fn assert_gone(kernel: NonZeroU32) {
    let error =
        bpfman_kernel::LinkObservations::tracepoint_link(&bpfman_kernel_aya::Kernel, kernel)
            .expect_err("link must be gone");
    assert_eq!(error.kind(), bpfman_kernel::ErrorKind::Missing);
}

pub(super) fn exercise<S>(backend: S)
where
    S: OpenStore + CommitLoad + LinkStore + UnloadStore + Clone,
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
                bpfman_model::ProgramSpec::Tracepoint(
                    bpfman_model::Symbol::try_from(NAME).expect("symbol"),
                ),
                Default::default(),
            )
            .expect("prepared"),
        )
        .expect("load");
    let id = loaded.record.id;

    let attached = app.attach_tracepoint(request(id)).expect("attach");
    let kernel_id = match attached.state {
        LinkState::Attached { kernel_id } => Some(kernel_id),
        LinkState::Pending => None,
    }
    .expect("finalised link");
    assert!(c.layout.link_pin_path(attached.id).exists());
    assert_eq!(
        app.list_link_records().expect("links"),
        std::slice::from_ref(&attached)
    );
    assert_eq!(
        app.list(&Default::default()).expect("programs")[0].links(),
        [attached.id]
    );
    let before = counter(&c, id);
    fire();
    assert!(counter(&c, id) > before, "attached program executes");

    // Reads must not wait for the giant lock, including link records.
    c.writer(|_| {
        assert_eq!(
            app.list_link_records().expect("read under writer lock"),
            std::slice::from_ref(&attached)
        );
        let observed = app
            .get_link(attached.id)
            .expect("observe under writer lock");
        assert_eq!(observed.kernel.expect("live kernel link").id, kernel_id);
        assert!(observed.pin_present);
        let program = app.get(id).expect("attached program under writer lock");
        assert_eq!(program.links.len(), 1);
        assert_eq!(program.links[0].record, attached);
        assert_eq!(
            program.links[0].kernel.as_ref().expect("link").id,
            kernel_id
        );
    });

    // A different link for the same program must not satisfy the stored kernel
    // identity. Refuse before removing either pin or deleting either record.
    let mut other_request = request(id);
    other_request.target = "syscalls/sys_enter_tkill".parse().expect("second target");
    let other = app.attach_tracepoint(other_request).expect("second attach");
    let swap = || {
        c.writer(|_| {
            let held = c.layout.root().join("fs/held-link");
            let first = c.layout.link_pin_path(attached.id);
            let second = c.layout.link_pin_path(other.id);
            std::fs::rename(&first, &held).expect("hold first pin");
            std::fs::rename(&second, &first).expect("replace first pin");
            std::fs::rename(&held, &second).expect("replace second pin");
        });
    };

    swap();
    assert_eq!(
        app.get_link(attached.id).expect_err("replaced pin").kind(),
        LinkErrorKind::InvalidState
    );
    assert!(
        app.get(id).is_err(),
        "program observation validates link identity"
    );
    let unload = app
        .unload(id)
        .expect_err("refuse replaced pin before any teardown");
    assert!(unload.report().is_none());
    c.present(id);
    let error = app.detach(attached.id).expect_err("replaced kernel link");
    assert!(error.report().is_none(), "refused before teardown");
    assert!(c.layout.link_pin_path(attached.id).exists());
    assert!(c.layout.link_pin_path(other.id).exists());
    assert_eq!(
        app.list_link_records().expect("both records remain").len(),
        2
    );

    swap();
    c.writer(|writer| {
        let pin = writer
            .observe_link_pin(&bpfman_kernel_aya::Kernel, other.id, id, None)
            .expect("observe pin")
            .expect("pin");
        writer
            .remove_link_pin(pin)
            .map_err(|e| e.cause)
            .expect("externally unpin");
    });
    let missing = app
        .get_link(other.id)
        .expect("stored intent without kernel state");
    assert!(missing.kernel.is_none());
    assert!(!missing.pin_present);
    let program = app.get(id).expect("observe absent link kernel state");
    assert_eq!(program.links.len(), 2);
    assert_eq!(program.links[1], missing);
    let report = app.detach(other.id).expect("detach second link");
    assert_eq!(report.unresolved(), 0);
    assert_eq!(report.attempts().len(), 1);
    assert_eq!(report.attempts()[0].kind, LinkCleanupKind::DeleteRecord);

    let report = app.detach(attached.id).expect("detach");
    assert_eq!(report.unresolved(), 0);
    assert_eq!(
        report.attempts().iter().map(|a| a.kind).collect::<Vec<_>>(),
        [LinkCleanupKind::RemovePin, LinkCleanupKind::DeleteRecord]
    );
    assert_gone(kernel_id);
    let before = counter(&c, id);
    fire();
    assert_eq!(
        counter(&c, id),
        before,
        "detached program no longer executes"
    );
    assert!(app.list_link_records().expect("no links").is_empty());
    c.present(id);
    assert_eq!(
        app.detach(attached.id).expect_err("already absent").kind(),
        LinkErrorKind::NotFound
    );

    // A real kernel attach failure must remove the previously committed intent.
    let mut missing = request(id);
    missing.target = "bpfman_missing_event/bpfman_missing_event"
        .parse()
        .expect("target");
    let error = app.attach_tracepoint(missing).expect_err("missing event");
    assert_eq!(error.report().expect("compensation").unresolved(), 0);
    assert!(
        app.list_link_records()
            .expect("no orphan intent")
            .is_empty()
    );

    store.set(Some(Point::FinaliseLink));
    let error = app
        .attach_tracepoint(request(id))
        .expect_err("finalisation failure");
    let report = error.report().expect("compensation report");
    assert_eq!(report.unresolved(), 0);
    assert_eq!(report.attempts().len(), 2);
    assert!(report.attempts().iter().all(|a| a.outcome.is_ok()));
    assert!(
        app.list_link_records()
            .expect("compensated intent")
            .is_empty()
    );
    assert!(!c.layout.link_pin_path(report.link_id()).exists());
    let before = counter(&c, id);
    fire();
    assert_eq!(counter(&c, id), before);

    // The environment blocks unpinning. Retain the live attachment's pending
    // record, then explicitly repair the obstruction and retry only the residue.
    store.set(Some(Point::FinaliseWithBlockedPin));
    let error = app
        .attach_tracepoint(request(id))
        .expect_err("blocked compensation");
    let report = error.report().expect("retained cleanup");
    let link_id = report.link_id();
    assert_eq!(report.unresolved(), 2);
    assert_eq!(report.attempts().len(), 1);
    assert!(report.attempts()[0].outcome.is_err());
    assert_eq!(
        app.list_link_records().expect("pending record")[0].state,
        LinkState::Pending
    );
    let blocked = app
        .unload(id)
        .expect_err("obstructed pending pin blocks unload");
    assert!(
        blocked.report().is_none(),
        "refuse malformed pin during preflight"
    );
    assert_eq!(
        app.list_link_records()
            .expect("pending intent retained")
            .len(),
        1
    );
    c.present(id);

    let cancellation = Cancellation::new();
    cancellation.cancel();
    let error = app
        .retry_link_cleanup_with_cancellation(error, &cancellation)
        .expect_err("cancel retry admission");
    assert_eq!(error.kind(), LinkErrorKind::Cancelled);
    assert_eq!(error.report().expect("retained").attempts().len(), 1);
    assert_eq!(error.report().expect("retained").unresolved(), 2);

    c.writer(|writer| restore_blocked_pin(writer, link_id));
    store.set(Some(Point::DeleteLink));
    let error = app
        .retry_link_cleanup(error)
        .expect_err("record deletion fails after successful unpin");
    let report = error.report().expect("record remains");
    assert_eq!(report.unresolved(), 1);
    assert_eq!(report.attempts().len(), 3);
    assert!(!c.layout.link_pin_path(link_id).exists());
    assert_eq!(app.list_link_records().expect("record remains").len(), 1);

    store.set(None);
    let report = app.retry_link_cleanup(error).expect("explicit retry");
    assert_eq!(report.unresolved(), 0);
    assert_eq!(report.attempts().len(), 4);
    assert!(report.primary_failure().is_some());
    assert!(app.list_link_records().expect("empty").is_empty());

    // Cancellation after creating intent compensates it; cancellation during
    // finalisation preserves the commit. Neither requires a new store setup.
    let cancellation = Cancellation::new();
    store.cancel_at(Point::CreateLink, &cancellation);
    let error = app
        .attach_tracepoint_with_cancellation(request(id), &cancellation)
        .expect_err("cancel after intent");
    assert_eq!(error.kind(), LinkErrorKind::Cancelled);
    assert_eq!(error.report().expect("cleanup").unresolved(), 0);
    assert!(app.list_link_records().expect("empty").is_empty());

    let cancellation = Cancellation::new();
    store.cancel_at(Point::FinaliseLink, &cancellation);
    let attached = app
        .attach_tracepoint_with_cancellation(request(id), &cancellation)
        .expect("commit wins");
    assert!(cancellation.is_cancelled());

    let cancellation = Cancellation::new();
    store.cancel_at(Point::ObserveLink, &cancellation);
    let error = app
        .detach_with_cancellation(attached.id, &cancellation)
        .expect_err("cancel preflight");
    assert_eq!(error.kind(), LinkErrorKind::Cancelled);
    assert!(error.report().is_none());
    assert!(c.layout.link_pin_path(attached.id).exists());

    let cancellation = Cancellation::new();
    store.cancel_at(Point::DeleteLink, &cancellation);
    let report = app
        .detach_with_cancellation(attached.id, &cancellation)
        .expect("admitted detach finishes");
    assert!(cancellation.is_cancelled());
    assert_eq!(report.unresolved(), 0);

    let report = app.unload(id).expect("unload after detach");
    assert_eq!(report.unresolved(), 0);
    c.no_artifacts();
}
