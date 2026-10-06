//! Same public lifecycle with one stateful kernel and each real store backend.
#![allow(clippy::expect_used, clippy::panic)]

#[allow(dead_code)]
#[path = "kernel/faults.rs"]
mod faults;
#[path = "support/kernel.rs"]
mod kernel;

use bpfman_fs::{RuntimeDirectory, RuntimeLayout};
use bpfman_model::{ProgramSpec, Symbol};
use bpfman_runtime::{
    ActiveStore, Bpfman, Cancellation, PreparedProgram, TracepointAttach, XdpAttach,
};
use bpfman_store::{
    CommitLoad, LinkReader, LinkStore, OpenStore, UnloadStore, XdpReader, XdpStore,
};
use faults::{Faults, Point as StorePoint};
use kernel::{FakeKernel, Phase, Point};
use std::time::Duration;

trait Store:
    OpenStore<Reader: LinkReader + XdpReader>
    + CommitLoad
    + UnloadStore
    + LinkStore
    + XdpStore
    + Copy
    + 'static
{
}
impl<S> Store for S where
    S: OpenStore<Reader: LinkReader + XdpReader>
        + CommitLoad
        + UnloadStore
        + LinkStore
        + XdpStore
        + Copy
        + 'static
{
}

struct Fixture<S: Store> {
    temp: tempfile::TempDir,
    layout: RuntimeLayout,
    kernel: FakeKernel,
    store: Faults<S>,
    app: Bpfman<Faults<S>, FakeKernel>,
}

impl<S: Store> Fixture<S> {
    fn new(backend: S) -> Self {
        let temp = tempfile::tempdir().expect("fixture");
        let layout = RuntimeLayout::try_from(temp.path().join("runtime")).expect("layout");
        let store = Faults::new(backend);
        let active =
            ActiveStore::open(store.clone(), &layout, Duration::from_secs(1)).expect("real store");
        let runtime = RuntimeDirectory::open_existing(layout.clone())
            .expect("runtime")
            .expect("exists");
        let kernel = FakeKernel::new(&runtime);
        let app = Bpfman::new(active, kernel.clone(), Duration::from_secs(1));
        std::fs::write(temp.path().join("source.o"), b"fake object").expect("source");
        Self {
            temp,
            layout,
            kernel,
            store,
            app,
        }
    }

    fn request(&self, xdp: bool) -> PreparedProgram {
        let symbol = Symbol::try_from("entry").expect("symbol");
        let spec = if xdp {
            ProgramSpec::Xdp(symbol)
        } else {
            ProgramSpec::Tracepoint(symbol)
        };
        PreparedProgram::new(
            &self.kernel,
            &self.temp.path().join("source.o"),
            spec,
            Default::default(),
        )
        .expect("prepare")
    }

    fn clean(&self) {
        self.kernel.assert_empty();
        assert!(
            self.app
                .list(&Default::default())
                .expect("real store")
                .is_empty()
        );
        assert!(self.app.list_link_records().expect("real links").is_empty());
        assert!(
            !self.layout.root().join("fs").exists(),
            "fake must not mount bpffs"
        );
    }
}

fn tracepoint(id: std::num::NonZeroU32) -> TracepointAttach {
    TracepointAttach {
        program_id: id,
        target: "syscalls/sys_enter_kill".parse().expect("target"),
        metadata: Default::default(),
    }
}

fn xdp(id: std::num::NonZeroU32) -> XdpAttach {
    XdpAttach {
        program_id: id,
        interface: "fake0".parse().expect("interface"),
        priority: 50,
        proceed_on: Default::default(),
        metadata: Default::default(),
    }
}

fn lifecycle<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let loaded = f.app.load(f.request(false)).expect("load");
    let id = loaded.record.id;
    assert_eq!(f.app.get(id).expect("get").maps.len(), 1);
    let link = f.app.attach_tracepoint(tracepoint(id)).expect("attach");
    assert!(f.app.get_link(link.id).expect("link").pin_present);
    assert_eq!(f.app.get(id).expect("get").links.len(), 1);
    assert_eq!(f.app.detach(link.id).expect("detach").unresolved(), 0);
    assert!(f.app.get(id).expect("detached program").links.is_empty());
    f.app.attach_tracepoint(tracepoint(id)).expect("reattach");
    assert_eq!(f.app.unload(id).expect("attached unload").unresolved(), 0);
    assert!(!f.layout.bytecode_path(id).exists());
    f.clean();

    let id = f.app.load(f.request(true)).expect("XDP load").record.id;
    let link = f.app.attach_xdp(xdp(id)).expect("XDP attach");
    assert!(f.app.get_link(link.id).expect("extension").pin_present);
    assert!(
        f.app.unload(id).is_err(),
        "attached XDP unload remains unsupported"
    );
    let held = f.kernel.hold_outer();
    assert!(held.attached());
    assert_eq!(
        f.app.detach_xdp(link.id).expect("last detach").unresolved(),
        0
    );
    assert!(
        !held.attached(),
        "detach stops attachment despite a retained handle"
    );
    drop(held);
    assert_eq!(f.app.unload(id).expect("unload XDP").unresolved(), 0);
    f.clean();
}

fn load_faults<S: Store>(backend: S) {
    for point in [
        Point::Prepare,
        Point::Load,
        Point::PinProgram,
        Point::Directory,
        Point::PinMap,
    ] {
        for phase in [Phase::Before, Phase::After] {
            if phase == Phase::After && matches!(point, Point::Prepare | Point::Load) {
                continue;
            }
            let f = Fixture::new(backend);
            f.kernel.fail(point, phase);
            let error = f
                .app
                .load(f.request(false))
                .expect_err("load acquisition failure");
            assert_eq!(error.unresolved(), 0, "{point:?} {phase:?}: {error}");
            f.clean();
        }
    }
    let f = Fixture::new(backend);
    f.store.set(Some(StorePoint::Commit));
    f.kernel.fail(Point::RemoveMap, Phase::Before);
    let error = f
        .app
        .load(f.request(false))
        .expect_err("commit with failed map cleanup");
    assert_eq!(error.unresolved(), 2, "map and its blocked directory");
    assert!(!f.kernel.events().contains(&Point::RemoveDirectory));
    f.kernel.clear();
    let error = f.app.retry_load_cleanup(error);
    assert_eq!(error.unresolved(), 0);
    assert_eq!(
        f.store.count(StorePoint::Commit),
        1,
        "cleanup never retries commit"
    );
    f.clean();
}

fn tracepoint_faults<S: Store>(backend: S) {
    for (point, phase) in [
        (Point::PrepareLink, Phase::Before),
        (Point::Attach, Phase::Before),
        (Point::PinLink, Phase::Before),
        (Point::PinLink, Phase::After),
    ] {
        let f = Fixture::new(backend);
        let id = f.app.load(f.request(false)).expect("load").record.id;
        f.kernel.fail(point, phase);
        let error = f
            .app
            .attach_tracepoint(tracepoint(id))
            .expect_err("acquisition failure");
        assert!(
            error.report().is_none_or(|report| report.unresolved() == 0),
            "{point:?} {phase:?}"
        );
        assert!(
            f.app
                .list_link_records()
                .expect("pending intent cleaned")
                .is_empty()
        );
        f.kernel.clear();
        assert_eq!(f.app.unload(id).expect("unload").unresolved(), 0);
        f.clean();
    }

    let f = Fixture::new(backend);
    let id = f.app.load(f.request(false)).expect("load").record.id;
    f.store.set(Some(StorePoint::FinaliseLink));
    f.kernel.fail(Point::RemoveLink, Phase::Before);
    let error = f
        .app
        .attach_tracepoint(tracepoint(id))
        .expect_err("finalise with failed cleanup");
    assert!(error.report().expect("cleanup").unresolved() > 0);
    assert_eq!(f.app.list_link_records().expect("pending").len(), 1);
    f.kernel.clear();
    f.store.set(None);
    let report = f.app.retry_link_cleanup(error).expect("retry");
    assert_eq!(report.unresolved(), 0);
    assert!(report.attempts().len() > 1);
    assert_eq!(f.app.unload(id).expect("unload").unresolved(), 0);
    f.clean();
}

fn xdp_faults<S: Store>(backend: S) {
    for point in [
        Point::PrepareXdp,
        Point::LoadDispatcher,
        Point::Revision,
        Point::PinDispatcher,
        Point::Extension,
        Point::Outer,
    ] {
        for phase in [Phase::Before, Phase::After] {
            if phase == Phase::After && matches!(point, Point::PrepareXdp | Point::LoadDispatcher) {
                continue;
            }
            let f = Fixture::new(backend);
            let id = f.app.load(f.request(true)).expect("load").record.id;
            f.kernel.fail(point, phase);
            let error = f.app.attach_xdp(xdp(id)).expect_err("acquisition failure");
            assert_eq!(error.unresolved(), 0, "{point:?} {phase:?}");
            f.kernel.clear();
            let link = f.app.attach_xdp(xdp(id)).expect("no attachment residue");
            f.app.detach_xdp(link.id).expect("detach");
            assert_eq!(f.app.unload(id).expect("unload").unresolved(), 0);
            f.clean();
        }
    }

    let f = Fixture::new(backend);
    let id = f.app.load(f.request(true)).expect("load").record.id;
    f.store.set(Some(StorePoint::XdpCommit));
    f.kernel.fail(Point::RemoveOuter, Phase::Before);
    let error = f
        .app
        .attach_xdp(xdp(id))
        .expect_err("commit with blocked detach");
    assert_eq!(error.unresolved(), 4);
    assert!(!f.kernel.events().contains(&Point::RemoveExtension));
    f.kernel.clear();
    f.store.set(None);
    assert_eq!(
        f.app
            .retry_xdp_cleanup(error)
            .expect("explicit retry")
            .unresolved(),
        0
    );
    assert_eq!(f.app.unload(id).expect("unload").unresolved(), 0);
    f.clean();
}

fn cancellation_and_foreign_retry<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let token = Cancellation::new();
    f.kernel.cancel_at(Point::PinMap, &token);
    let error = f
        .app
        .load_with_cancellation(f.request(false), &token)
        .expect_err("cancel after acquisition");
    assert_eq!(error.kind(), bpfman_runtime::LoadErrorKind::Cancelled);
    assert_eq!(error.unresolved(), 0);
    f.clean();
    f.kernel.clear();

    let id = f.app.load(f.request(false)).expect("load").record.id;
    let token = Cancellation::new();
    f.kernel.cancel_at(Point::Attach, &token);
    f.kernel.fail(Point::Release, Phase::Before);
    let error = f
        .app
        .attach_tracepoint_with_cancellation(tracepoint(id), &token)
        .expect_err("cancel with live link");
    assert!(error.report().expect("report").unresolved() > 0);
    let runtime = RuntimeDirectory::open_existing(f.layout.clone())
        .expect("runtime")
        .expect("exists");
    // Both applications need the same store type for receipt compatibility.
    let foreign = Bpfman::new(
        ActiveStore::open(Faults::new(backend), &f.layout, Duration::from_secs(1)).expect("store"),
        FakeKernel::new(&runtime),
        Duration::from_secs(1),
    );
    let error = foreign
        .retry_link_cleanup(error)
        .expect_err("same runtime, foreign kernel");
    f.kernel.clear();
    assert_eq!(
        f.app
            .retry_link_cleanup(error)
            .expect("original kernel retry")
            .unresolved(),
        0
    );
    assert_eq!(f.app.unload(id).expect("unload").unresolved(), 0);
    f.clean();
}

fn batch_and_foreign_load_retry<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let prepare = || {
        f.request(false)
            .with_additional_programs(
                &f.kernel,
                vec![ProgramSpec::Tracepoint(
                    "second".try_into().expect("symbol"),
                )],
            )
            .expect("batch")
    };
    let loaded = f.app.load_batch(prepare()).expect("atomic batch");
    assert_eq!(loaded.len(), 2);
    assert_ne!(loaded[0].maps[0].kernel.id, loaded[1].maps[0].kernel.id);
    assert_eq!(f.store.count(StorePoint::Commit), 1);
    for program in loaded {
        assert_eq!(
            f.app
                .unload(program.record.id)
                .expect("unload")
                .unresolved(),
            0
        );
    }
    f.clean();

    f.store.set(Some(StorePoint::Commit));
    f.kernel.fail(Point::RemoveMap, Phase::Before);
    let error = f.app.load_batch(prepare()).expect_err("batch rollback");
    assert_eq!(
        error.unresolved(),
        4,
        "two maps and two dependent directories"
    );
    let runtime = RuntimeDirectory::open_existing(f.layout.clone())
        .expect("runtime")
        .expect("exists");
    let foreign = Bpfman::new(
        ActiveStore::open(Faults::new(backend), &f.layout, Duration::from_secs(1)).expect("store"),
        FakeKernel::new(&runtime),
        Duration::from_secs(1),
    );
    let error = foreign.retry_load_cleanup(error);
    assert_eq!(
        error.unresolved(),
        4,
        "foreign kernel cannot consume receipts"
    );
    f.kernel.clear();
    let error = f.app.retry_load_cleanup(error);
    assert_eq!(error.unresolved(), 0);
    assert_eq!(
        f.store.count(StorePoint::Commit),
        2,
        "no commit during retries"
    );
    f.clean();
}

fn partial_outer_retry<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    let id = f.app.load(f.request(true)).expect("load").record.id;
    f.kernel.fail(Point::Outer, Phase::After);
    f.kernel.fail(Point::RemoveOuter, Phase::Before);
    let error = f.app.attach_xdp(xdp(id)).expect_err("live partial outer");
    assert_eq!(error.unresolved(), 4);
    let held = f.kernel.hold_outer();
    assert!(held.attached());
    let runtime = RuntimeDirectory::open_existing(f.layout.clone())
        .expect("runtime")
        .expect("exists");
    let foreign = Bpfman::new(
        ActiveStore::open(Faults::new(backend), &f.layout, Duration::from_secs(1)).expect("store"),
        FakeKernel::new(&runtime),
        Duration::from_secs(1),
    );
    let error = foreign
        .retry_xdp_cleanup(error)
        .expect_err("foreign kernel");
    assert_eq!(error.unresolved(), 4);
    assert!(held.attached());
    f.kernel.clear();
    let report = f
        .app
        .retry_xdp_cleanup(error)
        .expect("retry live ownership");
    assert_eq!(report.unresolved(), 0);
    assert!(report.attempts().len() > 1);
    assert!(!held.attached());
    drop(held);
    assert_eq!(f.app.unload(id).expect("unload").unresolved(), 0);
    f.clean();
}

fn post_commit_and_teardown<S: Store>(backend: S) {
    let f = Fixture::new(backend);
    f.kernel.fail(Point::ObserveProgram, Phase::Before);
    let error = f
        .app
        .load(f.request(false))
        .expect_err("post-commit observation");
    assert_eq!(error.unresolved(), 0);
    let id = f.app.list(&Default::default()).expect("committed")[0].id();
    assert!(!f.kernel.events().contains(&Point::RemoveProgram));
    f.kernel.clear();
    let token = Cancellation::new();
    f.kernel.cancel_at(Point::RemoveProgram, &token);
    f.kernel.fail(Point::RemoveMap, Phase::Before);
    let report = f
        .app
        .unload_with_cancellation(id, &token)
        .expect("teardown completes with GC warning");
    assert_eq!(report.unresolved(), 3, "map, directory, map-set record");
    f.kernel.clear();
    let report = f.app.retry_unload_cleanup(report).expect("retry");
    assert_eq!(report.unresolved(), 0);
    f.clean();
}

macro_rules! backend_tests {
    ($module:ident,$backend:expr) => {
        mod $module {
            #[test]
            fn lifecycle() {
                super::lifecycle($backend);
            }
            #[test]
            fn load_faults() {
                super::load_faults($backend);
            }
            #[test]
            fn tracepoint_faults() {
                super::tracepoint_faults($backend);
            }
            #[test]
            fn xdp_faults() {
                super::xdp_faults($backend);
            }
            #[test]
            fn cancellation_and_foreign_retry() {
                super::cancellation_and_foreign_retry($backend);
            }
            #[test]
            fn batch_and_foreign_load_retry() {
                super::batch_and_foreign_load_retry($backend);
            }

            #[test]
            fn partial_outer_retry() {
                super::partial_outer_retry($backend);
            }

            #[test]
            fn post_commit_and_teardown() {
                super::post_commit_and_teardown($backend);
            }
        }
    };
}
backend_tests!(sqlite, bpfman_store_sqlite::Backend);
backend_tests!(json, bpfman_store_json::Backend);
