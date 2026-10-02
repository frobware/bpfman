use crate::{Error, ErrorKind};

#[derive(Debug, thiserror::Error)]
pub(super) enum Failure {
    #[error("{operation}")]
    Io {
        operation: &'static str,
        source: std::io::Error,
    },
    #[error("unsafe runtime layout: {0}")]
    Unsafe(&'static str),
    #[error(transparent)]
    Lock(bpfman_lock::Error),
}

impl Error {
    /// Classify without downcasting backend causes.
    pub fn kind(&self) -> ErrorKind {
        match &self.cause {
            Failure::Unsafe(_) => ErrorKind::UnsafeLayout,
            Failure::Io { source, .. } => match source.raw_os_error() {
                Some(code)
                    if code == rustix::io::Errno::LOOP.raw_os_error()
                        || code == rustix::io::Errno::XDEV.raw_os_error()
                        || code == rustix::io::Errno::NOTDIR.raw_os_error() =>
                {
                    ErrorKind::UnsafeLayout
                }
                _ => ErrorKind::Unavailable,
            },
            Failure::Lock(error) => match error.kind() {
                bpfman_lock::ErrorKind::TimedOut => ErrorKind::TimedOut,
                bpfman_lock::ErrorKind::Cancelled => ErrorKind::Cancelled,
                _ => ErrorKind::Unavailable,
            },
        }
    }
}

impl From<Failure> for Error {
    fn from(cause: Failure) -> Self {
        Self { cause }
    }
}

pub(super) fn io(operation: &'static str, source: impl Into<std::io::Error>) -> Error {
    Failure::Io {
        operation,
        source: source.into(),
    }
    .into()
}
