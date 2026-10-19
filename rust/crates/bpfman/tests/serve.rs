//! Process-level daemon socket, startup, signal and reopening contracts.
#![allow(clippy::expect_used)]
use std::{
    fs,
    os::unix::{
        fs::{MetadataExt, symlink},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
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

struct Fixture {
    _temp: tempfile::TempDir,
    runtime: PathBuf,
    socket: PathBuf,
    config: PathBuf,
    cache: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("directory");
        let config = temp.path().join("bpfman.toml");
        fs::write(&config, "[signing]\nverify_enabled = false\n").expect("config");
        Self {
            runtime: temp.path().join("runtime"),
            socket: temp.path().join("sock/bpfman.sock"),
            cache: temp.path().join("cache"),
            config,
            _temp: temp,
        }
    }

    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_bpfman"));
        c.env_remove("BPFMAN_SOCKET_PATH")
            .env_remove("BPFMAN_LOCK_TIMEOUT")
            .env_remove("BPFMAN_RUNTIME_DIR")
            .env_remove("BPFMAN_CONFIG")
            .arg("serve")
            .arg("--runtime-dir")
            .arg(&self.runtime)
            .arg("--image-cache-dir")
            .arg(&self.cache)
            .arg("--config")
            .arg(&self.config)
            .arg("--lock-timeout=100ms")
            .arg("--socket-path")
            .arg(&self.socket);
        c
    }

    fn start(&self) -> Daemon {
        let mut daemon = Daemon(
            self.command()
                .stdout(Stdio::null())
                .spawn()
                .expect("daemon"),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if UnixStream::connect(&self.socket).is_ok() {
                return daemon;
            }
            assert!(
                daemon.0.try_wait().expect("status").is_none(),
                "daemon exited before readiness"
            );
            assert!(Instant::now() < deadline, "socket readiness");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn stop(daemon: &mut Daemon, signal: &str) {
    assert!(
        Command::new("kill")
            .arg(format!("-{signal}"))
            .arg(daemon.0.id().to_string())
            .status()
            .expect("signal")
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = daemon.0.try_wait().expect("status") {
            assert!(status.success(), "graceful shutdown: {status}");
            return;
        }
        assert!(Instant::now() < deadline, "daemon did not join shutdown");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn daemon_signals_cleanup_socket_and_reopen_store() {
    let f = Fixture::new();
    for signal in ["TERM", "INT"] {
        let mut daemon = f.start();
        let mode = fs::metadata(&f.socket).expect("socket metadata").mode() & 0o777;
        assert_eq!(mode, 0o660);
        assert!(f.runtime.join("db/store.db").exists());
        // Prove writer admission as well as lock-free reading from another CLI.
        let layout = bpfman_fs::RuntimeLayout::try_from(f.runtime.clone()).expect("layout");
        bpfman_fs::RuntimeDirectory::open_or_create(layout)
            .expect("runtime")
            .with_writer(
                bpfman_lock::AcquireOptions {
                    timeout: Duration::from_millis(50),
                    cancelled: None,
                },
                |_| (),
            )
            .expect("independent writer admission");
        // The lifetime socket lock must never hold the program writer lock.
        let read = Command::new(env!("CARGO_BIN_EXE_bpfman"))
            .arg("--runtime-dir")
            .arg(&f.runtime)
            .arg("--config")
            .arg(&f.config)
            .args(["program", "list", "--output=json", "--lock-timeout=100ms"])
            .output()
            .expect("independent CLI");
        assert!(
            read.status.success(),
            "{}",
            String::from_utf8_lossy(&read.stderr)
        );
        stop(&mut daemon, signal);
        assert!(!f.socket.exists());
    }
}

#[test]
fn daemon_recovers_stale_socket_but_preserves_live_endpoint_and_files() {
    let f = Fixture::new();
    fs::create_dir_all(f.socket.parent().expect("parent")).expect("socket parent");
    let stale = UnixListener::bind(&f.socket).expect("stale socket");
    drop(stale);
    let mut daemon = f.start();
    let inode = fs::metadata(&f.socket).expect("socket metadata").ino();
    let second = f.command().output().expect("second daemon");
    assert!(!second.status.success());
    assert_eq!(fs::metadata(&f.socket).expect("live socket").ino(), inode);
    assert!(UnixStream::connect(&f.socket).is_ok());
    stop(&mut daemon, "TERM");

    fs::write(&f.socket, b"operator data").expect("existing file");
    let refused = f.command().output().expect("file refusal");
    assert!(!refused.status.success());
    assert_eq!(
        fs::read(&f.socket).expect("preserved file"),
        b"operator data"
    );
}

#[test]
fn daemon_shutdown_preserves_a_replaced_endpoint() {
    let f = Fixture::new();
    let mut daemon = f.start();
    fs::rename(&f.socket, f.socket.with_extension("old")).expect("move owned endpoint");
    let replacement = UnixListener::bind(&f.socket).expect("replacement endpoint");
    let inode = fs::metadata(&f.socket).expect("replacement inode").ino();
    assert!(
        Command::new("kill")
            .arg("-TERM")
            .arg(daemon.0.id().to_string())
            .status()
            .expect("TERM")
            .success()
    );
    assert!(
        !daemon.0.wait().expect("exit").success(),
        "report cleanup refusal"
    );
    assert_eq!(
        fs::metadata(&f.socket).expect("replacement retained").ino(),
        inode
    );
    drop(replacement);
}

#[test]
fn socket_symlinks_and_invalid_configuration_are_refused() {
    let f = Fixture::new();
    fs::create_dir_all(f.socket.parent().expect("parent")).expect("parent");
    let target = f._temp.path().join("target");
    fs::write(&target, b"preserve").expect("target");
    symlink(&target, &f.socket).expect("symlink");
    assert!(
        !f.command()
            .output()
            .expect("refuse symlink")
            .status
            .success()
    );
    assert_eq!(fs::read(&target).expect("preserved"), b"preserve");

    let invalid = Fixture::new();
    fs::write(
        &invalid.config,
        "[signing]\ntrusted_identities = [{certificate_identity = 'unpaired'}]\n",
    )
    .expect("bad config");
    assert!(
        !invalid
            .command()
            .output()
            .expect("bad config rejected")
            .status
            .success()
    );
    assert!(!invalid.runtime.exists());
    assert!(!invalid.socket.exists());
}

#[test]
fn socket_environment_and_scoping_match_the_daemon_command() {
    let f = Fixture::new();
    let help = Command::new(env!("CARGO_BIN_EXE_bpfman"))
        .args(["serve", "--help"])
        .output()
        .expect("help");
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--socket-path"));
    let relative = f
        .command()
        .args(["--socket-path", "relative"])
        .output()
        .expect("relative path");
    assert_eq!(relative.status.code(), Some(2));
    assert!(!f.runtime.exists());
    let mut command = Command::new(env!("CARGO_BIN_EXE_bpfman"));
    let mut daemon = Daemon(
        command
            .env("BPFMAN_SOCKET_PATH", &f.socket)
            .arg("serve")
            .arg("--runtime-dir")
            .arg(&f.runtime)
            .arg("--config")
            .arg(&f.config)
            .arg("--image-cache-dir")
            .arg(&f.cache)
            .stdout(Stdio::null())
            .spawn()
            .expect("env daemon"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !Path::new(&f.socket).exists() {
        assert!(daemon.0.try_wait().expect("status").is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    stop(&mut daemon, "INT");
}

#[test]
fn a_foreign_live_socket_and_symlinked_parent_are_preserved() {
    let f = Fixture::new();
    fs::create_dir_all(f.socket.parent().expect("parent")).expect("parent");
    let foreign = UnixListener::bind(&f.socket).expect("foreign listener");
    let inode = fs::metadata(&f.socket).expect("inode").ino();
    let result = f.command().output().expect("refuse foreign listener");
    assert!(!result.status.success());
    assert_eq!(
        fs::metadata(&f.socket).expect("preserved inode").ino(),
        inode
    );
    assert!(UnixStream::connect(&f.socket).is_ok());
    drop(foreign);

    let mut f = Fixture::new();
    let target = f._temp.path().join("outside");
    fs::create_dir(&target).expect("target directory");
    let alias = f._temp.path().join("alias");
    symlink(&target, &alias).expect("symlinked ancestor");
    f.socket = alias.join("bpfman.sock");
    assert!(
        !f.command()
            .output()
            .expect("refuse parent alias")
            .status
            .success()
    );
    assert_eq!(fs::read_dir(&target).expect("outside").count(), 0);
}
