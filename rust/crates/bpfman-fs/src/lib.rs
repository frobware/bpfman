//! Runtime filesystem vocabulary and layout.
//!
//! Constructing a layout performs no I/O and proves only that its root is a
//! validated absolute path, not that directories exist or bpffs is mounted.

use std::{os::fd::OwnedFd, path::PathBuf};

mod artifacts;
mod directory;
mod error;
mod layout;
mod removal;

/// Default runtime root, shared by front ends rather than duplicated there.
pub const DEFAULT_RUNTIME_ROOT: &str = "/run/bpfman";

/// Immutable, validated description of where runtime files belong.
///
/// Construct with [`TryFrom<PathBuf>`]. There is no invalid zero/default value
/// and callers cannot change the root without constructing another layout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeLayout {
    root: PathBuf,
}

/// An opened runtime root, not proof that bpffs is mounted or objects exist.
///
/// Managed filesystem operations stay relative to this directory descriptor.
/// SQLite remains a separate pathname-based adapter; this is not a sandbox for
/// SQLite or for other processes with authority to mutate the same filesystem.
pub struct RuntimeDirectory {
    root: OwnedFd,
    layout: RuntimeLayout,
}

/// Mutation authority bound to exactly one runtime's opened root and lock.
///
/// There is no public constructor, target substitution, or permit extraction.
/// The authority cannot escape its callback:
///
/// ```compile_fail
/// use bpfman_fs::RuntimeDirectory;
/// use bpfman_lock::AcquireOptions;
/// fn escape(root: &RuntimeDirectory, options: AcquireOptions<'_>) {
///     let writer = root.with_writer(options, |writer| writer);
/// }
/// ```
///
/// An unrelated low-level lock cannot be paired with a runtime by the caller:
///
/// ```compile_fail
/// use bpfman_fs::{RuntimeDirectory, RuntimeWriter};
/// use bpfman_lock::WritePermit;
/// fn forge<'a>(runtime: &'a RuntimeDirectory, permit: WritePermit<'a>) -> RuntimeWriter<'a> {
///     RuntimeWriter { runtime, _permit: permit }
/// }
/// ```
///
/// Authority cannot be cloned into another owner:
///
/// ```compile_fail
/// use bpfman_fs::RuntimeWriter;
/// fn duplicate<'a>(writer: &RuntimeWriter<'a>) -> RuntimeWriter<'a> {
///     (*writer).clone()
/// }
/// ```
pub struct RuntimeWriter<'scope> {
    runtime: &'scope RuntimeDirectory,
    _permit: bpfman_lock::WritePermit<'scope>,
}

/// Portable classification of filesystem and writer-acquisition failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// The operating system could not complete the operation.
    Unavailable,
    /// The path violates confinement or has an unexpected object type.
    UnsafeLayout,
    /// The writer-lock acquisition budget expired.
    TimedOut,
    /// Writer-lock acquisition was cancelled.
    Cancelled,
}

/// Filesystem boundary failure with private operating-system diagnostics.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct Error {
    cause: Box<error::Failure>,
}

/// Invalid configuration rejected before any filesystem effects.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum LayoutError {
    /// The filesystem root cannot be adopted as a bpfman runtime.
    #[error("filesystem root cannot be used as the runtime directory")]
    FilesystemRoot,
    /// No root was supplied.
    #[error("runtime directory must not be empty")]
    EmptyRoot,
    /// A relative root would depend on the process's working directory.
    #[error("runtime directory must be absolute: {}", .0.display())]
    RelativeRoot(PathBuf),
}

/// Verified bpffs and map collection descriptors for a single runtime.
#[derive(Debug)]
pub struct PreparedLoad {
    root: artifacts::Identity,
    bpffs: OwnedFd,
    maps: OwnedFd,
}

/// Non-cloneable ownership of a program pin; dropping it does not unpin.
///
/// ```compile_fail
/// fn duplicate(pin: &bpfman_fs::ProgramPin) { let _ = (*pin).clone(); }
/// ```
/// ```compile_fail
/// let pin = bpfman_fs::ProgramPin { id: std::num::NonZeroU32::MIN };
/// ```
#[derive(Debug)]
pub struct ProgramPin {
    id: std::num::NonZeroU32,
    entry: Box<artifacts::Entry>,
}

/// Non-cloneable ownership of one private map pin.
#[derive(Debug)]
pub struct MapPin {
    entry: Box<artifacts::Entry>,
}

/// Owned map container. Cleanup requires successful removal of all owned pins.
#[derive(Debug)]
pub struct MapDirectory {
    entry: Box<artifacts::Entry>,
}

/// Owned staged or published bytecode, including any partially written files.
#[derive(Debug)]
pub struct Bytecode {
    directory: Box<artifacts::Entry>,
    files: Vec<artifacts::Entry>,
}
