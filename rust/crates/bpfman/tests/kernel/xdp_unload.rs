//! Attached unload with real traffic, store failures, and retained preflight authority.
#![allow(clippy::panic)]
use super::{
    faults::{Faults, Point},
    support::*,
    xdp_attach::Interface,
    xdp_switch::traffic,
};
use bpfman_model::*;
use bpfman_runtime::{ActiveStore, Bpfman, PreparedProgram, XdpAttach};
use bpfman_store::*;

pub(super) fn exercise<S>(backend: S)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + XdpReplacementStore + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    for unload_first in [false, true] {
        let c = Context::new();
        let interface = Interface::new();
        let faults = Faults::new(backend.clone());
        let app = Bpfman::new(
            ActiveStore::open(faults.clone(), &c.layout, TIMEOUT).expect("store"),
            bpfman_kernel_aya::Kernel,
            TIMEOUT,
        );
        let load = || {
            app.load(
                PreparedProgram::new(
                    &bpfman_kernel_aya::Kernel,
                    &fixture("xdp_counter.bpf.o"),
                    ProgramSpec::Xdp("xdp_stats".try_into().expect("symbol")),
                    Default::default(),
                )
                .expect("prepare"),
            )
            .expect("load")
            .record
            .id
        };
        let a = load();
        let b = load();
        let request = |id| XdpAttach {
            program_id: id,
            interface: interface.name(),
            priority: 50,
            proceed_on: Default::default(),
            metadata: Default::default(),
        };
        let first = app.attach_xdp(request(a)).expect("first");
        let second = app.attach_xdp(request(b)).expect("second");
        let LinkDetails::Xdp(details) = &first.details else {
            panic!("XDP");
        };
        let (removed, survivor, survivor_link) = if unload_first {
            (a, b, second.id)
        } else {
            (b, a, first.id)
        };
        // One program can have several members; each detach advances the revision.
        app.attach_xdp(request(removed))
            .expect("duplicate program member");
        let before = app.get_xdp_dispatcher(details.key).expect("before");
        traffic(&c, &[a, b], &[]);
        faults.set(Some(Point::XdpReplace));
        let error = app.unload(removed).expect_err("publication failure");
        let progress = error.report().expect("retained unload");
        assert!(progress.attempts().is_empty());
        assert_eq!(progress.xdp_attempts().len(), 1);
        assert_eq!(
            progress.xdp_attempts()[0]
                .outcome()
                .expect_err("publication")
                .restoration_attempts()
                .len(),
            1
        );
        assert_eq!(
            app.get_xdp_dispatcher(details.key).expect("restored"),
            before
        );
        assert!(c.layout.program_pin_path(removed).exists());
        traffic(&c, &[a, b], &[]);
        faults.set(None);
        let pin = c.layout.program_pin_path(removed);
        let moved = pin.with_file_name("held_program");
        std::fs::rename(&pin, &moved).expect("move managed pin");
        let error = app
            .retry_unload(error)
            .expect_err("retained pin evidence rejects movement");
        assert!(error.report().expect("retained").attempts().is_empty());
        assert_eq!(
            app.get_xdp_dispatcher(details.key).expect("unchanged"),
            before
        );
        std::fs::rename(&moved, &pin).expect("restore managed pin");
        let report = app.retry_unload(error).expect("resume detach and unload");
        assert_eq!(report.unresolved(), 0);
        assert_eq!(report.xdp_attempts().len(), 3); // failed attempt plus two successful detaches
        c.absent(removed);
        assert_eq!(
            bpfman_kernel::ProgramObservations::program(&bpfman_kernel_aya::Kernel, removed)
                .expect_err("unloaded extension is released")
                .kind(),
            bpfman_kernel::ErrorKind::Missing
        );
        let after = app.get_xdp_dispatcher(details.key).expect("survivor");
        assert_eq!(after.members().len(), 1);
        assert_eq!(after.members()[0].member.id, survivor_link);
        assert_eq!(
            after.members()[0].outer_link_id,
            before.members()[0].outer_link_id
        );
        assert_eq!(
            after.members()[0].details.revision.get(),
            before.members()[0].details.revision.get() + 2
        );
        traffic(&c, &[survivor], &[]);
        faults.set(Some(Point::XdpDelete));
        let error = app
            .unload(survivor)
            .expect_err("last dispatcher record deletion");
        assert!(error.report().expect("retained").attempts().is_empty());
        faults.set(None);
        assert_eq!(
            app.retry_unload(error)
                .expect("finish last unload")
                .unresolved(),
            0
        );
        assert!(app.get_xdp_dispatcher(details.key).is_err());
        c.no_artifacts();
        assert!(names(&c.layout.root().join("fs/xdp")).is_empty());
    }
}
