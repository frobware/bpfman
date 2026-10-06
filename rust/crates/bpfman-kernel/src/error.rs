use crate::{Error, ErrorKind};

#[derive(Debug, thiserror::Error)]
#[error("{operation}")]
pub(super) struct Cause {
    kind: ErrorKind,
    operation: &'static str,
    #[source]
    source: Box<dyn std::error::Error + Send + Sync>,
}

impl Error {
    /// Retain a concrete backend cause behind a portable classification.
    pub fn new(
        kind: ErrorKind,
        operation: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            cause: Box::new(Cause {
                kind,
                operation,
                source: Box::new(source),
            }),
        }
    }

    /// Classify without inspecting or downcasting the source.
    pub fn kind(&self) -> ErrorKind {
        self.cause.kind
    }
}
