use crate::{LinkCause, LinkError, LinkErrorKind, LinkReport};
use bpfman_store::LinkStore;
use std::fmt;

#[derive(Debug, thiserror::Error)]
pub(super) enum Cause {
    #[error("link operation cancelled")]
    Cancelled,
    #[error("managed program or link not found")]
    NotFound,
    #[error("attachment is outside the supported slice")]
    Unsupported,
    #[error("XDP kernel operation")]
    Xdp(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("invalid attachment state: {0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Store(#[from] bpfman_store::Error),
    #[error(transparent)]
    Filesystem(#[from] bpfman_fs::Error),
    #[error(transparent)]
    Kernel(#[from] bpfman_kernel::Error),
}

pub(super) enum Failure<S: LinkStore> {
    Before(LinkCause),
    After(Box<LinkReport<S>>),
    Admission {
        cause: LinkCause,
        report: Box<LinkReport<S>>,
    },
}

impl From<Cause> for LinkCause {
    fn from(cause: Cause) -> Self {
        Self { cause }
    }
}

impl From<bpfman_fs::Error> for LinkCause {
    fn from(cause: bpfman_fs::Error) -> Self {
        Cause::Filesystem(cause).into()
    }
}

impl From<bpfman_store::Error> for LinkCause {
    fn from(cause: bpfman_store::Error) -> Self {
        Cause::Store(cause).into()
    }
}

impl From<bpfman_kernel::Error> for LinkCause {
    fn from(cause: bpfman_kernel::Error) -> Self {
        Cause::Kernel(cause).into()
    }
}

impl<S: LinkStore> From<LinkCause> for LinkError<S> {
    fn from(cause: LinkCause) -> Self {
        Self {
            failure: Failure::Before(cause),
        }
    }
}

impl LinkCause {
    /// Stable classification; adapter diagnostics remain in the source chain.
    pub fn kind(&self) -> LinkErrorKind {
        match &self.cause {
            Cause::Cancelled => LinkErrorKind::Cancelled,
            Cause::NotFound => LinkErrorKind::NotFound,
            Cause::Unsupported => LinkErrorKind::Unsupported,
            Cause::Xdp(_) => LinkErrorKind::Unavailable,
            Cause::Invalid(_) => LinkErrorKind::InvalidState,
            Cause::Kernel(cause) => match cause.kind() {
                bpfman_kernel::ErrorKind::InvalidData => LinkErrorKind::InvalidState,
                _ => LinkErrorKind::Unavailable,
            },
            Cause::Filesystem(cause) => match cause.kind() {
                bpfman_fs::ErrorKind::Cancelled => LinkErrorKind::Cancelled,
                bpfman_fs::ErrorKind::TimedOut => LinkErrorKind::TimedOut,
                bpfman_fs::ErrorKind::UnsafeLayout => LinkErrorKind::InvalidState,
                _ => LinkErrorKind::Unavailable,
            },
            Cause::Store(cause) => match cause.kind() {
                bpfman_store::ErrorKind::Unsupported => LinkErrorKind::Unsupported,
                bpfman_store::ErrorKind::InvalidData
                | bpfman_store::ErrorKind::IncompatibleState => LinkErrorKind::InvalidState,
                _ => LinkErrorKind::Unavailable,
            },
        }
    }
}

impl<S: LinkStore> LinkReport<S> {
    /// Managed handle identifying the record associated with this cleanup.
    pub fn link_id(&self) -> std::num::NonZeroU64 {
        self.id
    }

    /// Original forward failure, retained even after successful explicit cleanup.
    pub fn primary_failure(&self) -> Option<&LinkCause> {
        self.primary.as_ref()
    }

    /// Every actual cleanup attempt, including successful effects and prior passes.
    pub fn attempts(&self) -> &[bpfman_core::LinkAttempt<LinkCause>] {
        self.cleanup.attempts()
    }

    /// Number of unresolved effects, including blocked record deletion.
    pub fn unresolved(&self) -> usize {
        self.cleanup.unresolved()
    }

    fn cause(&self) -> Option<&LinkCause> {
        self.primary.as_ref().or_else(|| {
            self.attempts()
                .iter()
                .find_map(|attempt| attempt.outcome.as_ref().err())
        })
    }
}

impl<S: LinkStore> LinkError<S> {
    /// Application failure category, including a cancelled explicit retry admission.
    pub fn kind(&self) -> LinkErrorKind {
        self.cause()
            .map_or(LinkErrorKind::Unavailable, LinkCause::kind)
    }

    /// Cleanup progress; absent when the operation failed before owning intent.
    pub fn report(&self) -> Option<&LinkReport<S>> {
        match &self.failure {
            Failure::Before(_) => None,
            Failure::After(report) | Failure::Admission { report, .. } => Some(report),
        }
    }

    fn cause(&self) -> Option<&LinkCause> {
        match &self.failure {
            Failure::Before(cause) | Failure::Admission { cause, .. } => Some(cause),
            Failure::After(report) => report.cause(),
        }
    }
}

impl<S: LinkStore> fmt::Debug for LinkReport<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LinkReport")
            .field("link_id", &self.id)
            .field("primary", &self.primary)
            .field("attempts", &self.attempts())
            .field("unresolved", &self.unresolved())
            .finish()
    }
}

impl<S: LinkStore> fmt::Debug for LinkError<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LinkError")
            .field("cause", &self.cause())
            .field("report", &self.report())
            .finish()
    }
}

impl<S: LinkStore> fmt::Display for LinkError<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "link operation failed; {} unresolved cleanup effects",
            self.report().map_or(0, LinkReport::unresolved)
        )
    }
}

impl<S: LinkStore> std::error::Error for LinkError<S> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause().map(|cause| cause as _)
    }
}
