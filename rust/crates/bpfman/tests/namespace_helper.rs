//! Exercise the production launcher with real processes, sockets and inherited locks.
#![allow(clippy::expect_used)]
#[allow(dead_code)]
#[path = "../src/ns_helper.rs"]
mod ns_helper;

use bpfman_lock::{AcquireOptions, with_write_lock};
use bpfman_runtime::Cancellation;
use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags};
use std::{
    fs::{self, File},
    io::{self, IoSlice, Read},
    mem::MaybeUninit,
    os::{
        fd::{AsFd, FromRawFd},
        unix::{fs::PermissionsExt, net::UnixDatagram},
    },
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

// A separate invocation of this integration binary acts as a deliberately
// faulty helper. Real successful setns is covered by the unchanged DSL corpus.
#[test]
#[allow(unsafe_code)]
fn helper_fixture() {
    let Ok(mode) = std::env::var("BPFMAN_NS_TEST_MODE") else {
        return;
    };
    let directory = std::env::var("BPFMAN_NS_TEST_DIR").expect("fixture dir");
    // SAFETY: the fixture is the dedicated receiver of the writer-lock slot.
    let inherited_lock = unsafe { std::os::fd::OwnedFd::from_raw_fd(4) };
    let _lock =
        bpfman_lock::InheritedWriteLock::from_fd(inherited_lock).expect("inherited writer lock");
    fs::write(
        Path::new(&directory).join("pid"),
        std::process::id().to_string(),
    )
    .expect("pid marker");
    if matches!(mode.as_str(), "wait" | "request-wait") {
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    if mode == "fail" {
        std::process::exit(17);
    }
    if mode == "empty" {
        return;
    }
    // SAFETY: this dedicated fixture process inherits the launcher's socket in
    // slot 3 and adopts it once; no other code owns the slot.
    let socket = unsafe { UnixDatagram::from_raw_fd(3) };
    if mode == "nofd" {
        socket.send(b"U").expect("message without descriptor");
        return;
    }
    let target = if mode == "nonregular" {
        "/dev/null"
    } else {
        "/proc/self/exe"
    };
    let file = File::open(target).expect("target");
    let fds = [file.as_fd(), file.as_fd(), file.as_fd()];
    let count = match mode.as_str() {
        "many" => 3,
        "two" => 2,
        _ => 1,
    };
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(3))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    assert!(ancillary.push(SendAncillaryMessage::ScmRights(&fds[..count])));
    rustix::net::sendmsg(
        &socket,
        &[IoSlice::new(if mode == "badbyte" { b"X" } else { b"U" })],
        &mut ancillary,
        SendFlags::NOSIGNAL,
    )
    .expect("send descriptor");
    if mode == "sendfail" {
        std::process::exit(18);
    }
}

fn fd_count() -> usize {
    fs::read_dir("/proc/self/fd").expect("fd inventory").count()
}

fn quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "'\\''"))
}

#[test]
fn helper_handoff_failure_cancellation_and_reaping() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let layout = bpfman_fs::RuntimeLayout::try_from(temp.path().join("runtime"))?;
    let runtime = bpfman_fs::RuntimeDirectory::open_or_create(layout.clone())?;
    let lock_path = layout.lock_path();
    let target = bpfman_model::UprobeLink::new("/absolute/target", None, 1, 0, false)?
        .with_container_pid(std::process::id())?;
    let executable = temp.path().join("helper");
    for mode in [
        "success",
        "fail",
        "empty",
        "nofd",
        "spawnfail",
        "badbyte",
        "two",
        "many",
        "nonregular",
        "sendfail",
        "wait",
        "request-wait",
        "precancel",
    ] {
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nBPFMAN_NS_TEST_MODE='{mode}' BPFMAN_NS_TEST_DIR={} exec {} --exact helper_fixture --nocapture\n",
                quote(temp.path()),
                quote(&std::env::current_exe()?)
            ),
        )?;
        fs::set_permissions(
            &executable,
            fs::Permissions::from_mode(if mode == "spawnfail" { 0o600 } else { 0o700 }),
        )?;
        let cancellation = Cancellation::new();
        if mode == "precancel" {
            cancellation.cancel();
        }
        let launcher = ns_helper::Launcher::new(executable.clone(), cancellation.clone());
        let request_cancel = Cancellation::new();
        let before = fd_count();
        let result = runtime.with_writer(
            AcquireOptions {
                timeout: Duration::from_secs(1),
                cancelled: None,
            },
            |writer| {
                std::thread::scope(|scope| {
                    let worker = if matches!(mode, "wait" | "request-wait") {
                        let cancellation = if mode == "request-wait" {
                            request_cancel.clone()
                        } else {
                            cancellation.clone()
                        };
                        let marker = temp.path().join("pid");
                        // Remove the previous marker before launching this child.
                        fs::write(&marker, "").expect("clear marker");
                        Some(scope.spawn(move || {
                            let started = Instant::now();
                            while fs::read_to_string(&marker).expect("marker").is_empty() {
                                assert!(
                                    started.elapsed() < Duration::from_secs(5),
                                    "helper started"
                                );
                                std::thread::sleep(Duration::from_millis(5));
                            }
                            cancellation.cancel();
                        }))
                    } else {
                        None
                    };
                    let result = if mode == "request-wait" {
                        launcher.open_with_cancellation(&writer, &target, request_cancel.flag())
                    } else {
                        launcher.open(&writer, &target)
                    };
                    if let Some(worker) = worker {
                        worker.join().expect("cancellation worker");
                    }
                    result
                })
            },
        )?;
        if mode == "request-wait" {
            assert!(request_cancel.is_cancelled());
            assert!(
                !cancellation.is_cancelled(),
                "one RPC must not cancel the daemon"
            );
        }
        if mode == "success" {
            let mut file = result?;
            let mut magic = [0; 4];
            file.read_exact(&mut magic)?;
            assert_eq!(&magic, b"\x7fELF");
            assert!(rustix::io::fcntl_getfd(&file)?.contains(rustix::io::FdFlags::CLOEXEC));
        } else {
            let error = result.expect_err("helper failure");
            if matches!(mode, "wait" | "precancel") {
                assert_eq!(error.kind(), io::ErrorKind::Interrupted);
            }
        }
        assert_eq!(fd_count(), before, "no leaked descriptors after {mode}");
        if !matches!(mode, "precancel" | "spawnfail") {
            let pid = fs::read_to_string(temp.path().join("pid"))?;
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "child reaped after {mode}"
            );
        }
        // A fresh open description acquires immediately after every outcome.
        with_write_lock(
            &lock_path,
            AcquireOptions {
                timeout: Duration::from_millis(50),
                cancelled: None,
            },
            |_| (),
        )?;
    }
    helper_cli_rejects_invalid_inherited_descriptors_before_runtime()?;
    Ok(())
}

fn helper_cli_rejects_invalid_inherited_descriptors_before_runtime() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let runtime = temp.path().join("must-not-exist");
    let binary = env!("CARGO_BIN_EXE_bpfman");
    let output = Command::new(binary).arg("--help").output()?;
    assert!(!String::from_utf8_lossy(&output.stdout).contains("__ns-helper"));
    for (target, socket, namespace, lock) in [
        ("/bin/a", "3", "3", "4"),
        ("/bin/a", "900", "901", "902"),
        ("relative/a", "3", "5", "4"),
        ("/bin/a", "3", "5", "invalid"),
        ("--help", "3", "5", "invalid"),
    ] {
        let output = Command::new(binary)
            .env("BPFMAN_RUNTIME_DIR", &runtime)
            .env(bpfman_lock::WRITER_LOCK_FD_ENV, lock)
            .args([
                "__ns-helper",
                "open-uprobe-target",
                "--socket-fd",
                socket,
                "--namespace-fd",
                namespace,
                "--",
                target,
            ])
            .output()?;
        assert!(!output.status.success());
        assert!(!runtime.exists());
    }
    Ok(())
}
