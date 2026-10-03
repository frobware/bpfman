//! Lock contracts use real flock and separate processes, without root or BPF.

use std::{
    fs::File,
    io::{BufRead, BufReader, Write},
    os::fd::AsFd,
    path::Path,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use bpfman_lock::{AcquireOptions, ErrorKind, InheritedWriteLock, with_write_lock};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn options() -> AcquireOptions<'static> {
    AcquireOptions {
        timeout: Duration::from_millis(40),
        cancelled: None,
    }
}

fn peer(path: &Path, mode: &str) -> std::io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "process_peer", "--nocapture"])
        .env("BPFMAN_TEST_LOCK", path)
        .env("BPFMAN_TEST_LOCK_MODE", mode);

    Ok(command)
}

fn assert_peer(path: &Path, mode: &str) -> Result {
    let output = peer(path, mode)?.output()?;

    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    Ok(())
}

#[test]
fn process_peer() -> Result {
    let Some(path) = std::env::var_os("BPFMAN_TEST_LOCK") else {
        return Ok(());
    };
    let path = Path::new(&path);

    match std::env::var("BPFMAN_TEST_LOCK_MODE")?.as_str() {
        "blocked" => assert_eq!(
            with_write_lock(path, options(), |_| ())
                .expect_err("another process holds the lock")
                .kind(),
            ErrorKind::TimedOut
        ),
        "acquire" => with_write_lock(path, options(), |_| ())?,
        "inherited" => {
            // Stdio maps the duplicated descriptor to fd 0, with no unsafe code.
            let inherited =
                InheritedWriteLock::from_fd(std::io::stdin().as_fd().try_clone_to_owned()?)?;
            inherited.with_permit(|_| ())?;
            println!("LOCK_READY");
            std::io::stdout().flush()?;
            let release = path.with_extension("release");
            let start = Instant::now();

            while !release.exists() && start.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(5));
            }

            assert!(release.exists(), "parent failed to release helper");
        }
        mode => return Err(format!("unknown peer mode: {mode}").into()),
    }

    Ok(())
}

#[test]
fn creates_directories_and_excludes_other_processes() -> Result {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("runtime/.lock");
    with_write_lock(&path, options(), |_| assert_peer(&path, "blocked"))??;

    assert!(path.is_file());
    assert_peer(&path, "acquire")
}

#[test]
fn rejects_same_thread_reentry_including_aliases_but_allows_other_locks() -> Result {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join(".lock");
    let alias = directory.path().join("alias");
    with_write_lock(&path, options(), |_| -> Result {
        std::fs::hard_link(&path, &alias)?;

        for name in [&path, &alias] {
            assert_eq!(
                with_write_lock(name, options(), |_| ())
                    .expect_err("re-entry")
                    .kind(),
                ErrorKind::Reentrant
            );
        }

        with_write_lock(&directory.path().join("other"), options(), |_| ())?;

        Ok(())
    })??;
    with_write_lock(&path, options(), |_| ())?;

    Ok(())
}

#[test]
fn work_outlives_acquisition_budget_and_preserves_its_error() -> Result {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join(".lock");
    let cancelled = AtomicBool::new(false);
    let result = with_write_lock(
        &path,
        AcquireOptions {
            timeout: Duration::from_millis(1),
            cancelled: Some(&cancelled),
        },
        |_| {
            cancelled.store(true, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(10));
            Err::<(), _>("work failed")
        },
    )?;

    assert_eq!(result, Err("work failed"));
    assert_peer(&path, "acquire")
}

#[test]
fn cancellation_stops_an_indefinite_wait() -> Result {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join(".lock");
    let cancelled = AtomicBool::new(false);
    with_write_lock(&path, options(), |_| {
        std::thread::scope(|threads| {
            let (started, waiting) = mpsc::channel();
            let path = &path;
            let cancelled = &cancelled;
            let waiter = threads.spawn(move || {
                started.send(()).expect("notify parent");
                with_write_lock(
                    path,
                    AcquireOptions {
                        timeout: Duration::ZERO,
                        cancelled: Some(cancelled),
                    },
                    |_| (),
                )
            });
            waiting.recv().expect("waiter started");
            cancelled.store(true, Ordering::Relaxed);

            assert_eq!(
                waiter
                    .join()
                    .expect("waiter thread")
                    .expect_err("cancelled")
                    .kind(),
                ErrorKind::Cancelled
            );
        });
    })?;

    Ok(())
}

#[test]
fn waiter_acquires_after_release() -> Result {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join(".lock");
    std::thread::scope(|threads| -> Result {
        let (ready, held) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let holder_path = &path;
        let holder = threads.spawn(move || {
            with_write_lock(holder_path, options(), |_| {
                ready.send(()).expect("holder ready");
                released
                    .recv_timeout(Duration::from_secs(5))
                    .expect("release holder");
            })
        });
        held.recv_timeout(Duration::from_secs(5))?;
        release.send(())?;
        with_write_lock(
            &path,
            AcquireOptions {
                timeout: Duration::from_secs(5),
                cancelled: None,
            },
            |_| (),
        )?;
        holder.join().expect("holder thread")?;

        Ok(())
    })
}

#[test]
fn duplicated_descriptor_keeps_lock_after_parent_scope_exits() -> Result {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join(".lock");
    let descriptor = with_write_lock(&path, options(), |scope| scope.duplicate_fd())??;
    let inherited = InheritedWriteLock::from_fd(descriptor)?;
    inherited.with_permit(|_| assert_peer(&path, "blocked"))??;
    drop(inherited);
    assert_peer(&path, "acquire")
}

#[test]
fn inherited_child_keeps_lock_after_parent_scope_exits() -> Result {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join(".lock");
    let mut child = with_write_lock(&path, options(), |scope| -> Result<_> {
        Ok(peer(&path, "inherited")?
            .stdin(Stdio::from(scope.duplicate_fd()?))
            .stdout(Stdio::piped())
            .spawn()?)
    })??;
    let stdout = child.stdout.take().ok_or("missing helper stdout")?;
    let mut reader = BufReader::new(stdout);
    let mut ready = false;

    for line in (&mut reader).lines() {
        if line? == "LOCK_READY" {
            ready = true;
            break;
        }
    }

    assert!(ready, "helper did not inherit the descriptor");

    let blocked = assert_peer(&path, "blocked");
    File::create(path.with_extension("release"))?;

    assert!(child.wait()?.success());
    blocked?;
    assert_peer(&path, "acquire")
}

#[test]
fn inherited_descriptor_must_be_exclusive() -> Result {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join(".lock");
    with_write_lock(&path, options(), |_| -> Result {
        let independent = File::open(&path)?;
        let error = InheritedWriteLock::from_fd(independent.into())
            .err()
            .ok_or("accepted unlocked descriptor")?;

        assert_eq!(error.kind(), ErrorKind::Unavailable);

        Ok(())
    })??;

    Ok(())
}
