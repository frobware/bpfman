use bpfman_store::{Error, ErrorKind};

#[derive(Debug, thiserror::Error)]
pub(super) enum Failure {
    #[error("access JSON snapshot")]
    Filesystem(#[from] bpfman_fs::Error),
    #[error("decode JSON snapshot")]
    Json(#[from] serde_json::Error),
    #[error("create JSON store identity")]
    Random(#[from] std::io::Error),
    #[error("unsupported JSON store version: {0}")]
    Version(u32),
    #[error("invalid JSON store: {0}")]
    Invalid(&'static str),
}

impl From<Failure> for Error {
    fn from(cause: Failure) -> Self {
        let kind = match &cause {
            Failure::Filesystem(_) | Failure::Random(_) => ErrorKind::Unavailable,
            Failure::Version(_) => ErrorKind::IncompatibleState,
            Failure::Json(_) | Failure::Invalid(_) => ErrorKind::InvalidData,
        };

        Error::new(kind, cause)
    }
}
