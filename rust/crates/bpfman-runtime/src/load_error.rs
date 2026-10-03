use crate::{
    Bpfman, LoadError, LoadErrorKind,
    compensation::compensate_load,
    load::{CleanupEffects, Effects, FailureFor},
};
use bpfman_core::{CompensationKind, LoadFailure, LoadRollback};
use bpfman_fs::{Bytecode, MapDirectory, MapPin, ProgramPin, RuntimeWriter};
use std::fmt;

#[derive(Debug, thiserror::Error)]
pub(super) enum LoadCause {
    #[error("program {id} was committed but its result could not be observed; it remains loaded")]
    Observation {
        id: std::num::NonZeroU32,
        #[source]
        source: crate::ObservationError,
    },
    #[error("read local ELF")]
    Read(#[source] std::io::Error),
    #[error("parse local ELF")]
    Parse(#[source] Box<aya_obj::ParseError>),
    #[error("load ELF maps and relocations")]
    Kernel(#[source] Box<aya::EbpfError>),
    #[error("load tracepoint program")]
    Program(#[source] Box<aya::programs::ProgramError>),
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

pub(super) enum Failure<P = ProgramPin, M = MapPin, B = Bytecode, D = MapDirectory, E = LoadCause> {
    NoOwnedArtifacts(E),
    Compensated {
        report: Box<LoadFailure<P, M, B, E>>,
        directory: Option<D>,
        directory_attempts: Vec<Result<(), E>>,
    },
}

impl From<LoadCause> for LoadError {
    fn from(cause: LoadCause) -> Self {
        Self {
            failure: Box::new(Failure::NoOwnedArtifacts(cause)),
            retry_lock_error: None,
        }
    }
}

impl LoadError {
    /// Application failure category; backend errors are available only as causes.
    pub fn kind(&self) -> LoadErrorKind {
        let cause = match self.failure.as_ref() {
            Failure::NoOwnedArtifacts(c) => c,
            Failure::Compensated { report, .. } => report.primary(),
        };

        match cause {
            LoadCause::Invalid(_) | LoadCause::Parse(_) => LoadErrorKind::InvalidInput,
            LoadCause::Unsupported(_) => LoadErrorKind::Unsupported,
            _ => LoadErrorKind::Unavailable,
        }
    }

    /// Number of owned resources still requiring cleanup (including a blocked map directory).
    pub fn unresolved(&self) -> usize {
        match self.failure.as_ref() {
            Failure::NoOwnedArtifacts(_) => 0,
            Failure::Compensated {
                report, directory, ..
            } => report.remaining().len() + usize::from(directory.is_some()),
        }
    }

    /// Failure to acquire writer authority for the most recent explicit cleanup pass.
    /// The original load failure and all unresolved receipts remain available.
    pub fn retry_lock_error(&self) -> Option<&crate::Error> {
        self.retry_lock_error.as_ref()
    }

    /// Explicitly retry unresolved cleanup once, retaining the original error
    /// and all prior attempts. The supplied writer must belong to the same root.
    fn retry_cleanup(self, writer: &RuntimeWriter<'_>) -> Self {
        Self {
            failure: Box::new(retry(writer, &mut Effects(&()), *self.failure)),
            retry_lock_error: None,
        }
    }
}

impl<S> Bpfman<S> {
    /// Retry unresolved load cleanup once under this instance's writer lock.
    /// The original failure remains the result, including after complete cleanup.
    /// Acquisition failure retains all receipts and is exposed by `retry_lock_error`.
    #[tracing::instrument(name = "program.retry_load_cleanup", level = "debug", skip_all)]
    pub fn retry_load_cleanup(&self, error: LoadError) -> LoadError {
        if error.unresolved() == 0 {
            return error;
        }

        let mut pending = Some(error);
        let acquired = self.store.runtime().with_writer(
            bpfman_lock::AcquireOptions {
                timeout: self.lock_timeout,
                cancelled: None,
            },
            |writer| {
                if let Some(error) = pending.take() {
                    pending = Some(error.retry_cleanup(&writer));
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

impl fmt::Debug for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "load local tracepoint")?;

        if let Some(error) = &self.retry_lock_error {
            write!(f, "; cleanup lock: ")?;
            write_chain(f, error)?;
        }

        if let Failure::Compensated {
            report,
            directory_attempts,
            ..
        } = self.failure.as_ref()
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

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self.failure.as_ref() {
            Failure::NoOwnedArtifacts(c) => c,
            Failure::Compensated { report, .. } => report.primary(),
        })
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
