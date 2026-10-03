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
    store: &'static str,
}

impl Context {
    pub(super) fn new() -> Self {
        Self::with_store("sqlite")
    }

    pub(super) fn with_store(store: &'static str) -> Self {
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
            store,
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
            .env("BPFMAN_STORE", self.store)
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
        for path in ["fs/maps", "programs", ".staging"] {
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
