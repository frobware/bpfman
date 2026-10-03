//! Read-only Linux BPF observations, with no loading, attachment or pin mutation.
//! Unsafe code is confined to the private syscall boundary. Kernel and backend
//! representations do not escape; optional values retain availability evidence.

mod observe;
mod syscall;

pub use observe::{observe_map, observe_program};

/// Portable classification of a kernel observation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// The requested kernel ID no longer exists.
    Missing,
    /// Observation was denied or failed at an OS boundary.
    Unavailable,
    /// The kernel or procfs returned inconsistent data.
    InvalidData,
}

/// Opaque observation error with an OS diagnostic source.
#[derive(Debug, thiserror::Error)]
#[error("{operation}")]
pub struct Error {
    operation: &'static str,
    #[source]
    source: std::io::Error,
}
