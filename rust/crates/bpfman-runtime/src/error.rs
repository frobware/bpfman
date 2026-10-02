use crate::{Error, ErrorKind};

impl Error {
    /// Application classification, independent of the concrete adapter.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum Failure {
    #[error("schema version mismatch: database is at {found}, expected {expected}")]
    IncompatibleSchema { found: i64, expected: i64 },
    #[error("setup selected an existing store without an observation")]
    MissingSetupObservation,
    #[error(transparent)]
    Store(bpfman_store_sqlite::Error),
    #[error(transparent)]
    Lock(bpfman_lock::Error),
}

pub(super) fn store_error(source: bpfman_store_sqlite::Error) -> Error {
    let kind = match source.kind() {
        bpfman_store_sqlite::ErrorKind::Unavailable => ErrorKind::Unavailable,
        bpfman_store_sqlite::ErrorKind::IncompatibleSchema => ErrorKind::IncompatibleState,
        bpfman_store_sqlite::ErrorKind::InvalidData => ErrorKind::InvalidState,
    };
    Error {
        kind,
        source: Failure::Store(source),
    }
}

pub(super) fn lock_error(source: bpfman_lock::Error) -> Error {
    let kind = match source.kind() {
        bpfman_lock::ErrorKind::TimedOut => ErrorKind::TimedOut,
        bpfman_lock::ErrorKind::Cancelled => ErrorKind::Cancelled,
        bpfman_lock::ErrorKind::Reentrant | bpfman_lock::ErrorKind::Unavailable => {
            ErrorKind::Unavailable
        }
    };
    Error {
        kind,
        source: Failure::Lock(source),
    }
}
