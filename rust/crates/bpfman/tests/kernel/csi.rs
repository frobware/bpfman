//! The same stock CSI client runs against both production executables.
use super::support::*;
use std::{
    fs,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(super) fn wire(implementation: &str) {
    let c = Context::new();
    let parent = c.layout.root().parent().expect("parent");
    let socket = parent.join("bpfman.sock");
    let config = parent.join("bpfman.toml");
    fs::write(&config, "[signing]\nverify_enabled = false\n").expect("config");
    let binary = if implementation == "rust" {
        rust()
    } else {
        repository().join("bin/bpfman")
    };
    let label = format!("csi-{}-{}", implementation, fixture_id());
    let backing = if implementation == "rust" {
        c.layout.csi_filesystem_path()
    } else {
        PathBuf::from("/run/bpfman/csi/fs")
    };
    let start = || {
        let mut daemon = Daemon(
            Command::new(&binary)
                // Operator startup form is the actual production entrypoint.
                .arg("--csi-support")
                .arg("--runtime-dir")
                .arg(c.layout.root())
                .arg("--socket-path")
                .arg(&socket)
                .arg("--config")
                .arg(&config)
                .arg("--image-cache-dir")
                .arg(parent.join("cache"))
                .env_remove("BPFMAN_RUNTIME_DIR")
                .env_remove("BPFMAN_SOCKET_PATH")
                .env_remove("BPFMAN_CONFIG")
                .env_remove("BPFMAN_STORE")
                .stdout(Stdio::null())
                .spawn()
                .expect("daemon"),
        );
        let deadline = Instant::now() + Duration::from_secs(15);
        while !socket.exists() || !c.layout.csi_socket_path().exists() {
            assert!(
                daemon.0.try_wait().expect("status").is_none(),
                "{implementation} daemon exited"
            );
            assert!(Instant::now() < deadline, "CSI readiness");
            std::thread::sleep(Duration::from_millis(5));
        }
        daemon
    };
    let client = |lane: &str| {
        let result = Command::new("timeout")
            .arg("60s")
            .arg(repository().join("bin/csi-contract"))
            .arg("-socket")
            .arg(&socket)
            .arg("-csi-socket")
            .arg(c.layout.csi_socket_path())
            .arg("-runtime")
            .arg(c.layout.root())
            .arg("-backing")
            .arg(&backing)
            .arg("-objects")
            .arg(fixture("."))
            .arg("-label")
            .arg(&label)
            .arg("-lane")
            .arg(lane)
            .output()
            .expect("CSI client");
        assert!(
            result.status.success(),
            "{implementation}/{lane}: {}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    };
    let stop = |daemon: &mut Daemon| {
        assert!(
            Command::new("kill")
                .args(["-TERM", &daemon.0.id().to_string()])
                .status()
                .expect("signal")
                .success()
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = daemon.0.try_wait().expect("exit") {
                assert!(
                    status.success(),
                    "{implementation} daemon shutdown: {status}"
                );
                break;
            }
            assert!(Instant::now() < deadline, "daemon shutdown");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    let mut daemon = start();
    client("exercise");
    client("persist-create");
    stop(&mut daemon);
    if implementation == "rust" {
        assert!(!c.layout.csi_socket_path().exists());
    }
    let mut daemon = start();
    client("persist-read");
    stop(&mut daemon);
    c.no_artifacts();
    assert!(
        c.json(&rust(), &["program", "list", "-o", "json"])["programs"]
            .as_array()
            .expect("list")
            .is_empty()
    );
}

pub(super) fn rollback<S>(backend: S)
where
    S: bpfman_store::OpenStore
        + bpfman_store::CommitLoad
        + bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpReplacementStore
        + bpfman_store::TcStore,
    S::Reader: bpfman_store::LinkReader,
{
    use bpfman_fs::{MapPinning, MapReopening, PinSource, PinTarget, VolumeRequest};
    use bpfman_model::{ProgramSpec, Symbol};
    use bpfman_runtime::{ActiveStore, Bpfman, Cancellation, PreparedProgram};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    struct Fault {
        cancel: Arc<AtomicBool>,
        mode: u8,
        busy: Arc<Mutex<Option<PathBuf>>>,
    }
    struct Map {
        inner: bpfman_kernel_aya::VolumeMap,
        cancel: Arc<AtomicBool>,
        mode: u8,
        busy: Arc<Mutex<Option<PathBuf>>>,
    }
    fn injected() -> Box<dyn std::error::Error + Send + Sync> {
        std::io::Error::other("injected CSI map failure").into()
    }
    impl MapReopening for Fault {
        type Map = Map;
        fn open_map(&self, source: PinSource<'_>) -> bpfman_fs::KernelResult<Map> {
            if self.mode == 0 {
                return Err(injected());
            }
            let inner = bpfman_kernel_aya::Kernel::default().open_map(source)?;
            if self.mode == 2 {
                self.cancel.store(true, Ordering::Release);
            }
            Ok(Map {
                inner,
                cancel: self.cancel.clone(),
                mode: self.mode,
                busy: self.busy.clone(),
            })
        }
    }
    impl MapPinning for Map {
        fn pin(&self, target: PinTarget<'_>) -> bpfman_fs::KernelResult<()> {
            if self.mode == 1 {
                return Err(injected());
            }
            let busy_path = target.path().parent().expect("map parent").join("busy");
            self.inner.pin(target)?;
            if self.mode == 3 {
                self.cancel.store(true, Ordering::Release);
            }
            if self.mode == 4 {
                let busy = fs::canonicalize(busy_path.parent().expect("backing"))
                    .expect("backing path")
                    .join("busy");
                fs::create_dir(&busy).expect("busy directory");
                assert!(
                    Command::new("mount")
                        .args(["-t", "tmpfs", "tmpfs"])
                        .arg(&busy)
                        .status()
                        .expect("busy mount")
                        .success()
                );
                *self.busy.lock().expect("busy") =
                    Some(fs::canonicalize(busy).expect("injected mount path"));
                return Err(injected());
            }
            Ok(())
        }
    }
    let c = Context::new();
    let store = ActiveStore::open(backend, &c.layout, TIMEOUT).expect("store");
    let app = Bpfman::new(store, bpfman_kernel_aya::Kernel::default(), TIMEOUT);
    let program = PreparedProgram::new(
        &bpfman_kernel_aya::Kernel::default(),
        &fixture("tracepoint_counter.bpf.o"),
        ProgramSpec::Tracepoint(Symbol::try_from(NAME).expect("symbol")),
        Default::default(),
    )
    .expect("prepare");
    let id = app.load(program).expect("load").record.id;
    let descriptors = || fs::read_dir("/proc/self/fd").expect("descriptors").count();
    let baseline = descriptors();
    for mode in 0..5 {
        let target = c.layout.root().join(format!("target-{mode}"));
        fs::create_dir(&target).expect("CO-owned target");
        let req = VolumeRequest::new(
            &format!("rollback-{mode}"),
            &target,
            "tracepoint_stats_map",
            true,
            Some(12345),
        )
        .expect("request");
        let fault = Fault {
            cancel: Arc::new(AtomicBool::new(false)),
            mode,
            busy: Arc::new(Mutex::new(None)),
        };
        let mut failed = c
            .writer(|w| w.publish_volume(&req, id, &fault, &fault.cancel))
            .expect_err("injected failure/cancellation");
        assert!(target.is_dir(), "CO target survives rollback");
        if mode == 4 {
            assert!(
                failed.pending.is_some(),
                "blocked cleanup retains ownership"
            );
            assert_eq!(failed.cleanup.len(), 1, "one cleanup attempt");
            let busy = fault.busy.lock().expect("busy").take().expect("busy mount");
            assert!(
                Command::new("umount")
                    .arg(&busy)
                    .status()
                    .expect("unmount injected child")
                    .success()
            );
            assert!(
                Command::new("rmdir")
                    .arg(&busy)
                    .status()
                    .expect("remove injected child")
                    .success()
            );
            let pending = failed.pending.take().expect("retained receipt");
            let (pending, errors) = c.writer(|w| w.cleanup_volume(pending));
            assert!(pending.is_none());
            assert!(errors.is_empty(), "{errors:?}");
        } else {
            assert!(failed.pending.is_none(), "{failed:?}");
            assert!(failed.cleanup.is_empty(), "{failed:?}");
        }
        if matches!(mode, 2 | 3) {
            assert!(fault.cancel.load(Ordering::Acquire));
        }
        assert!(!c.layout.csi_filesystem_path().join(req.volume()).exists());
        drop(failed);
        assert_eq!(descriptors(), baseline, "CSI operation leaked descriptors");
    }
    let cancelled = Cancellation::new();
    cancelled.cancel();
    let req = VolumeRequest::new(
        "pre-cancelled",
        &c.layout.root().join("cancelled"),
        "tracepoint_stats_map",
        false,
        None,
    )
    .expect("request");
    let failure = c
        .writer(|w| {
            w.publish_volume(
                &req,
                id,
                &bpfman_kernel_aya::Kernel::default(),
                cancelled.flag(),
            )
        })
        .expect_err("cancelled admission");
    assert!(failure.pending.is_none());
    assert!(!c.layout.csi_filesystem_path().join(req.volume()).exists());
    assert_eq!(app.unload(id).expect("unload").unresolved(), 0);
    c.no_artifacts();
}

pub(super) fn admission<S>(backend: S)
where
    S: bpfman_store::OpenStore
        + bpfman_store::CommitLoad
        + bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpReplacementStore
        + bpfman_store::TcStore
        + Send
        + Sync
        + 'static,
    S::Reader: bpfman_store::LinkReader,
{
    use bpfman_fs::VolumeRequest;
    use bpfman_model::{ProgramSpec, Symbol};
    use bpfman_runtime::{
        ActiveStore, Bpfman, Cancellation, MapVolumes, PreparedProgram, VolumeErrorKind,
    };
    use std::sync::Arc;
    let c = Context::new();
    let active = ActiveStore::open(backend, &c.layout, TIMEOUT).expect("store");
    let app = Arc::new(Bpfman::new(
        active,
        bpfman_kernel_aya::Kernel::default(),
        Duration::ZERO,
    ));
    let load = || {
        PreparedProgram::new(
            &bpfman_kernel_aya::Kernel::default(),
            &fixture("tracepoint_counter.bpf.o"),
            ProgramSpec::Tracepoint(Symbol::try_from(NAME).expect("symbol")),
            [("bpfman.io/ProgramName".into(), "volume-program".into())].into(),
        )
        .expect("prepare")
    };
    let first = app.load(load()).expect("first load").record.id;
    let second = app.load(load()).expect("second load").record.id;
    let target = c.layout.root().join("target");
    let request = VolumeRequest::new("admission", &target, "tracepoint_stats_map", false, None)
        .expect("request");
    let volumes = MapVolumes::new(app.clone());
    let token = Cancellation::new();
    let lock_count = || {
        fs::read_dir("/proc/self/fd")
            .expect("fd directory")
            .filter_map(Result::ok)
            .filter(|e| {
                fs::read_link(e.path())
                    .ok()
                    .is_some_and(|p| p == c.layout.lock_path())
            })
            .count()
    };
    c.writer(|_| {
        let owned_lock_fds = lock_count();
        std::thread::scope(|scope| {
            let work = scope.spawn(|| volumes.publish("volume-program", &request, &token));
            let deadline = Instant::now() + Duration::from_secs(5);
            // Observe admission, not delayed resource reclamation.
            while lock_count() < owned_lock_fds + 1 {
                assert!(Instant::now() < deadline, "writer admission");
                std::thread::sleep(Duration::from_millis(2));
            }
            assert_eq!(
                app.list(&Default::default()).expect("lock-free read").len(),
                2
            );
            assert_eq!(
                volumes
                    .publish("volume-program", &request, &Cancellation::new())
                    .expect_err("same-volume concurrency")
                    .kind(),
                VolumeErrorKind::Busy
            );
            token.cancel();
            assert_eq!(
                work.join()
                    .expect("writer worker")
                    .expect_err("cancelled writer")
                    .kind(),
                VolumeErrorKind::Cancelled
            );
            assert_eq!(
                lock_count(),
                owned_lock_fds,
                "waiting writer FD closes before lock release"
            );
        });
    });
    assert!(!target.exists(), "cancelled admission has no late effects");
    volumes
        .publish("volume-program", &request, &Cancellation::new())
        .expect("publish");
    // Ambiguous metadata follows Go's lowest-live-program-ID rule.
    let published = aya::maps::MapInfo::from_pin(target.join("tracepoint_stats_map"))
        .expect("published map")
        .id();
    let original = aya::maps::MapInfo::from_pin(
        c.layout
            .map_directory_path(first)
            .join("tracepoint_stats_map"),
    )
    .expect("first map")
    .id();
    assert_eq!(published, original);
    // An independent child mount blocks target teardown but must not block the
    // backing mount's one cleanup attempt. No lazy unmount or automatic retry.
    let busy = target.join("busy");
    fs::create_dir(&busy).expect("child directory");
    assert!(
        Command::new("mount")
            .args(["-t", "tmpfs", "tmpfs"])
            .arg(&busy)
            .status()
            .expect("child mount")
            .success()
    );
    let failure = volumes
        .unpublish(request.volume(), &target, &Cancellation::new())
        .expect_err("blocked target");
    assert_eq!(failure.kind(), VolumeErrorKind::Internal);
    assert!(target.is_dir());
    assert!(
        !c.layout
            .csi_filesystem_path()
            .join(request.volume())
            .exists(),
        "independent backing cleanup completed"
    );
    assert_eq!(
        volumes
            .publish("volume-program", &request, &Cancellation::new())
            .expect_err("pending ownership blocks forward work")
            .kind(),
        VolumeErrorKind::FailedPrecondition
    );

    assert_eq!(
        volumes
            .unpublish(
                request.volume(),
                &c.layout.root().join("different-target"),
                &Cancellation::new()
            )
            .expect_err("pending teardown cannot be redirected")
            .kind(),
        VolumeErrorKind::FailedPrecondition
    );
    assert!(target.is_dir());
    let cancelled = Cancellation::new();
    cancelled.cancel();
    assert_eq!(
        volumes
            .retry_cleanup(&cancelled)
            .expect_err("cancelled recovery admission")
            .kind(),
        VolumeErrorKind::Cancelled
    );
    assert!(
        Command::new("umount")
            .arg(&busy)
            .status()
            .expect("remove child mount")
            .success()
    );
    assert!(
        Command::new("rmdir")
            .arg(&busy)
            .status()
            .expect("remove child directory")
            .success()
    );
    assert_eq!(
        volumes
            .retry_cleanup(&Cancellation::new())
            .expect("explicit retry"),
        0
    );
    assert!(!target.exists());
    assert!(volumes.take_recovery().is_empty());
    for id in [first, second] {
        assert_eq!(app.unload(id).expect("unload").unresolved(), 0);
    }
    c.no_artifacts();
}
