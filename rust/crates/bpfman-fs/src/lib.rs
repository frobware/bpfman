//! Runtime filesystem vocabulary and layout.
//!
//! Constructing a layout performs no I/O and proves only that its root is a
//! validated absolute path, not that directories exist or bpffs is mounted.

use std::path::PathBuf;

mod layout;

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
