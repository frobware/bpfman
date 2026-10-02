//! Application operations: gather observations, then evaluate pure policy.

mod error;
mod list;
mod store;

pub use list::list_programs;

/// Backend-independent application failure classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// Runtime state could not be created, opened, or read.
    Unavailable,
    /// The writer lock could not be acquired within its wait budget.
    TimedOut,
    /// The writer-lock acquisition was cancelled.
    Cancelled,
    /// Stored state requires another schema version or initialisation.
    IncompatibleState,
    /// Stored observations violate domain invariants.
    InvalidState,
}

/// Application failure; concrete adapter errors remain private diagnostics.
#[derive(Debug, thiserror::Error)]
#[error("list managed programs")]
pub struct Error {
    kind: ErrorKind,
    #[source]
    source: error::Failure,
}
