//! Cross-process writer lock compatible with Go's `lock` package.
//!
//! This crate supplies low-level [`WritePermit`] values, not root-bound runtime
//! authority. `bpfman-fs` binds a permit to its opened root as `RuntimeWriter`;
//! application mutations require that stronger capability. A permit cannot
//! escape its callback. Helpers receive duplicated
//! descriptors, never reacquire the lock by path. Descriptors are closed, not
//! explicitly unlocked: inherited copies must keep the same lock alive.

use std::{fs::File, sync::atomic::AtomicBool, time::Duration};

mod acquire;
mod error;
mod inherited;

pub use acquire::{with_write_lock, with_write_lock_file};

/// Environment variable used by Go and Rust helpers to identify an inherited fd.
pub const WRITER_LOCK_FD_ENV: &str = "BPFMAN_WRITER_LOCK_FD";

/// Acquisition budget and optional cooperative cancellation.
///
/// Neither timeout nor cancellation interrupts the callback once acquired.
#[derive(Clone, Copy, Debug)]
pub struct AcquireOptions<'a> {
    /// Maximum acquisition wait; zero waits indefinitely.
    pub timeout: Duration,
    /// Optional flag checked during acquisition; true cancels the wait.
    pub cancelled: Option<&'a AtomicBool>,
}

/// Proof that this borrowed file's exclusive lock is held, without a runtime identity.
///
/// It cannot be constructed by callers or returned from the callback:
///
/// ```compile_fail
/// use bpfman_lock::{AcquireOptions, with_write_lock};
/// use std::{path::Path, time::Duration};
/// let permit = with_write_lock(Path::new("/tmp/bpfman.lock"),
///     AcquireOptions { timeout: Duration::ZERO, cancelled: None }, |permit| permit);
/// ```
pub struct WritePermit<'lock> {
    file: &'lock File,
}

/// An owned descriptor received by a helper, preserving the parent's lock.
pub struct InheritedWriteLock {
    file: File,
}

/// Backend-independent classification of a lock failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// Acquisition exhausted its wait budget.
    TimedOut,
    /// Acquisition was cancelled before entering the callback.
    Cancelled,
    /// The same thread attempted to reacquire an active lock.
    Reentrant,
    /// The filesystem or locking operation failed.
    Unavailable,
}

/// Opaque lock failure with diagnostic context and a private OS cause.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct Error {
    cause: error::Failure,
}
