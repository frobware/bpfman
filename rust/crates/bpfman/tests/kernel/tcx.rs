//! Native TCX teardown, extra holders, confinement and reopened consuming retries.
#![allow(clippy::panic)]
use super::{
    faults::{Faults, Point},
    support::*,
    tc::clsact,
    xdp_delivery::Network,
};
use bpfman_kernel::{LinkObservations, ProgramObservations, TcxLinks};
use bpfman_model::{KernelLinkDetails, LinkDetails, LinkState, ProgramSpec, TcDirection};
use bpfman_runtime::{ActiveStore, Bpfman, Cancellation, PreparedProgram, TcxAttach};
use bpfman_store::*;
use std::num::NonZeroU32;

fn request(id: NonZeroU32, network: &Network, direction: TcDirection) -> TcxAttach {
    TcxAttach {
        program_id: id,
        interface: "in0".parse().expect("interface"),
        netns: network.namespace(),
        direction,
        priority: 0,
        metadata: Default::default(),
    }
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
    for direction in [TcDirection::Ingress, TcDirection::Egress] {
        let c = Context::new();
        let other = Context::new();
        let first = Network::new();
        let second = Network::new();
        let aliases = tempfile::tempdir().expect("namespace selector aliases");
        let selector = aliases.path().join("selected");
        let saved = aliases.path().join("saved");
        let rebound = aliases.path().join("rebound");
        std::os::unix::fs::symlink(first.namespace().as_str(), &selector).expect("selector");
        let selected = |id, hook| {
            let mut r = request(id, &first, hook);
            r.netns = selector
                .to_str()
                .expect("UTF8 selector")
                .parse()
                .expect("selector");
            r
        };
        let store = Faults::new(backend.clone());
        let app = Bpfman::new(
            ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("store"),
            bpfman_kernel_aya::Kernel::default(),
            TIMEOUT,
        );
        let load = || {
            app.load(
                PreparedProgram::new(
                    &bpfman_kernel_aya::Kernel::default(),
                    &fixture("tcx_counter.bpf.o"),
                    ProgramSpec::Tcx("tcx_stats".try_into().expect("symbol")),
                    Default::default(),
                )
                .expect("prepare TCX"),
            )
            .expect("native load")
            .record
            .id
        };
        let id = load();
        let opposite = match direction {
            TcDirection::Ingress => TcDirection::Egress,
            TcDirection::Egress => TcDirection::Ingress,
        };
        // Prepared native attachment retains namespace evidence and refuses a
        // rebound selector before acquiring a link, even at the same ifindex.
        let r = selected(id, direction);
        let (_, prepared) = c.writer(|w| {
            bpfman_kernel_aya::Kernel::default()
                .prepare_tcx(w, id, &r.interface, &r.netns, r.direction, r.priority)
                .expect("prepare retained namespace")
        });
        std::fs::rename(&selector, &saved).expect("hide selector");
        std::os::unix::fs::symlink(second.namespace().as_str(), &selector)
            .expect("rebound selector");
        c.writer(|w| {
            assert!(
                bpfman_kernel_aya::Kernel::default()
                    .attach_tcx(w, prepared, bpfman_model::TcxOrder::First)
                    .is_err(),
                "changed namespace refuses attachment"
            )
        });
        std::fs::rename(&selector, &rebound).expect("save rebound selector");
        std::fs::rename(&saved, &selector).expect("restore original selector");
        let links = [
            app.attach_tcx(selected(id, direction)).expect("first hook"),
            app.attach_tcx(selected(id, opposite))
                .expect("opposite hook"),
            app.attach_tcx(request(id, &second, direction))
                .expect("independent namespace"),
        ];
        assert!(!clsact(&first, "in0"));
        assert!(!clsact(&second, "in0"));
        let before = app.list_link_records().expect("snapshot");
        let held = c.layout.root().join("fs/held-tcx");
        std::fs::rename(&links[2].pin_path, &held).expect("hold test pin");
        std::fs::create_dir(&links[2].pin_path).expect("obstruct later pin");
        let error = app
            .unload(id)
            .expect_err("all links admitted before effects");
        assert!(error.report().is_none());
        assert_eq!(app.list_link_records().expect("unchanged"), before);
        for link in &links[..2] {
            assert!(
                app.get_link(link.id)
                    .expect("prior links untouched")
                    .pin_present
            );
        }
        // This directory was created immediately above solely to obstruct this
        // fixture's pin. Production removal still goes through owned receipts.
        #[allow(clippy::disallowed_methods)]
        {
            std::fs::remove_dir(&links[2].pin_path).expect("remove test obstruction");
        }
        std::fs::rename(&held, &links[2].pin_path).expect("restore pin");
        let LinkDetails::Tcx(target) = &links[0].details else {
            panic!("TCX")
        };
        let LinkState::Attached { kernel_id } = links[0].state else {
            panic!("attached")
        };
        let pin = c.writer(|w| {
            w.observe_tcx_link_pin(
                &bpfman_kernel_aya::Kernel::default(),
                target,
                id,
                Some(kernel_id),
            )
            .expect("observe")
            .expect("pin")
        });
        let pin = other.writer(|w| {
            w.remove_link_pin(&bpfman_kernel_aya::Kernel::default(), pin)
                .expect_err("foreign runtime")
                .remaining
        });
        drop(pin);
        assert!(
            app.get_link(links[0].id)
                .expect("still attached")
                .pin_present
        );

        // Explicit detach stops execution even while another descriptor holds the
        // old link object. Reattachment must succeed before that descriptor closes.
        let held_link: aya::programs::links::FdLink =
            aya::programs::links::PinnedLink::from_pin(&links[0].pin_path)
                .expect("external holder")
                .into();
        store.set(Some(Point::DeleteLink));
        let error = app.detach(links[0].id).expect_err("record-only failure");
        assert_eq!(error.report().expect("retained record").unresolved(), 1);
        assert!(matches!(
            bpfman_kernel_aya::Kernel::default()
                .tcx_link(kernel_id)
                .expect("held object")
                .details,
            KernelLinkDetails::Tcx { ifindex: 0, .. }
        ));
        assert!(!std::path::Path::new(&links[0].pin_path).exists());
        store.set(None);
        assert_eq!(
            app.retry_link_cleanup(error)
                .expect("record-only retry")
                .unresolved(),
            0
        );
        let replacement = app
            .attach_tcx(selected(id, direction))
            .expect("immediate reattach with old link held");
        drop(held_link);
        assert_eq!(
            bpfman_kernel_aya::Kernel::default()
                .tcx_link(kernel_id)
                .expect_err("released old link")
                .kind(),
            bpfman_kernel::ErrorKind::Missing
        );

        store.set(Some(Point::FinaliseLink));
        let other_id = load();
        let error = app
            .attach_tcx(request(other_id, &second, opposite))
            .expect_err("finalise failure compensates native attach");
        assert_eq!(error.report().expect("compensation").unresolved(), 0);
        store.set(None);
        assert_eq!(app.unload(other_id).expect("other program").unresolved(), 0);
        absent(other_id, "compensated attachment program");

        // Native teardown uses its exact link, even after the stored namespace
        // name disappears or points elsewhere. The original namespace stays live.
        std::fs::rename(&selector, &saved).expect("remove stored selector");
        if direction == TcDirection::Ingress {
            std::os::unix::fs::symlink(second.namespace().as_str(), &selector)
                .expect("rebind stored selector");
        }
        store.set(Some(Point::DeleteLink));
        let error = app
            .unload(id)
            .expect_err("record deletion blocks program teardown");
        assert!(c.layout.program_pin_path(id).exists());
        for link in [&replacement, &links[1], &links[2]] {
            assert!(
                !std::path::Path::new(&link.pin_path).exists(),
                "pin {} survived: {:#?}",
                link.pin_path,
                error.report().map(|r| r
                    .attempts()
                    .iter()
                    .map(|a| (&a.kind, &a.outcome))
                    .collect::<Vec<_>>())
            );
            let LinkState::Attached { kernel_id } = link.state else {
                panic!("attached")
            };
            assert_eq!(
                bpfman_kernel_aya::Kernel::default()
                    .tcx_link(kernel_id)
                    .expect_err("owned native link released immediately")
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
        let report = reopened
            .retry_unload(error)
            .expect("reopened record-only retry");
        assert_eq!(report.unresolved(), 0);
        absent(id, "reopened multi-hook unload");
        c.absent(id);
        c.no_artifacts();

        let id = load();
        let token = Cancellation::new();
        store.cancel_at(Point::CreateLink, &token);
        let error = app
            .attach_tcx_with_cancellation(request(id, &first, direction), &token)
            .expect_err("cancelled intent compensates");
        assert_eq!(error.report().expect("compensated").unresolved(), 0);
        assert!(app.list_link_records().expect("empty").is_empty());
        // A genuine external program holder is observable until its close.
        let program = aya::programs::ProgramInfo::from_pin(c.layout.program_pin_path(id))
            .expect("program info")
            .fd()
            .expect("program holder");
        app.attach_tcx(request(id, &first, direction))
            .expect("attach control");
        let report = app.unload(id).expect("unload control");
        assert_eq!(report.unresolved(), 0);
        assert!(bpfman_kernel_aya::Kernel::default().program(id).is_ok());
        drop(program);
        absent(id, "external program holder closed");
        c.no_artifacts();
    }
}

fn absent(id: NonZeroU32, boundary: &str) {
    let observation = bpfman_kernel_aya::Kernel::default().program(id);
    assert!(
        matches!(&observation,Err(error) if error.kind() == bpfman_kernel::ErrorKind::Missing),
        "{boundary}: owned program {id} remains observable: {observation:?}"
    );
}
