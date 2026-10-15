use bpfman_fs::{RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use serde_json::Value;
use std::{
    fs,
    num::NonZeroU32,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::Duration,
};

pub(super) const TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const NAME: &str = "tracepoint_kill_recorder";
pub(super) const SELECTION: &str = "tracepoint:tracepoint_kill_recorder";

pub(super) fn timings_enabled() -> bool {
    std::env::var_os("BPFMAN_KERNEL_TIMINGS").as_deref() == Some(std::ffi::OsStr::new("1"))
}

// Shared allocator for every in-process network fixture. Interface names use
// short hexadecimal identities to stay below Linux's 15-byte name limit.
pub(super) fn fixture_id() -> String {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    assert!(sequence < 0x10000, "fixture identity exhausted");
    format!("{:x}{sequence:04x}", std::process::id())
}

// Only independent forwarding fixtures opt in. Concurrent outer tests use one
// fixture worker each, avoiding multiplied concurrency. Each job constructs and
// tears down its own runtime, namespace, fault state and descriptors in its worker.
#[allow(clippy::panic)]
pub(super) fn forwarding_fixtures<T: Send>(jobs: Vec<T>, run: impl Fn(T) + Sync) {
    let count = match std::env::var("BPFMAN_KERNEL_TEST_THREADS").as_deref() {
        Ok("2" | "4" | "8") => 1,
        Ok("1") | Err(_) => 2,
        other => panic!("invalid kernel concurrency: {other:?}"),
    };
    let mut workers: Vec<Vec<T>> = (0..count).map(|_| Vec::new()).collect();
    for (index, job) in jobs.into_iter().enumerate() {
        workers[index % count].push(job);
    }
    let parent = std::thread::current();
    let name = parent.name().expect("named kernel test");
    std::thread::scope(|scope| {
        let handles: Vec<_> = workers
            .into_iter()
            .map(|jobs| {
                let run = &run;
                std::thread::Builder::new()
                    // Phase telemetry remains attributed to its enclosing test.
                    .name(name.to_owned())
                    .spawn_scoped(scope, move || {
                        let _timing = TestTiming::phase("forwarding.worker");
                        for job in jobs {
                            run(job);
                        }
                    })
                    .expect("forwarding worker")
            })
            .collect();
        // Join every worker before propagating a failure: sibling fixture
        // cleanup must finish before the next test/backend can begin.
        let outcomes: Vec<_> = handles.into_iter().map(|handle| handle.join()).collect();
        if let Some(failure) = outcomes.into_iter().find_map(Result::err) {
            std::panic::resume_unwind(failure);
        }
    });
}

// Direct stderr bypasses libtest's success-output capture. Opt in through the
// Make environment; one guard covers the whole scenario, including teardown.
pub(super) struct TestTiming {
    name: &'static str,
    started: Option<std::time::Instant>,
    phase: bool,
}

impl TestTiming {
    pub(super) fn start(name: &'static str) -> Self {
        Self::new(name, false)
    }

    pub(super) fn phase(name: &'static str) -> Self {
        Self::new(name, true)
    }

    fn new(name: &'static str, phase: bool) -> Self {
        use std::io::Write;

        let timer = Self {
            name,
            started: timings_enabled().then(|| {
                let _ = timing_origin();
                std::time::Instant::now()
            }),
            phase,
        };
        if let Some(started) = timer.started.filter(|_| !phase) {
            let _ = writeln!(
                std::io::stderr().lock(),
                "kernel-test-start\t{}\t{name}\t{:.6}",
                std::process::id(),
                timing_offset(started)
            );
        }
        timer
    }
}

// Monotonic offsets share an origin within one libtest process, including
// overlapping tests. Process IDs distinguish sequential backend invocations.
fn timing_offset(now: std::time::Instant) -> f64 {
    now.duration_since(timing_origin()).as_secs_f64()
}

fn timing_origin() -> std::time::Instant {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    *ORIGIN.get_or_init(std::time::Instant::now)
}

impl Drop for TestTiming {
    fn drop(&mut self) {
        use std::io::Write;

        if let Some(started) = self.started {
            let outcome = if std::thread::panicking() {
                "failed"
            } else {
                "completed"
            };
            if self.phase {
                let thread = std::thread::current();
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "kernel-phase-timing\t{}\t{}\t{:.3}\t{outcome}",
                    thread.name().unwrap_or("unnamed"),
                    self.name,
                    started.elapsed().as_secs_f64()
                );
            } else {
                let ended = std::time::Instant::now();
                let mut stderr = std::io::stderr().lock();
                let _ = writeln!(
                    stderr,
                    "kernel-test-end\t{}\t{}\t{:.6}\t{outcome}",
                    std::process::id(),
                    self.name,
                    timing_offset(ended)
                );
                let _ = writeln!(
                    stderr,
                    "kernel-timing\t{}\t{:.3}\t{outcome}",
                    self.name,
                    ended.duration_since(started).as_secs_f64(),
                );
            }
        }
    }
}

pub(super) fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("repository")
}

pub(super) fn fixture(name: &str) -> PathBuf {
    repository().join("e2e/testdata/bpf").join(name)
}

pub(super) fn rust() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_bpfman"))
}

pub(super) struct Context {
    _temporary: tempfile::TempDir,
    pub(super) layout: RuntimeLayout,
}

impl Context {
    pub(super) fn new() -> Self {
        assert_ne!(
            fs::read_link("/proc/self/ns/mnt").expect("self namespace"),
            fs::read_link("/proc/1/ns/mnt").expect("host namespace"),
            "run via the Make target in a private mount namespace"
        );

        let temporary = tempfile::tempdir().expect("test runtime");
        let layout = RuntimeLayout::try_from(temporary.path().join("runtime")).expect("layout");
        Self {
            _temporary: temporary,
            layout,
        }
    }

    pub(super) fn writer<T>(&self, run: impl FnOnce(&RuntimeWriter<'_>) -> T) -> T {
        RuntimeDirectory::open_or_create(self.layout.clone())
            .expect("runtime")
            .with_writer(
                AcquireOptions {
                    timeout: TIMEOUT,
                    cancelled: None,
                },
                |w| run(&w),
            )
            .expect("writer")
    }

    pub(super) fn command(&self, binary: &Path, args: &[&str]) -> Command {
        let mut command = Command::new("timeout");
        command
            .arg("30s")
            .arg(binary)
            .arg("--runtime-dir")
            .arg(self.layout.root())
            .args(args)
            .env_remove("BPFMAN_STORE")
            .env_remove("BPFMAN_RUNTIME_DIR")
            .env_remove("BPFMAN_LOCK_TIMEOUT");
        command
    }

    pub(super) fn run(&self, binary: &Path, args: &[&str], success: bool) -> Output {
        let output = self.command(binary, args).output().expect("CLI process");

        assert_eq!(
            output.status.code(),
            Some(if success { 0 } else { 1 }),
            "{args:?}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        if !success {
            assert!(output.stdout.is_empty());
        }

        output
    }

    pub(super) fn json(&self, binary: &Path, args: &[&str]) -> Value {
        serde_json::from_slice(&self.run(binary, args, true).stdout).expect("JSON")
    }

    pub(super) fn load_cli(&self, binary: &Path) -> Value {
        self.json(
            binary,
            &[
                "program",
                "load",
                "file",
                fixture("tracepoint_counter.bpf.o").to_str().expect("path"),
                "--programs",
                SELECTION,
                "-a",
                "rust-slice",
                "-m",
                "test=acceptance",
                "-o",
                "json",
            ],
        )["programs"][0]
            .clone()
    }

    pub(super) fn present(&self, id: NonZeroU32) {
        assert!(self.layout.program_pin_path(id).exists());
        assert!(
            self.layout
                .map_directory_path(id)
                .join("tracepoint_stats_map")
                .exists()
        );
        assert!(self.layout.bytecode_path(id).exists());
    }

    pub(super) fn absent(&self, id: NonZeroU32) {
        assert!(!self.layout.program_pin_path(id).exists());
        assert!(!self.layout.map_directory_path(id).exists());
        assert!(
            !self
                .layout
                .bytecode_path(id)
                .parent()
                .expect("bytecode directory")
                .exists()
        );
    }

    pub(super) fn no_artifacts(&self) {
        let root = self.layout.root();

        assert!(
            names(&root.join("fs"))
                .iter()
                .all(|s| !s.starts_with("prog_"))
        );

        for path in [
            "fs/maps",
            "fs/shared",
            "fs/links",
            "fs/xdp",
            "fs/tc-ingress",
            "fs/tc-egress",
            "fs/tcx-ingress",
            "fs/tcx-egress",
            "tc",
            "programs",
            ".staging",
        ] {
            assert!(names(&root.join(path)).is_empty(), "residue in {path}");
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        let mount = self.layout.root().join("fs");

        // Unmount before TempDir cleanup, including during assertion unwinding.
        if fs::metadata(&mount)
            .ok()
            .zip(fs::metadata(self.layout.root()).ok())
            .is_some_and(|(a, b)| a.dev() != b.dev())
        {
            let result = Command::new("umount").arg(&mount).status();

            if !std::thread::panicking() {
                assert!(result.expect("umount").success());
            }
        }
    }
}

pub(super) fn names(path: &Path) -> Vec<String> {
    let entries = match fs::read_dir(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        result => result.expect("read artifact collection"),
    };
    let mut names: Vec<_> = entries
        .map(|e| e.expect("entry").file_name().into_string().expect("name"))
        .collect();
    names.sort();
    names
}

pub(super) fn id(value: &Value) -> NonZeroU32 {
    NonZeroU32::new(
        value["record"]["program_id"]
            .as_u64()
            .expect("program id")
            .try_into()
            .expect("u32"),
    )
    .expect("nonzero")
}

pub(super) fn assert_program_absent(id: NonZeroU32) {
    let error =
        bpfman_kernel::ProgramObservations::program(&bpfman_kernel_aya::Kernel::default(), id)
            .expect_err("owned program references must be released before return");
    assert_eq!(error.kind(), bpfman_kernel::ErrorKind::Missing);
}
