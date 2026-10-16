//! Native LSM hook binding, execution, recovery and managed-reference lifetime.
use super::{
    faults::{Faults, Point},
    support::*,
};
use bpfman_kernel::{LinkObservations, ProgramObservations, TracingLinks};
use bpfman_model::{LinkState, ProgramSpec, TracingKind, TracingLink};
use bpfman_runtime::{ActiveStore, Bpfman, Cancellation, PreparedProgram, TracingAttach};
use bpfman_store::*;
use std::{io::Read, num::NonZeroU32};

fn request(id: NonZeroU32) -> TracingAttach {
    TracingAttach {
        program_id: id,
        kind: TracingKind::Lsm,
        metadata: [("owner".into(), "lsm-contract".into())].into(),
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
    let active = std::fs::read_to_string("/sys/kernel/security/lsm").expect("active LSMs");
    assert!(
        active.trim().split(',').any(|lsm| lsm == "bpf"),
        "BPF LSM must be enabled"
    );
    let c = Context::new();
    let store = Faults::new(backend);
    let app = Bpfman::new(
        ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("store"),
        bpfman_kernel_aya::Kernel::default(),
        TIMEOUT,
    );
    // Use the current test thread's comm. Other processes and kernel workers
    // cannot perturb this fixture's counter; no global process renaming is needed.
    let comm = std::fs::read("/proc/thread-self/comm").expect("thread comm");
    assert!(comm.len() > 8);
    let prepared = |hook: &str| {
        PreparedProgram::new(
            &bpfman_kernel_aya::Kernel::default(),
            &fixture("lsm_exact.bpf.o"),
            ProgramSpec::Lsm {
                name: "test_lsm".try_into().expect("name"),
                hook: hook.try_into().expect("hook"),
            },
            Default::default(),
        )
        .expect("prepare")
        .with_globals(
            &bpfman_kernel_aya::Kernel::default(),
            [("target_comm".into(), comm[..8].to_vec())].into(),
        )
        .expect("globals")
    };
    let load = |hook: &str| app.load(prepared(hook)).expect("load").record.id;
    let counter = |id| {
        let map = aya::maps::MapData::from_pin(c.layout.map_directory_path(id).join("lsm_count"))
            .expect("counter map");
        aya::maps::Array::<_, u64>::try_from(aya::maps::Map::Array(map)).expect("array")
    };
    let error = app
        .load(prepared("bpfman_missing_lsm_hook"))
        .expect_err("missing hook");
    assert_eq!(error.unresolved(), 0);
    assert!(app.list_link_records().expect("no intent").is_empty());
    c.no_artifacts();

    store.set(Some(Point::Commit));
    let error = app
        .load(prepared("file_open"))
        .expect_err("load publication failure");
    assert_eq!(error.unresolved(), 0);
    store.set(None);
    c.no_artifacts();

    // The fixture's ELF says file_open. Binding file_permission proves that the
    // requested hook overrides it: open alone does not count, read counts once.
    let id = load("file_permission");
    let link = app.attach_tracing(request(id)).expect("override attach");
    let counts = counter(id);
    let before = counts.get(&0, 0).expect("count");
    let mut target = std::fs::File::open("/dev/null").expect("open");
    assert_eq!(
        counts.get(&0, 0).expect("count"),
        before,
        "ELF hook overridden"
    );
    assert_eq!(
        target
            .read(&mut [0])
            .expect("read triggers permission hook"),
        0
    );
    assert_eq!(counts.get(&0, 0).expect("traffic"), before + 1);
    assert_eq!(app.detach(link.id).expect("detach").unresolved(), 0);
    // Detach itself reads stored intent while the hook is still active. Take
    // the quiescence baseline after teardown, then fire only the target read.
    let detached = counts.get(&0, 0).expect("detached baseline");
    assert_eq!(target.read(&mut [0]).expect("detached read"), 0);
    assert_eq!(counts.get(&0, 0).expect("quiescent"), detached);
    drop(counts);
    drop(target);
    assert_eq!(app.unload(id).expect("unload override").unresolved(), 0);
    absent(id);
    c.no_artifacts();

    let id = load("file_open");
    assert_eq!(app.get(id).expect("kernel type").kernel.kind, "lsm");
    c.writer(|writer| {
        let wrong = TracingLink {
            function: "file_permission".try_into().expect("hook"),
            kind: TracingKind::Lsm,
        };
        assert!(
            bpfman_kernel_aya::Kernel::default()
                .prepare_tracing(writer, id, &wrong)
                .is_err()
        );
        let wrong_type = TracingLink {
            function: "do_unlinkat".try_into().expect("function"),
            kind: TracingKind::Fentry,
        };
        assert!(
            bpfman_kernel_aya::Kernel::default()
                .prepare_tracing(writer, id, &wrong_type)
                .is_err()
        );
    });
    let mut wrong_family = request(id);
    wrong_family.kind = TracingKind::Fentry;
    assert!(
        app.attach_tracing(wrong_family)
            .expect_err("wrong family")
            .report()
            .is_none()
    );
    assert!(
        app.list_link_records()
            .expect("no wrong-family intent")
            .is_empty()
    );

    store.set(Some(Point::FinaliseLink));
    let error = app
        .attach_tracing(request(id))
        .expect_err("publication failure");
    assert_eq!(error.report().expect("compensation").unresolved(), 0);
    store.set(None);
    assert!(app.list_link_records().expect("compensated").is_empty());

    let token = Cancellation::new();
    store.cancel_at(Point::CreateLink, &token);
    let error = app
        .attach_tracing_with_cancellation(request(id), &token)
        .expect_err("cancelled intent");
    assert_eq!(error.report().expect("compensation").unresolved(), 0);
    assert!(
        app.list_link_records()
            .expect("cancelled intent removed")
            .is_empty()
    );

    let first = app.attach_tracing(request(id)).expect("attach");
    let observed = app.get_link(first.id).expect("observation");
    assert!(observed.pin_present && observed.kernel.is_some());
    assert_eq!(observed.record.metadata["owner"], "lsm-contract");
    let duplicate = app
        .attach_tracing(request(id))
        .expect_err("duplicate native link");
    assert_eq!(duplicate.report().expect("compensation").unresolved(), 0);
    assert_eq!(app.list_link_records().expect("original retained").len(), 1);
    let other_id = load("file_open");
    let second = app
        .attach_tracing(request(other_id))
        .expect("independent program");
    let counts = counter(id);
    let before = counts.get(&0, 0).expect("count");
    for _ in 0..5 {
        drop(std::fs::File::open("/dev/null").expect("fire hook"));
    }
    assert_eq!(counts.get(&0, 0).expect("traffic"), before + 5);
    drop(counts);

    let saved = c.layout.root().join("fs/saved-lsm-link");
    let swap = || {
        std::fs::rename(&first.pin_path, &saved).expect("save");
        std::fs::rename(&second.pin_path, &first.pin_path).expect("replace");
        std::fs::rename(&saved, &second.pin_path).expect("swap");
    };
    swap();
    assert!(app.unload(id).expect_err("foreign pin").report().is_none());
    assert!(std::path::Path::new(&first.pin_path).exists());
    assert!(std::path::Path::new(&second.pin_path).exists());
    swap();

    store.set(Some(Point::DeleteLink));
    let error = app
        .unload(id)
        .expect_err("record deletion blocks program teardown");
    assert!(error.report().is_some());
    assert!(bpfman_kernel_aya::Kernel::default().program(id).is_ok());
    assert!(!std::path::Path::new(&first.pin_path).exists());
    let LinkState::Attached { kernel_id } = first.state else {
        unreachable!()
    };
    assert_eq!(
        bpfman_kernel_aya::Kernel::default()
            .extension_link(kernel_id)
            .expect_err("released link")
            .kind(),
        bpfman_kernel::ErrorKind::Missing
    );
    assert!(std::path::Path::new(&second.pin_path).exists());
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
    assert_eq!(app.unload(other_id).expect("other unload").unresolved(), 0);
    absent(other_id);
    c.no_artifacts();

    let id = load("file_open");
    let holder = aya::programs::ProgramInfo::from_pin(c.layout.program_pin_path(id))
        .expect("program")
        .fd()
        .expect("holder");
    app.attach_tracing(request(id)).expect("held attach");
    assert_eq!(app.unload(id).expect("held unload").unresolved(), 0);
    assert!(bpfman_kernel_aya::Kernel::default().program(id).is_ok());
    drop(holder);
    absent(id);
    c.no_artifacts();
}
