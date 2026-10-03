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
    #[error(transparent)]
    Store(bpfman_store_sqlite::Error),
    #[error(transparent)]
    Filesystem(bpfman_fs::Error),
}

pub(super) fn store_error(source: bpfman_store_sqlite::Error) -> Error {
    let kind = match source.kind() {
        bpfman_store_sqlite::ErrorKind::Unavailable => ErrorKind::Unavailable,
        bpfman_store_sqlite::ErrorKind::IncompatibleSchema => ErrorKind::IncompatibleState,
        bpfman_store_sqlite::ErrorKind::Unsupported
        | bpfman_store_sqlite::ErrorKind::InvalidData => ErrorKind::InvalidState,
    };
    Error {
        kind,
        source: Failure::Store(source),
    }
}

pub(super) fn filesystem_error(source: bpfman_fs::Error) -> Error {
    let kind = match source.kind() {
        bpfman_fs::ErrorKind::TimedOut => ErrorKind::TimedOut,
        bpfman_fs::ErrorKind::Cancelled => ErrorKind::Cancelled,
        bpfman_fs::ErrorKind::UnsafeLayout => ErrorKind::InvalidState,
        bpfman_fs::ErrorKind::Unavailable => ErrorKind::Unavailable,
    };
    Error {
        kind,
        source: Failure::Filesystem(source),
    }
}
