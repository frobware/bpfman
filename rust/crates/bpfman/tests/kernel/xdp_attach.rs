//! Shared real-kernel first-attach ownership, refusal, and conditional retry.
#![allow(clippy::panic)]

use super::{
    faults::{Faults, Point},
    support::*,
};
use bpfman_model::{InterfaceName, LinkDetails, LinkState, ProgramSpec};
use bpfman_runtime::{ActiveStore, Bpfman, Cancellation, PreparedProgram, XdpAttach};
use bpfman_store::{
    CommitLoad, LinkReader, LinkStore, OpenStore, UnloadStore, XdpDispatcherReader,
    XdpReplacementStore,
};
use std::{fs, num::NonZeroU32, process::Command};

pub(super) struct Interface(String);

impl Interface {
    pub(super) fn new() -> Self {
        let name = format!("bxa{}", std::process::id());
        let peer = format!("bxb{}", std::process::id());
        let out = Command::new("ip")
            .args(["link", "add", &name, "type", "veth", "peer", "name", &peer])
            .output()
            .expect("ip");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let lease = Self(name);
        for iface in [&lease.0, &peer] {
            assert!(
                Command::new("ip")
                    .args(["link", "set", iface, "up"])
                    .status()
                    .expect("ip up")
                    .success()
            );
        }
        lease
    }

    pub(super) fn name(&self) -> InterfaceName {
        self.0.parse().expect("interface")
    }
}

impl Drop for Interface {
    fn drop(&mut self) {
        let result = Command::new("ip").args(["link", "del", &self.0]).output();
        if !std::thread::panicking() {
            assert!(result.expect("delete interface").status.success());
        }
    }
}

fn request(program_id: NonZeroU32, interface: &Interface) -> XdpAttach {
    XdpAttach {
        program_id,
        interface: interface.name(),
        priority: 50,
        proceed_on: Default::default(),
        metadata: Default::default(),
    }
}

fn gone(id: NonZeroU32) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match bpfman_kernel::LinkObservations::extension_link(&bpfman_kernel_aya::Kernel, id) {
            Err(e) if e.kind() == bpfman_kernel::ErrorKind::Missing => return,
            Ok(_) => {}
            Err(e) => {
                panic!("unexpected observation: {e}");
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "extension link leaked"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

pub(super) fn exercise<S>(backend: S)
where
    S: OpenStore + CommitLoad + UnloadStore + LinkStore + XdpReplacementStore + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    let c = Context::new();
    let interface = Interface::new();
    let store = Faults::new(backend.clone());
    let app = Bpfman::new(
        ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("store"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    let program = app
        .load(
            PreparedProgram::new(
                &bpfman_kernel_aya::Kernel,
                &fixture("xdp_pass.bpf.o"),
                ProgramSpec::Xdp("pass".try_into().expect("symbol")),
                Default::default(),
            )
            .expect("prepare"),
        )
        .expect("load")
        .record
        .id;

    store.set(Some(Point::XdpCommit));
    let failed = app
        .attach_xdp(request(program, &interface))
        .expect_err("commit fault");
    assert_eq!(failed.unresolved(), 0);
    assert!(app.list_link_records().expect("links").is_empty());
    assert!(names(&c.layout.root().join("fs/xdp")).is_empty());
    assert!(c.layout.program_pin_path(program).exists());
    store.set(None);

    let first = app
        .attach_xdp(request(program, &interface))
        .expect("attach");
    let LinkDetails::Xdp(details) = &first.details else {
        panic!("XDP details");
    };
    let snapshot = app.get_xdp_dispatcher(details.key).expect("snapshot");
    c.writer(|_| {
        assert_eq!(
            app.get_xdp_dispatcher(details.key).expect("lock-free read"),
            snapshot
        );
        assert!(app.get_link(first.id).expect("link read").pin_present);
    });
    let mut invalid = request(program, &interface);
    invalid.priority = u32::MAX;
    assert!(app.attach_xdp(invalid).is_err());
    assert_eq!(
        app.get_xdp_dispatcher(details.key).expect("unchanged"),
        snapshot
    );

    // A second runtime has no stored occupancy evidence. The actual interface
    // attachment must still refuse replacement and compensate its acquisitions.
    let foreign = Context::new();
    let foreign_app = Bpfman::new(
        ActiveStore::open(backend, &foreign.layout, TIMEOUT).expect("foreign store"),
        bpfman_kernel_aya::Kernel,
        TIMEOUT,
    );
    let foreign_program = foreign_app
        .load(
            PreparedProgram::new(
                &bpfman_kernel_aya::Kernel,
                &fixture("xdp_pass.bpf.o"),
                ProgramSpec::Xdp("pass".try_into().expect("symbol")),
                Default::default(),
            )
            .expect("foreign prepare"),
        )
        .expect("foreign load")
        .record
        .id;
    let failed = foreign_app
        .attach_xdp(request(foreign_program, &interface))
        .expect_err("foreign interface attachment");
    assert_eq!(failed.unresolved(), 0);
    assert!(
        foreign_app
            .list_link_records()
            .expect("foreign links")
            .is_empty()
    );
    assert!(names(&foreign.layout.root().join("fs/xdp")).is_empty());
    assert_eq!(
        app.get_xdp_dispatcher(details.key)
            .expect("original snapshot"),
        snapshot
    );
    assert!(app.get_link(first.id).expect("original link").pin_present);
    assert_eq!(
        foreign_app
            .unload(foreign_program)
            .expect("foreign unload")
            .unresolved(),
        0
    );
    foreign.no_artifacts();

    // Refusing attached unload must precede every destructive operation.
    assert!(app.unload(program).is_err());
    assert!(c.layout.program_pin_path(program).exists());
    assert!(app.get_link(first.id).expect("still attached").pin_present);

    // A wrong kernel object at an otherwise canonical path is not adopted.
    let extension = c.layout.xdp_extension_path(details.key, details.revision);
    // Observations must use the recorded slot, never silently inspect link_0.
    let runtime = bpfman_fs::RuntimeDirectory::open_existing(c.layout.clone())
        .expect("open runtime")
        .expect("runtime exists");
    let mut another_slot = details.clone();
    another_slot.slot = 1usize.try_into().expect("slot");
    assert!(
        runtime
            .read_xdp_link_pin(&bpfman_kernel_aya::Kernel, &another_slot)
            .expect("empty slot")
            .is_none()
    );
    let slot_pin = c
        .layout
        .xdp_slot_path(details.key, details.revision, another_slot.slot);
    fs::rename(&extension, &slot_pin).expect("move pin to slot one");
    let observed = runtime
        .read_xdp_link_pin(&bpfman_kernel_aya::Kernel, &another_slot)
        .expect("slot one")
        .expect("pin present");
    assert_eq!(
        first.state,
        LinkState::Attached {
            kernel_id: observed.id
        }
    );
    assert!(
        runtime
            .read_xdp_link_pin(&bpfman_kernel_aya::Kernel, details)
            .expect("empty original slot")
            .is_none()
    );
    fs::rename(&slot_pin, &extension).expect("restore pin");

    let outer = c.layout.xdp_outer_path(details.key);
    let saved = c
        .layout
        .xdp_outer_path(details.key)
        .with_file_name("saved_extension");
    fs::rename(&extension, &saved).expect("save extension");
    fs::rename(&outer, &extension).expect("substitute outer");
    assert!(app.detach_xdp(first.id).is_err());
    assert!(
        c.layout
            .xdp_program_path(details.key, details.revision)
            .exists()
    );
    fs::rename(&extension, &outer).expect("restore outer");
    fs::rename(&saved, &extension).expect("restore extension");

    // An absent outer pin is not proof of detach. Refuse before touching the
    // revision when another pin (or a retained descriptor) keeps the link live.
    let held_outer: aya::programs::links::FdLink =
        aya::programs::links::PinnedLink::from_pin(&outer)
            .expect("retain outer descriptor")
            .into();
    let saved_outer = outer.with_file_name("saved_outer");
    fs::rename(&outer, &saved_outer).expect("move outer");
    assert!(app.detach_xdp(first.id).is_err());
    assert!(extension.exists());
    fs::rename(&saved_outer, &outer).expect("restore outer");

    // Store deletion can fail after all kernel artifacts were released. Retry
    // retains conditional evidence, including when retry admission is cancelled.
    store.set(Some(Point::XdpDelete));
    let failed = app.detach_xdp(first.id).expect_err("delete fault");
    assert_eq!(failed.unresolved(), 1);
    assert!(names(&c.layout.root().join("fs/xdp")).is_empty());
    assert!(app.get_xdp_dispatcher(details.key).is_ok());
    let LinkState::Attached { kernel_id } = first.state else {
        panic!("attached");
    };
    gone(kernel_id);
    let cancelled = Cancellation::new();
    cancelled.cancel();
    let failed = app
        .retry_xdp_cleanup_with_cancellation(failed, &cancelled)
        .expect_err("cancel retry admission");
    assert_eq!(failed.unresolved(), 1);
    assert_eq!(store.count(Point::XdpDelete), 1);
    store.set(None);
    assert_eq!(
        app.retry_xdp_cleanup(failed).expect("retry").unresolved(),
        0
    );
    assert!(app.get_xdp_dispatcher(details.key).is_err());
    assert!(app.get_link(first.id).is_err());

    let second = app
        .attach_xdp(request(program, &interface))
        .expect("reattach");
    // Reattachment while this descriptor remains open proves synchronous detach,
    // rather than relying on the final close of the old link.
    assert!(held_outer.info().is_ok());
    drop(held_outer);
    let LinkDetails::Xdp(next) = &second.details else {
        panic!("XDP");
    };
    assert_ne!(next.dispatcher_id, details.dispatcher_id);
    assert_ne!(second.id, first.id);
    app.detach_xdp(second.id).expect("last detach");
    assert_eq!(app.unload(program).expect("unload").unresolved(), 0);
    c.no_artifacts();
    assert!(names(&c.layout.root().join("fs/xdp")).is_empty());
}
