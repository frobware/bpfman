use crate::SCHEMA_VERSION;

/// Backend-independent classification of a store failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// The requested mutation is outside this adapter's implemented scope.
    Unsupported,
    /// State could not be created, opened, or read.
    Unavailable,
    /// State is uninitialised or uses an unsupported schema.
    IncompatibleSchema,
    /// Stored values violate the expected domain contract.
    InvalidData,
}

/// Opaque store failure; concrete backend types stay behind this boundary.
#[derive(Debug, thiserror::Error)]
#[error("access program store")]
pub struct Error {
    #[source]
    cause: Failure,
}

impl Error {
    /// Classification for callers; the backend cause is diagnostic information.
    pub fn kind(&self) -> ErrorKind {
        match self.cause {
            Failure::Unsupported(_) => ErrorKind::Unsupported,
            Failure::Sqlite(_) | Failure::Runtime(_) | Failure::Filesystem { .. } => {
                ErrorKind::Unavailable
            }
            Failure::SchemaVersion(_) | Failure::UnsupportedSchema { .. } => {
                ErrorKind::IncompatibleSchema
            }
            Failure::InvalidRecord { .. } | Failure::Metadata { .. } => ErrorKind::InvalidData,
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
    #[error(transparent)]
    Runtime(#[from] bpfman_fs::Error),
    #[error("unsupported unload: {0}")]
    Unsupported(&'static str),
    #[error("SQLite operation failed")]
    Sqlite(#[from] rusqlite::Error),
    #[error("access database at {}", path.display())]
    Filesystem {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("cannot read Go schema version")]
    SchemaVersion(#[source] rusqlite::Error),
    #[error("schema version mismatch: database is at {found}, expected {SCHEMA_VERSION}")]
    UnsupportedSchema { found: i64 },
    #[error("invalid program {id}: {reason}")]
    InvalidRecord { id: i64, reason: String },
    #[error("invalid metadata for program {id}")]
    Metadata { id: i64, source: serde_json::Error },
}
