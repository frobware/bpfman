use crate::{Bpfman, UnloadCause, UnloadError, UnloadErrorKind, UnloadReport};
use bpfman_core::{UnloadAttempt, UnloadKind};
use bpfman_store::{LinkReader, LinkStore, UnloadStore};
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
    Kernel(#[from] bpfman_kernel::Error),
    #[error(transparent)]
    Filesystem(#[from] bpfman_fs::Error),
    #[error(transparent)]
    Store(#[from] bpfman_store::Error),
}

pub(super) enum Failure<
    S: UnloadStore + LinkStore + bpfman_store::XdpStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement,
> {
    Before(UnloadCause),
    Incomplete(Box<UnloadReport<S, K>>),
    RetryBlocked {
        cause: UnloadCause,
        report: Box<UnloadReport<S, K>>,
    },
}

impl From<Cause> for UnloadCause {
    fn from(cause: Cause) -> Self {
        Self { cause }
    }
}

impl From<bpfman_kernel::Error> for UnloadCause {
    fn from(cause: bpfman_kernel::Error) -> Self {
        Cause::Kernel(cause).into()
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

impl<
    S: UnloadStore + LinkStore + bpfman_store::XdpStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement,
> From<UnloadCause> for UnloadError<S, K>
{
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
            Cause::Kernel(error) => match error.kind() {
                bpfman_kernel::ErrorKind::InvalidData | bpfman_kernel::ErrorKind::InvalidInput => {
                    UnloadErrorKind::InvalidState
                }
                bpfman_kernel::ErrorKind::Unsupported => UnloadErrorKind::Unsupported,
                _ => UnloadErrorKind::Unavailable,
            },
        }
    }
}

impl<
    S: UnloadStore + LinkStore + bpfman_store::XdpStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement,
> UnloadReport<S, K>
{
    /// Program and standalone-link effects, including previous retry passes.
    /// Dispatcher work is reported separately by [`Self::xdp_attempts`].
    pub fn attempts(&self) -> &[UnloadAttempt<UnloadCause>] {
        self.report.attempts()
    }

    /// Dispatcher detach and recovery outcomes, before program teardown.
    pub fn xdp_attempts(&self) -> &[crate::UnloadXdpAttempt<S, K>] {
        self.xdp.attempts()
    }

    /// Retained work, including dependencies that could not yet be attempted.
    pub fn unresolved(&self) -> usize {
        self.report.remaining().len() + self.xdp.unresolved()
    }
}

impl<
    S: UnloadStore + LinkStore + bpfman_store::XdpStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement,
> UnloadError<S, K>
{
    /// Backend-independent failure category.
    pub fn kind(&self) -> UnloadErrorKind {
        if let Some(cause) = self.primary() {
            return cause.kind();
        }
        match self
            .report()
            .and_then(|r| r.xdp.cause())
            .map(crate::LinkCause::kind)
        {
            Some(crate::LinkErrorKind::Cancelled) => UnloadErrorKind::Cancelled,
            Some(crate::LinkErrorKind::NotFound) => UnloadErrorKind::NotFound,
            Some(crate::LinkErrorKind::InvalidState) => UnloadErrorKind::InvalidState,
            Some(crate::LinkErrorKind::Unsupported) => UnloadErrorKind::Unsupported,
            _ => UnloadErrorKind::Unavailable,
        }
    }

    /// Progress if teardown started; preflight errors have no cleanup report.
    pub fn report(&self) -> Option<&UnloadReport<S, K>> {
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

impl<
    S: bpfman_store::XdpReplacementStore + UnloadStore + LinkStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement,
> Bpfman<S, K>
where
    S::Reader: LinkReader,
{
    /// Retry retained teardown once, acquiring this instance's writer lock.
    /// Preflight failures are returned unchanged and require a fresh request.
    pub fn retry_unload(
        &self,
        error: UnloadError<S, K>,
    ) -> Result<UnloadReport<S, K>, UnloadError<S, K>> {
        self.retry_unload_with_cancellation(error, &crate::Cancellation::new())
    }

    /// Retry retained teardown with cancellable lock admission.
    pub fn retry_unload_with_cancellation(
        &self,
        error: UnloadError<S, K>,
        cancellation: &crate::Cancellation,
    ) -> Result<UnloadReport<S, K>, UnloadError<S, K>> {
        match error.failure {
            Failure::Before(_) => Err(error),
            Failure::Incomplete(report) | Failure::RetryBlocked { report, .. } => {
                self.retry_unload_cleanup_with_cancellation(*report, cancellation)
            }
        }
    }

    /// Retry retained cleanup once, preserving receipts if lock acquisition fails.
    /// Dispatcher continuations revalidate logical member identity; cleanup uses retained receipts.
    /// Failed forward detaches are never retried inline with their recovery.
    pub fn retry_unload_cleanup(
        &self,
        report: UnloadReport<S, K>,
    ) -> Result<UnloadReport<S, K>, UnloadError<S, K>> {
        self.retry_unload_cleanup_with_cancellation(report, &crate::Cancellation::new())
    }

    /// Cancel admission without losing receipts. An admitted cleanup pass runs
    /// to completion even if cancellation is requested while it is executing.
    #[tracing::instrument(name = "program.retry_unload_cleanup", level = "debug", skip_all, err)]
    pub fn retry_unload_cleanup_with_cancellation(
        &self,
        report: UnloadReport<S, K>,
        cancellation: &crate::Cancellation,
    ) -> Result<UnloadReport<S, K>, UnloadError<S, K>> {
        // The callback borrows this slot so acquisition failure cannot drop its receipts.
        let mut pending = Some(report);
        let result = self.store.runtime().with_writer(
            bpfman_lock::AcquireOptions {
                timeout: self.lock_timeout,
                cancelled: Some(cancellation.flag()),
            },
            |writer| {
                pending
                    .take()
                    .map(|report| crate::unload_xdp::resume(self, &writer, report))
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

impl<
    S: UnloadStore + LinkStore + bpfman_store::XdpStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement,
> fmt::Debug for UnloadReport<S, K>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnloadReport")
            .field("attempts", &self.attempts().len())
            .field("unresolved", &self.unresolved())
            .finish()
    }
}

impl<
    S: UnloadStore + LinkStore + bpfman_store::XdpStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement,
> fmt::Debug for UnloadError<S, K>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl<
    S: UnloadStore + LinkStore + bpfman_store::XdpStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement,
> fmt::Display for UnloadError<S, K>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unload program")?;

        if let Some(report) = self.report() {
            write!(
                f,
                "; {} unresolved teardown instructions",
                report.unresolved()
            )?;

            if let Some(attempt) = report.xdp_attempts().last() {
                if let Err(error) = attempt.outcome() {
                    write!(f, "; XDP link {}: {error}", attempt.link_id())?;
                }
            }

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

impl<
    S: UnloadStore + LinkStore + bpfman_store::XdpStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement,
> std::error::Error for UnloadError<S, K>
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Some(cause) = self.primary() {
            return Some(cause);
        }
        self.report().and_then(|r| r.xdp.cause()).map(|e| e as _)
    }
}
