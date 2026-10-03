use crate::{UnloadCause, UnloadError, UnloadErrorKind, UnloadReport, unload};
use bpfman_core::{UnloadAttempt, UnloadKind};
use bpfman_fs::RuntimeWriter;
use std::{fmt, num::NonZeroU32};
#[derive(Debug, thiserror::Error)]
pub(super) enum Cause {
    #[error("managed program {0} not found")]
    NotFound(NonZeroU32),
    #[error(transparent)]
    Filesystem(#[from] bpfman_fs::Error),
    #[error(transparent)]
    Store(#[from] bpfman_store_sqlite::Error),
}
pub(super) enum Failure {
    Before(UnloadCause),
    Incomplete(Box<UnloadReport>),
}
impl From<Cause> for UnloadCause {
    fn from(cause: Cause) -> Self {
        Self { cause }
    }
}
impl From<bpfman_fs::Error> for UnloadCause {
    fn from(cause: bpfman_fs::Error) -> Self {
        Cause::Filesystem(cause).into()
    }
}
impl From<bpfman_store_sqlite::Error> for UnloadCause {
    fn from(cause: bpfman_store_sqlite::Error) -> Self {
        Cause::Store(cause).into()
    }
}
impl From<UnloadCause> for UnloadError {
    fn from(cause: UnloadCause) -> Self {
        Self {
            failure: Failure::Before(cause),
        }
    }
}
impl UnloadCause {
    /// Application category; concrete backend diagnostics remain in the source chain.
    pub fn kind(&self) -> UnloadErrorKind {
        match &self.cause {
            Cause::NotFound(_) => UnloadErrorKind::NotFound,
            Cause::Store(error) => match error.kind() {
                bpfman_store_sqlite::ErrorKind::Unsupported => UnloadErrorKind::Unsupported,
                bpfman_store_sqlite::ErrorKind::InvalidData
                | bpfman_store_sqlite::ErrorKind::IncompatibleSchema => {
                    UnloadErrorKind::InvalidState
                }
                _ => UnloadErrorKind::Unavailable,
            },
            Cause::Filesystem(error) if error.kind() == bpfman_fs::ErrorKind::UnsafeLayout => {
                UnloadErrorKind::InvalidState
            }
            Cause::Filesystem(_) => UnloadErrorKind::Unavailable,
        }
    }
}
impl UnloadReport {
    /// All attempted effects, including successful ones and previous retry passes.
    pub fn attempts(&self) -> &[UnloadAttempt<UnloadCause>] {
        self.report.attempts()
    }
    /// Retained work, including dependencies that could not yet be attempted.
    pub fn unresolved(&self) -> usize {
        self.report.remaining().len()
    }
    /// Perform one caller-budgeted pass over retained cleanup only, preserving history.
    /// Requires writer authority for the same runtime. Does not reobserve by ID.
    pub fn retry_cleanup(self, writer: &RuntimeWriter<'_>) -> Result<Self, UnloadError> {
        unload::finish(unload::drain(
            writer,
            &mut unload::real::Effects,
            self.report.retry(),
        ))
    }
}
impl UnloadError {
    /// Backend-independent failure category.
    pub fn kind(&self) -> UnloadErrorKind {
        self.primary()
            .map_or(UnloadErrorKind::Unavailable, UnloadCause::kind)
    }
    /// Progress if teardown started; preflight errors have no cleanup report.
    pub fn report(&self) -> Option<&UnloadReport> {
        match &self.failure {
            Failure::Before(_) => None,
            Failure::Incomplete(report) => Some(report),
        }
    }
    /// Explicitly retry retained work. Preflight failures require a fresh request.
    pub fn retry(self, writer: &RuntimeWriter<'_>) -> Result<UnloadReport, Self> {
        match self.failure {
            Failure::Before(_) => Err(self),
            Failure::Incomplete(report) => report.retry_cleanup(writer),
        }
    }
    fn primary(&self) -> Option<&UnloadCause> {
        match &self.failure {
            Failure::Before(cause) => Some(cause),
            Failure::Incomplete(report) => report
                .attempts()
                .iter()
                .filter(|a| matches!(a.kind, UnloadKind::ProgramPin | UnloadKind::ProgramRecord))
                .find_map(|a| a.outcome.as_ref().err()),
        }
    }
}
impl fmt::Debug for UnloadReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnloadReport")
            .field("attempts", &self.attempts().len())
            .field("unresolved", &self.unresolved())
            .finish()
    }
}
impl fmt::Debug for UnloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl fmt::Display for UnloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unload program")?;
        if let Some(report) = self.report() {
            write!(
                f,
                "; {} unresolved teardown instructions",
                report.unresolved()
            )?;
            for attempt in report.attempts() {
                if let Err(error) = &attempt.outcome {
                    if self
                        .primary()
                        .is_some_and(|primary| std::ptr::eq(primary, error))
                    {
                        continue; // the original failure is rendered by Error::source
                    }
                    write!(f, "; {:?}: {error}", attempt.kind)?;
                    let mut source = std::error::Error::source(error);
                    while let Some(cause) = source {
                        write!(f, ": {cause}")?;
                        source = cause.source();
                    }
                }
            }
        }
        Ok(())
    }
}
impl std::error::Error for UnloadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.primary().map(|e| e as _)
    }
}
