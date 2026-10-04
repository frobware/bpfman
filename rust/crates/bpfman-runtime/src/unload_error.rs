use crate::{Bpfman, UnloadCause, UnloadError, UnloadErrorKind, UnloadReport, unload};
use bpfman_core::{UnloadAttempt, UnloadKind};
use bpfman_store::{LinkReader, LinkStore, OpenStore, UnloadStore};
use std::{fmt, num::NonZeroU32};

#[derive(Debug, thiserror::Error)]
pub(super) enum Cause {
    #[error("unload cancelled before teardown")]
    Cancelled,
    #[error("managed program {0} not found")]
    NotFound(NonZeroU32),
    #[error("{0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Filesystem(#[from] bpfman_fs::Error),
    #[error(transparent)]
    Store(#[from] bpfman_store::Error),
}

pub(super) enum Failure<S: UnloadStore + LinkStore> {
    Before(UnloadCause),
    Incomplete(Box<UnloadReport<S>>),
    RetryBlocked {
        cause: UnloadCause,
        report: Box<UnloadReport<S>>,
    },
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

impl From<bpfman_store::Error> for UnloadCause {
    fn from(cause: bpfman_store::Error) -> Self {
        Cause::Store(cause).into()
    }
}

impl<S: UnloadStore + LinkStore> From<UnloadCause> for UnloadError<S> {
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
            Cause::Invalid(_) => UnloadErrorKind::InvalidState,
            Cause::Cancelled => UnloadErrorKind::Cancelled,
            Cause::Filesystem(e) if e.kind() == bpfman_fs::ErrorKind::Cancelled => {
                UnloadErrorKind::Cancelled
            }
            Cause::NotFound(_) => UnloadErrorKind::NotFound,
            Cause::Store(error) => match error.kind() {
                bpfman_store::ErrorKind::Unsupported => UnloadErrorKind::Unsupported,
                bpfman_store::ErrorKind::InvalidData
                | bpfman_store::ErrorKind::IncompatibleState => UnloadErrorKind::InvalidState,
                _ => UnloadErrorKind::Unavailable,
            },
            Cause::Filesystem(error) if error.kind() == bpfman_fs::ErrorKind::UnsafeLayout => {
                UnloadErrorKind::InvalidState
            }
            Cause::Filesystem(_) => UnloadErrorKind::Unavailable,
        }
    }
}

impl<S: UnloadStore + LinkStore> UnloadReport<S> {
    /// All attempted effects, including successful ones and previous retry passes.
    pub fn attempts(&self) -> &[UnloadAttempt<UnloadCause>] {
        self.report.attempts()
    }

    /// Retained work, including dependencies that could not yet be attempted.
    pub fn unresolved(&self) -> usize {
        self.report.remaining().len()
    }
}

impl<S: UnloadStore + LinkStore> UnloadError<S> {
    /// Backend-independent failure category.
    pub fn kind(&self) -> UnloadErrorKind {
        self.primary()
            .map_or(UnloadErrorKind::Unavailable, UnloadCause::kind)
    }

    /// Progress if teardown started; preflight errors have no cleanup report.
    pub fn report(&self) -> Option<&UnloadReport<S>> {
        match &self.failure {
            Failure::Before(_) => None,
            Failure::Incomplete(report) | Failure::RetryBlocked { report, .. } => Some(report),
        }
    }

    fn primary(&self) -> Option<&UnloadCause> {
        match &self.failure {
            Failure::Before(cause) | Failure::RetryBlocked { cause, .. } => Some(cause),
            Failure::Incomplete(report) => report
                .attempts()
                .iter()
                .filter(|a| {
                    matches!(
                        a.kind,
                        UnloadKind::LinkPin(_)
                            | UnloadKind::LinkRecord(_)
                            | UnloadKind::ProgramPin
                            | UnloadKind::ProgramRecord
                    )
                })
                .find_map(|a| a.outcome.as_ref().err()),
        }
    }
}

impl<S: OpenStore + UnloadStore + LinkStore> Bpfman<S>
where
    S::Reader: LinkReader,
{
    /// Retry retained teardown once, acquiring this instance's writer lock.
    /// Preflight failures are returned unchanged and require a fresh request.
    pub fn retry_unload(&self, error: UnloadError<S>) -> Result<UnloadReport<S>, UnloadError<S>> {
        self.retry_unload_with_cancellation(error, &crate::Cancellation::new())
    }

    /// Retry retained teardown with cancellable lock admission.
    pub fn retry_unload_with_cancellation(
        &self,
        error: UnloadError<S>,
        cancellation: &crate::Cancellation,
    ) -> Result<UnloadReport<S>, UnloadError<S>> {
        match error.failure {
            Failure::Before(_) => Err(error),
            Failure::Incomplete(report) | Failure::RetryBlocked { report, .. } => {
                self.retry_unload_cleanup_with_cancellation(*report, cancellation)
            }
        }
    }

    /// Retry retained cleanup once, preserving receipts if lock acquisition fails.
    /// No new observations or automatic retry loops are performed.
    pub fn retry_unload_cleanup(
        &self,
        report: UnloadReport<S>,
    ) -> Result<UnloadReport<S>, UnloadError<S>> {
        self.retry_unload_cleanup_with_cancellation(report, &crate::Cancellation::new())
    }

    /// Cancel admission without losing receipts. An admitted cleanup pass runs
    /// to completion even if cancellation is requested while it is executing.
    #[tracing::instrument(name = "program.retry_unload_cleanup", level = "debug", skip_all, err)]
    pub fn retry_unload_cleanup_with_cancellation(
        &self,
        report: UnloadReport<S>,
        cancellation: &crate::Cancellation,
    ) -> Result<UnloadReport<S>, UnloadError<S>> {
        // The callback borrows this slot so acquisition failure cannot drop its receipts.
        let mut pending = Some(report);
        let result = self.store.runtime().with_writer(
            bpfman_lock::AcquireOptions {
                timeout: self.lock_timeout,
                cancelled: Some(cancellation.flag()),
            },
            |writer| {
                pending.take().map(|report| {
                    unload::finish::<S>(unload::drain(
                        &writer,
                        &mut unload::real::Effects(&self.store),
                        report.report.retry(),
                    ))
                })
            },
        );

        match result {
            Ok(Some(result)) => result,
            Err(cause) => {
                let cause = UnloadCause::from(cause);
                Err(UnloadError {
                    failure: match pending {
                        Some(report) => Failure::RetryBlocked {
                            cause,
                            report: Box::new(report),
                        },
                        None => Failure::Before(cause),
                    },
                })
            }
            Ok(None) => Err(UnloadCause::from(bpfman_store::Error::new(
                bpfman_store::ErrorKind::InvalidData,
                std::io::Error::other("cleanup callback has no retained report"),
            ))
            .into()),
        }
    }
}

impl<S: UnloadStore + LinkStore> fmt::Debug for UnloadReport<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnloadReport")
            .field("attempts", &self.attempts().len())
            .field("unresolved", &self.unresolved())
            .finish()
    }
}

impl<S: UnloadStore + LinkStore> fmt::Debug for UnloadError<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl<S: UnloadStore + LinkStore> fmt::Display for UnloadError<S> {
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

impl<S: UnloadStore + LinkStore> std::error::Error for UnloadError<S> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.primary().map(|e| e as _)
    }
}
