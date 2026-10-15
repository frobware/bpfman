//! Real probe pin identity, pending-intent compensation and owned-object lifetime.
use super::{
    faults::{Faults, Point},
    support::*,
};
use bpfman_kernel::{LinkObservations, ProgramObservations};
use bpfman_model::{LinkState, ProgramSpec};
use bpfman_runtime::{ActiveStore, Bpfman, Cancellation, KprobeAttach, PreparedProgram};
use bpfman_store::*;
use std::num::NonZeroU32;

fn request(id: NonZeroU32) -> KprobeAttach {
    KprobeAttach {
        program_id: id,
        function: "bpfman_e2e_target_0".parse().expect("function"),
        offset: 0,
        metadata: Default::default(),
    }
}

fn absent(id: NonZeroU32) {
    assert_eq!(
        bpfman_kernel_aya::Kernel::default()
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
    for retprobe in [false, true] {
        let c = Context::new();
        let store = Faults::new(backend.clone());
        let app = Bpfman::new(
            ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("store"),
            bpfman_kernel_aya::Kernel::default(),
            TIMEOUT,
        );
        let load = || {
            let name = "kprobe_counter".try_into().expect("symbol");
            app.load(
                PreparedProgram::new(
                    &bpfman_kernel_aya::Kernel::default(),
                    &fixture("kprobe_counter.bpf.o"),
                    if retprobe {
                        ProgramSpec::Kretprobe(name)
                    } else {
                        ProgramSpec::Kprobe(name)
                    },
                    Default::default(),
                )
                .expect("prepare"),
            )
            .expect("load")
            .record
            .id
        };
        let id = load();
        store.set(Some(Point::FinaliseLink));
        let error = app
            .attach_kprobe(request(id))
            .expect_err("finalise failure");
        assert_eq!(error.report().expect("compensation").unresolved(), 0);
        store.set(None);
        assert!(
            app.list_link_records()
                .expect("no pending intent")
                .is_empty()
        );

        let token = Cancellation::new();
        store.cancel_at(Point::CreateLink, &token);
        let error = app
            .attach_kprobe_with_cancellation(request(id), &token)
            .expect_err("cancel after intent");
        assert_eq!(error.report().expect("compensation").unresolved(), 0);
        assert!(
            app.list_link_records()
                .expect("no cancelled intent")
                .is_empty()
        );

        let first = app.attach_kprobe(request(id)).expect("first");
        let second = app.attach_kprobe(request(id)).expect("second");
        let saved = c.layout.root().join("fs/saved-probe-link");
        let swap = || {
            std::fs::rename(&first.pin_path, &saved).expect("save");
            std::fs::rename(&second.pin_path, &first.pin_path).expect("replace");
            std::fs::rename(&saved, &second.pin_path).expect("swap");
        };
        swap();
        let error = app
            .unload(id)
            .expect_err("changed pin identity refuses before effects");
        assert!(error.report().is_none());
        assert!(std::path::Path::new(&first.pin_path).exists());
        assert!(std::path::Path::new(&second.pin_path).exists());
        swap();

        // A failed record deletion cannot block independent pin removal. Reopen
        // and retry only the retained records, without adopting completed pins.
        store.set(Some(Point::DeleteLink));
        let error = app.unload(id).expect_err("record failure");
        assert!(bpfman_kernel_aya::Kernel::default().program(id).is_ok());
        for link in [&first, &second] {
            assert!(!std::path::Path::new(&link.pin_path).exists());
            let LinkState::Attached { kernel_id } = link.state else {
                unreachable!()
            };
            assert_eq!(
                bpfman_kernel_aya::Kernel::default()
                    .tracepoint_link(kernel_id)
                    .expect_err("owned link released")
                    .kind(),
                bpfman_kernel::ErrorKind::Missing
            );
        }
        store.set(None);
        let reopened = Bpfman::new(
            ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("reopen"),
            bpfman_kernel_aya::Kernel::default(),
            TIMEOUT,
        );
        assert_eq!(
            reopened
                .retry_unload(error)
                .expect("record-only retry")
                .unresolved(),
            0
        );
        absent(id);
        c.no_artifacts();

        // A genuine external holder protects the program until its close.
        let id = load();
        let held_program = aya::programs::ProgramInfo::from_pin(c.layout.program_pin_path(id))
            .expect("program")
            .fd()
            .expect("holder");
        app.attach_kprobe(request(id)).expect("control attach");
        assert_eq!(app.unload(id).expect("held program unload").unresolved(), 0);
        assert!(bpfman_kernel_aya::Kernel::default().program(id).is_ok());
        drop(held_program);
        absent(id);
        c.no_artifacts();
    }
}
