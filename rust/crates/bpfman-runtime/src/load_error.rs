use crate::{LoadError, LoadErrorKind, compensation::compensate_load, load::FilesystemCleanup};
use bpfman_core::{CompensationKind, LoadFailure, LoadRollback};
use bpfman_fs::{Bytecode, MapDirectory, MapPin, ProgramPin, RuntimeWriter};
use std::fmt;

pub(super) type Rollback = LoadRollback<ProgramPin, MapPin, Bytecode, LoadCause>;
pub(super) type Report = LoadFailure<ProgramPin, MapPin, Bytecode, LoadCause>;

#[derive(Debug, thiserror::Error)]
pub(super) enum LoadCause {
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
    Store(#[from] bpfman_store_sqlite::Error),
    #[error("open program store")]
    Open(#[source] crate::Error),
    #[error("encode bytecode provenance")]
    Json(#[source] serde_json::Error),
}

pub(super) enum Failure {
    BeforeEffects(LoadCause),
    Compensated {
        report: Report,
        directory: Option<MapDirectory>,
        directory_errors: Vec<LoadCause>,
    },
}

impl From<LoadCause> for LoadError {
    fn from(cause: LoadCause) -> Self {
        Self {
            failure: Box::new(Failure::BeforeEffects(cause)),
        }
    }
}

impl LoadError {
    /// Application failure category; backend errors are available only as causes.
    pub fn kind(&self) -> LoadErrorKind {
        let cause = match self.failure.as_ref() {
            Failure::BeforeEffects(c) => c,
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
            Failure::BeforeEffects(_) => 0,
            Failure::Compensated {
                report, directory, ..
            } => report.remaining().len() + usize::from(directory.is_some()),
        }
    }
    /// Explicitly retry unresolved cleanup once, retaining the original error
    /// and all prior attempts. The supplied writer must belong to the same root.
    pub fn retry_cleanup(self, writer: &RuntimeWriter<'_>) -> Self {
        match *self.failure {
            Failure::BeforeEffects(_) => self,
            Failure::Compensated {
                report,
                directory,
                directory_errors,
            } => finish(writer, report.retry(), directory, directory_errors),
        }
    }
}

pub(super) fn finish(
    writer: &RuntimeWriter<'_>,
    rollback: Rollback,
    mut directory: Option<MapDirectory>,
    mut directory_errors: Vec<LoadCause>,
) -> LoadError {
    let report = compensate_load(writer, &mut FilesystemCleanup, rollback);
    // Explicit prerequisite: no unresolved map-pin instructions. Container
    // cleanup is never an independent recursive-removal instruction.
    let maps_pending = report
        .remaining()
        .iter()
        .any(|p| p.instruction().kind() == CompensationKind::MapPin);
    if !maps_pending {
        if let Some(owned) = directory.take() {
            if let Err(failure) = writer.remove_empty_map_directory(owned) {
                directory = Some(failure.remaining);
                directory_errors.push(failure.cause.into());
            }
        }
    }
    LoadError {
        failure: Box::new(Failure::Compensated {
            report,
            directory,
            directory_errors,
        }),
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
        if let Failure::Compensated {
            report,
            directory_errors,
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
            for cause in directory_errors {
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
            Failure::BeforeEffects(c) => c,
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
