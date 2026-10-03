/// Backend-independent persistence failure categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// State could not be accessed or an operation failed.
    Unavailable,
    /// Existing state uses an unsupported format or version.
    IncompatibleState,
    /// Stored data or ownership evidence violates the domain contract.
    InvalidData,
    /// The operation is outside the supported domain scope.
    Unsupported,
}

/// Classified failure retaining private backend diagnostics.
#[derive(Debug, thiserror::Error)]
#[error("access program store")]
pub struct Error {
    kind: ErrorKind,
    #[source]
    source: Box<dyn std::error::Error + Send + Sync>,
}

impl Error {
    /// Attach a private diagnostic cause to a portable category.
    pub fn new(kind: ErrorKind, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self {
            kind,
            source: Box::new(source),
        }
    }

    /// Category callers may act on without strings or backend downcasts.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }
}
