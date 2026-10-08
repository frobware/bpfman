use crate::{
    Bpfman, LoadError, LoadErrorKind,
    compensation::compensate_load,
    load::{CleanupEffects, Effects, FailureFor},
};
use bpfman_core::{CompensationKind, LoadFailure, LoadRollback};
use bpfman_fs::{Bytecode, RuntimeWriter};
use std::fmt;

#[derive(Debug, thiserror::Error)]
pub(super) enum LoadCause {
    #[error("load cancelled before commit")]
    Cancelled,
    #[error("batch aborted before commit")]
    BatchAborted,
    #[error(
        "program {id} was committed but its result could not be observed; batch programs {committed:?} remain loaded"
    )]
    Observation {
        id: std::num::NonZeroU32,
        committed: Vec<std::num::NonZeroU32>,
        #[source]
        source: crate::ObservationError,
    },
    #[error("read local ELF")]
    Read(#[source] std::io::Error),
    #[error(transparent)]
    Kernel(#[from] bpfman_kernel::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("{0} execution is not implemented in this Rust load slice")]
    Unsupported(&'static str),
    #[error(transparent)]
    Filesystem(#[from] bpfman_fs::Error),
    #[error(transparent)]
    Store(#[from] bpfman_store::Error),
    #[error("open program store")]
    Open(#[source] crate::Error),
    #[error("encode bytecode provenance")]
    Json(#[source] serde_json::Error),
}

pub(super) enum Failure<P, M, B, D, E> {
    NoOwnedArtifacts(E),
    Batch {
        primary: Box<Self>,
        previous: Vec<Self>,
    },
    Compensated {
        report: Box<LoadFailure<P, M, B, E>>,
        directory: Option<D>,
        directory_attempts: Vec<Result<(), E>>,
    },
}

pub(super) type KernelFailure<K> = Failure<
    <K as bpfman_kernel::ProgramResources>::ProgramPin,
    <K as bpfman_kernel::ProgramResources>::MapPin,
    Bytecode,
    <K as bpfman_kernel::ProgramResources>::MapDirectory,
    LoadCause,
>;

impl<K: bpfman_kernel::ProgramResources> From<LoadCause> for LoadError<K> {
    fn from(cause: LoadCause) -> Self {
        Self {
            failure: Box::new(Failure::NoOwnedArtifacts(cause)),
            retry_lock_error: None,
        }
    }
}

impl<K: bpfman_kernel::ProgramResources> LoadError<K> {
    /// Application failure category; backend errors are available only as causes.
    pub fn kind(&self) -> LoadErrorKind {
        let cause = self.failure.primary();

        match cause {
            LoadCause::Cancelled => LoadErrorKind::Cancelled,
            LoadCause::Open(e) if e.kind() == crate::ErrorKind::Cancelled => {
                LoadErrorKind::Cancelled
            }
            LoadCause::Filesystem(e) if e.kind() == bpfman_fs::ErrorKind::Cancelled => {
                LoadErrorKind::Cancelled
            }
            LoadCause::Invalid(_) => LoadErrorKind::InvalidInput,
            LoadCause::Unsupported(_) => LoadErrorKind::Unsupported,
            LoadCause::Kernel(e) => match e.kind() {
                bpfman_kernel::ErrorKind::InvalidInput => LoadErrorKind::InvalidInput,
                bpfman_kernel::ErrorKind::Unsupported => LoadErrorKind::Unsupported,
                _ => LoadErrorKind::Unavailable,
            },
            _ => LoadErrorKind::Unavailable,
        }
    }

    /// Number of owned resources still requiring cleanup (including a blocked map directory).
    pub fn unresolved(&self) -> usize {
        self.failure.unresolved()
    }

    /// Failure to acquire writer authority for the most recent explicit cleanup pass.
    /// The original load failure and all unresolved receipts remain available.
    pub fn retry_lock_error(&self) -> Option<&crate::Error> {
        self.retry_lock_error.as_ref()
    }

    /// Explicitly retry unresolved cleanup once, retaining the original error
    /// and all prior attempts. The supplied writer must belong to the same root.
    fn retry_cleanup(self, writer: &RuntimeWriter<'_>, kernel: &K) -> Self {
        Self {
            failure: Box::new(retry(writer, &mut Effects(&(), kernel), *self.failure)),
            retry_lock_error: None,
        }
    }
}

impl<S: bpfman_store::OpenStore, K: bpfman_kernel::ProgramResources> Bpfman<S, K> {
    /// Retry unresolved load cleanup once under this instance's writer lock.
    /// The original failure remains the result, including after complete cleanup.
    /// Acquisition failure retains all receipts and is exposed by `retry_lock_error`.
    pub fn retry_load_cleanup(&self, error: LoadError<K>) -> LoadError<K> {
        self.retry_load_cleanup_with_cancellation(error, &crate::Cancellation::new())
    }

    /// Cancel admission to an explicit cleanup pass without losing receipts.
    /// Once admitted, every eligible cleanup instruction is attempted once.
    #[tracing::instrument(name = "program.retry_load_cleanup", level = "debug", skip_all)]
    pub fn retry_load_cleanup_with_cancellation(
        &self,
        error: LoadError<K>,
        cancellation: &crate::Cancellation,
    ) -> LoadError<K> {
        if error.unresolved() == 0 {
            return error;
        }

        let mut pending = Some(error);
        let acquired = self.store.runtime().with_writer(
            bpfman_lock::AcquireOptions {
                timeout: self.lock_timeout,
                cancelled: Some(cancellation.flag()),
            },
            |writer| {
                if let Some(error) = pending.take() {
                    pending = Some(error.retry_cleanup(&writer, &self.kernel));
                }
            },
        );

        match pending {
            Some(mut error) => {
                if let Err(cause) = acquired {
                    error.retry_lock_error = Some(crate::error::filesystem_error(cause));
                }
                error
            }
            None => LoadCause::Invalid("cleanup callback lost its retained failure".into()).into(),
        }
    }
}

pub(super) fn retry<F: CleanupEffects>(
    writer: &RuntimeWriter<'_>,
    effects: &mut F,
    failure: FailureFor<F>,
) -> FailureFor<F> {
    match failure {
        Failure::NoOwnedArtifacts(_) => failure,
        Failure::Batch { primary, previous } => Failure::Batch {
            primary: Box::new(retry(writer, effects, *primary)),
            previous: previous
                .into_iter()
                .map(|f| retry(writer, effects, f))
                .collect(),
        },
        Failure::Compensated {
            report,
            directory,
            directory_attempts,
        } => finish(
            writer,
            effects,
            report.retry(),
            directory,
            directory_attempts,
        ),
    }
}

pub(super) fn finish<F: CleanupEffects>(
    writer: &RuntimeWriter<'_>,
    effects: &mut F,
    rollback: LoadRollback<F::ProgramPin, F::MapPin, F::Bytecode, F::Error>,
    mut directory: Option<F::MapDirectory>,
    mut directory_attempts: Vec<Result<(), F::Error>>,
) -> FailureFor<F> {
    let report = compensate_load(writer, effects, rollback);

    // Explicit prerequisite: no unresolved map-pin instructions. A blocked
    // container is retained without claiming that its removal was attempted.
    let maps_pending = report
        .remaining()
        .iter()
        .any(|p| p.instruction().kind() == CompensationKind::MapPin);

    if !maps_pending {
        if let Some(owned) = directory.take() {
            let outcome = match effects.remove_map_directory(writer, owned) {
                Ok(()) => Ok(()),
                Err(failure) => {
                    directory = Some(failure.remaining);

                    Err(failure.cause)
                }
            };
            directory_attempts.push(outcome);
        }
    }

    Failure::Compensated {
        report: Box::new(report),
        directory,
        directory_attempts,
    }
}

impl<K: bpfman_kernel::ProgramResources> fmt::Debug for LoadError<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl<K: bpfman_kernel::ProgramResources> fmt::Display for LoadError<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "load local program")?;

        if let Some(error) = &self.retry_lock_error {
            write!(f, "; cleanup lock: ")?;
            write_chain(f, error)?;
        }

        self.failure.fmt_cleanup(f)?;

        Ok(())
    }
}

impl<K: bpfman_kernel::ProgramResources> std::error::Error for LoadError<K> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.failure.primary())
    }
}

fn write_chain(f: &mut fmt::Formatter<'_>, cause: &dyn std::error::Error) -> fmt::Result {
    write!(f, "{cause}")?;
    let mut source = cause.source();

    while let Some(error) = source {
        write!(f, ": {error}")?;
        source = error.source();
    }

    Ok(())
}

impl<P, M, B, D, E> Failure<P, M, B, D, E> {
    pub(super) fn primary(&self) -> &E {
        match self {
            Self::NoOwnedArtifacts(cause) => cause,
            Self::Compensated { report, .. } => report.primary(),
            Self::Batch { primary, .. } => primary.primary(),
        }
    }

    fn unresolved(&self) -> usize {
        match self {
            Self::NoOwnedArtifacts(_) => 0,
            Self::Compensated {
                report, directory, ..
            } => report.remaining().len() + usize::from(directory.is_some()),
            Self::Batch { primary, previous } => {
                primary.unresolved() + previous.iter().map(Self::unresolved).sum::<usize>()
            }
        }
    }
}

impl<P, M, B, D> Failure<P, M, B, D, LoadCause> {
    fn fmt_cleanup(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Self::Batch { primary, previous } = self {
            primary.fmt_cleanup(f)?;
            for member in previous {
                member.fmt_cleanup(f)?;
            }
        }
        if let Self::Compensated {
            report,
            directory_attempts,
            ..
        } = self
        {
            write!(f, " ({} unresolved cleanup resources)", self.unresolved())?;

            for attempt in report.attempts() {
                if let Err(cause) = &attempt.outcome {
                    write!(f, "; cleanup {:?}: ", attempt.kind)?;
                    write_chain(f, cause)?;
                }
            }

            for cause in directory_attempts
                .iter()
                .filter_map(|result| result.as_ref().err())
            {
                write!(f, "; map-directory cleanup: ")?;
                write_chain(f, cause)?;
            }
        }

        Ok(())
    }
}
