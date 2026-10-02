use std::{io, path::PathBuf, time::Duration};

use crate::{Error, ErrorKind};

impl Error {
    /// Classify the failure without inspecting an operating-system error.
    pub fn kind(&self) -> ErrorKind {
        match self.cause {
            Failure::TimedOut { .. } => ErrorKind::TimedOut,
            Failure::Cancelled => ErrorKind::Cancelled,
            Failure::Reentrant => ErrorKind::Reentrant,
            Failure::Io { .. } => ErrorKind::Unavailable,
        }
    }
}

impl From<Failure> for Error {
    fn from(cause: Failure) -> Self {
        Self { cause }
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum Failure {
    #[error("timed out waiting for lock {} after {timeout:?}", path.display())]
    TimedOut { path: PathBuf, timeout: Duration },
    #[error("writer lock acquisition cancelled")]
    Cancelled,
    #[error("writer lock re-entry: pass the existing WritePermit instead of reacquiring")]
    Reentrant,
    #[error("{operation}")]
    Io {
        operation: String,
        source: io::Error,
    },
}

pub(super) fn io_error(operation: impl Into<String>, source: impl Into<io::Error>) -> Error {
    Failure::Io {
        operation: operation.into(),
        source: source.into(),
    }
    .into()
}
