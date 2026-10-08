//! Namespace identity, caller isolation, and retained unload recovery on both stores.
#![allow(clippy::panic)]

use super::{
    faults::{Faults, Point},
    support::*,
};
use bpfman_kernel::XdpLifecycle;
use bpfman_model::{LinkDetails, NetworkNamespace, ProgramSpec, XdpKey};
use bpfman_runtime::{ActiveStore, Bpfman, PreparedProgram, XdpAttach};
use bpfman_store::*;
use std::{fs, os::unix::fs::symlink, path::PathBuf, process::Command};

struct Namespace {
    name: String,
    present: bool,
}

fn ip(args: &[&str]) {
    let output = Command::new("ip").args(args).output().expect("ip");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

impl Namespace {
    fn new(suffix: &str) -> Self {
        let name = format!("bpfman-rust-{}-{suffix}", std::process::id());
        ip(&["netns", "add", &name]);
        let ns = Self {
            name,
            present: true,
        };
        ip(&[
            "-n", &ns.name, "link", "add", "same0", "type", "veth", "peer", "name", "same1",
        ]);
        for name in ["same0", "same1"] {
            ip(&["-n", &ns.name, "link", "set", name, "up"]);
        }
        ns
    }

    fn path(&self) -> PathBuf {
        PathBuf::from("/run/netns").join(&self.name)
    }

    fn delete(&mut self) {
        ip(&["netns", "del", &self.name]);
        self.present = false;
    }
}

impl Drop for Namespace {
    fn drop(&mut self) {
        if self.present {
            let result = Command::new("ip")
                .args(["netns", "del", &self.name])
                .output();
            if !std::thread::panicking() {
                assert!(result.expect("namespace cleanup").status.success());
            }
        }
    }
}

fn key(link: &bpfman_model::StoredLink) -> XdpKey {
    let LinkDetails::Xdp(details) = &link.details else {
        panic!("XDP link")
    };
    details.key
}

pub(super) fn exercise<S>(backend: S)
where
    S: OpenStore
        + CommitLoad
        + UnloadStore
        + LinkStore
        + XdpReplacementStore
        + bpfman_store::TcStore
        + Clone,
    S::Reader: LinkReader + XdpDispatcherReader,
{
    let c = Context::new();
    let first_ns = Namespace::new("a");
    let mut second_ns = Namespace::new("b");
    let original = fs::read_link("/proc/thread-self/ns/net").expect("caller namespace");
    let faults = Faults::new(backend);
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
    let parent = c.layout.root().parent().expect("temporary directory");
    let alias = parent.join("namespace");
    let saved = parent.join("saved-namespace");
    let foreign = parent.join("foreign-namespace");
    symlink(first_ns.path(), &alias).expect("namespace alias");
    let select = |path: &std::path::Path| -> NetworkNamespace {
        path.to_str().expect("path").parse().expect("selection")
    };
    let request = |id, netns| XdpAttach {
        program_id: id,
        interface: "same0".parse().expect("interface"),
        netns,
        mode: Default::default(),
        priority: 50,
        proceed_on: Default::default(),
        metadata: Default::default(),
    };

    let regular = parent.join("regular");
    fs::write(&regular, b"not a namespace").expect("regular file");
    for invalid in [
        parent.join("missing"),
        regular,
        parent.to_owned(),
        PathBuf::from("/proc/thread-self/ns/mnt"),
    ] {
        assert!(app.attach_xdp(request(a, select(&invalid))).is_err());
        assert!(
            app.list_xdp_dispatchers()
                .expect("empty dispatchers")
                .is_empty()
        );
        assert!(app.list_link_records().expect("empty links").is_empty());
        assert_eq!(
            fs::read_link("/proc/thread-self/ns/net").expect("caller"),
            original
        );
    }

    // Path replacement after preparation must fail before creating an outer link.
    c.writer(|w| {
        let kernel = bpfman_kernel_aya::Kernel;
        let (_, prepared) = kernel
            .prepare_xdp(w, a, &request(a, select(&alias)).interface, &select(&alias))
            .expect("prepare retained namespace");
        let dispatcher = kernel
            .load_dispatcher(&bpfman_model::XdpConfig::single(Default::default()))
            .expect("dispatcher");
        fs::rename(&alias, &saved).expect("hide namespace after preparation");
        let Err(missing) = kernel.pin_outer(w, &prepared, &dispatcher, Default::default()) else {
            panic!("missing path must not attach");
        };
        assert!(missing.remaining.is_none());
        symlink(second_ns.path(), &alias).expect("replace namespace after preparation");
        let Err(replaced) = kernel.pin_outer(w, &prepared, &dispatcher, Default::default()) else {
            panic!("replaced path must not attach");
        };
        assert!(replaced.remaining.is_none());
        assert!(names(&c.layout.root().join("fs/xdp")).is_empty());
        fs::rename(&alias, &foreign).expect("save replacement");
        fs::rename(&saved, &alias).expect("restore admitted namespace");
    });

    let first = app
        .attach_xdp(request(a, select(&alias)))
        .expect("first namespace");
    let survivor = app
        .attach_xdp(request(b, select(&first_ns.path())))
        .expect("rebuild through an alias of the same namespace");
    let other = app
        .attach_xdp(request(a, select(&second_ns.path())))
        .expect("second namespace");
    assert_eq!(key(&first).ifindex, key(&other).ifindex);
    assert_ne!(key(&first).nsid, key(&other).nsid);
    assert_eq!(
        app.list_xdp_dispatchers().expect("both dispatchers").len(),
        2
    );
    let before = app.get_xdp_dispatcher(key(&first)).expect("first snapshot");
    let other_before = app.get_xdp_dispatcher(key(&other)).expect("other snapshot");
    assert_eq!(before.members()[0].details.netns, select(&alias));
    assert_eq!(
        fs::read_link("/proc/thread-self/ns/net").expect("caller"),
        original
    );

    // Failed publication retains both namespace and program evidence for unload.
    faults.set(Some(Point::XdpReplace));
    let error = app.unload(a).expect_err("publication failure");
    assert!(error.report().expect("retained").attempts().is_empty());
    faults.set(None);
    fs::rename(&alias, &saved).expect("hide selected namespace path");
    let error = app
        .retry_unload(error)
        .expect_err("missing path refuses retry");
    symlink(second_ns.path(), &alias).expect("replace selected namespace");
    let error = app
        .retry_unload(error)
        .expect_err("changed namespace refuses retry");
    assert!(error.report().expect("retained").attempts().is_empty());
    assert_eq!(
        app.get_xdp_dispatcher(key(&first))
            .expect("first unchanged"),
        before
    );
    assert_eq!(
        app.get_xdp_dispatcher(key(&other))
            .expect("other unchanged"),
        other_before
    );
    fs::rename(&alias, &foreign).expect("save foreign alias");
    fs::rename(&saved, &alias).expect("restore original path");
    assert_eq!(
        app.retry_unload(error)
            .expect("unload both namespaces")
            .unresolved(),
        0
    );
    let after = app.get_xdp_dispatcher(key(&first)).expect("survivor");
    assert_eq!(after.members().len(), 1);
    assert_eq!(after.members()[0].member.id, survivor.id);
    assert!(app.get_xdp_dispatcher(key(&other)).is_err());
    assert_eq!(app.unload(b).expect("last member unload").unresolved(), 0);

    // Final detach uses retained kernel/pin identities even if the named path is gone.
    let last = load();
    let link = app
        .attach_xdp(request(last, select(&second_ns.path())))
        .expect("attach");
    second_ns.delete();
    assert_eq!(
        app.detach_xdp(link.id)
            .expect("detach without named path")
            .unresolved(),
        0
    );
    assert_eq!(
        app.unload(last)
            .expect("unload detached program")
            .unresolved(),
        0
    );
    assert_eq!(
        fs::read_link("/proc/thread-self/ns/net").expect("caller"),
        original
    );
    assert!(app.list_xdp_dispatchers().expect("empty").is_empty());
    c.no_artifacts();
}
