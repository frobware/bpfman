//! Concrete Linux implementation of the kernel observation contracts.
//!
//! BPF ABI types and syscalls stay private. Filesystem traversal is delegated
//! to bpfman-fs, which verifies the adopted runtime and canonical pin locations.

mod backend;
mod observe;
mod syscall;

use bpfman_kernel::{Error, ErrorKind};

/// Linux kernel observer selected by the application composition root.
#[derive(Debug, Default)]
pub struct Kernel;
