use std::{
    cell::RefCell,
    fs::{File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use rustix::fs::{FlockOperation, flock};

use crate::{
    AcquireOptions, Error, WritePermit,
    error::{Failure, io_error},
};

thread_local! {
    // Diagnostic tripwire, not authority. Inode identity catches path aliases.
    static ACTIVE: RefCell<Vec<(u64, u64)>> = const { RefCell::new(Vec::new()) };
}

pub(super) struct ActiveScope((u64, u64));

impl ActiveScope {
    pub(super) fn enter(file: &File) -> Result<Self, Error> {
        let metadata = file
            .metadata()
            .map_err(|e| io_error("inspect writer lock", e))?;
        let identity = (metadata.dev(), metadata.ino());
        ACTIVE.with(|active| {
            let mut active = active.borrow_mut();
            if active.contains(&identity) {
                return Err(Failure::Reentrant.into());
            }
            active.push(identity);
            Ok(Self(identity))
        })
    }
}

impl Drop for ActiveScope {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.borrow_mut().retain(|id| *id != self.0));
    }
}

/// Acquire an exclusive `flock`, run a callback, then close the descriptor.
///
/// The parent directory is created on first touch. Acquisition uses nonblocking
/// attempts with bounded exponential backoff. Same-thread re-entry fails fast;
/// callers must pass their permit rather than acquire again. Other threads and
/// processes contend normally. Never unlink the lock file while it is in use.
pub fn with_write_lock<T>(
    path: &Path,
    options: AcquireOptions<'_>,
    work: impl for<'lock> FnOnce(WritePermit<'lock>) -> T,
) -> Result<T, Error> {
    let started = Instant::now();
    check_cancelled(options)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|e| io_error(format!("ensure lock directory {}", parent.display()), e))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|e| io_error(format!("open writer lock {}", path.display()), e))?;
    let _active = ActiveScope::enter(&file)?;
    let mut backoff = Duration::from_millis(1);
    let mut attempted = false;
    loop {
        check_cancelled(options)?;
        if attempted && !options.timeout.is_zero() && started.elapsed() >= options.timeout {
            return Err(Failure::TimedOut {
                path: path.to_owned(),
                timeout: options.timeout,
            }
            .into());
        }
        attempted = true;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(work(WritePermit { file: &file })),
            Err(rustix::io::Errno::WOULDBLOCK | rustix::io::Errno::INTR) => {}
            Err(error) => {
                return Err(io_error(
                    format!("acquire writer lock {}", path.display()),
                    error,
                ));
            }
        }
        let wait = if options.timeout.is_zero() {
            backoff
        } else {
            let remaining = options.timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(Failure::TimedOut {
                    path: path.to_owned(),
                    timeout: options.timeout,
                }
                .into());
            }
            backoff.min(remaining)
        };
        std::thread::sleep(wait);
        backoff = (backoff * 2).min(Duration::from_millis(500));
    }
}

fn check_cancelled(options: AcquireOptions<'_>) -> Result<(), Error> {
    if options
        .cancelled
        .is_some_and(|flag| flag.load(Ordering::Relaxed))
    {
        return Err(Failure::Cancelled.into());
    }
    Ok(())
}
