use crate::{DeleteError, DeleteErrorKind, ErrorKind, LinkErrorKind, UnloadErrorKind};
use std::fmt;

impl<S, K> DeleteError<S, K>
where
    S: bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpStore
        + bpfman_store::TcStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement
        + bpfman_kernel::TcLifecycle,
{
    /// Backend-independent category; original causes and receipts remain intact.
    pub fn kind(&self) -> DeleteErrorKind {
        match self {
            Self::Observation(e) => match e.kind() {
                ErrorKind::Cancelled => DeleteErrorKind::Cancelled,
                ErrorKind::InvalidState | ErrorKind::IncompatibleState => {
                    DeleteErrorKind::InvalidState
                }
                _ => DeleteErrorKind::Unavailable,
            },
            Self::BlockedProgram(_) | Self::BlockedLink(_) => DeleteErrorKind::InvalidState,
            Self::Link { error, .. } => match error.kind() {
                LinkErrorKind::Cancelled => DeleteErrorKind::Cancelled,
                LinkErrorKind::NotFound => DeleteErrorKind::NotFound,
                LinkErrorKind::Unsupported => DeleteErrorKind::Unsupported,
                LinkErrorKind::InvalidState => DeleteErrorKind::InvalidState,
                _ => DeleteErrorKind::Unavailable,
            },
            Self::Program { error, .. } => match error.kind() {
                UnloadErrorKind::Cancelled => DeleteErrorKind::Cancelled,
                UnloadErrorKind::NotFound => DeleteErrorKind::NotFound,
                UnloadErrorKind::Unsupported => DeleteErrorKind::Unsupported,
                UnloadErrorKind::InvalidState => DeleteErrorKind::InvalidState,
                UnloadErrorKind::Unavailable => DeleteErrorKind::Unavailable,
            },
        }
    }
}

impl<S, K> fmt::Display for DeleteError<S, K>
where
    S: bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpStore
        + bpfman_store::TcStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement
        + bpfman_kernel::TcLifecycle,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Observation(_) => f.write_str("observe cascading deletion"),
            Self::BlockedProgram(id) => write!(
                f,
                "program {id} is blocked by an earlier failed delete attempt; retain its cleanup receipt"
            ),
            Self::BlockedLink(id) => write!(
                f,
                "link {id} is blocked by an earlier failed delete attempt; retain its cleanup receipt"
            ),
            Self::Link { id, .. } => write!(f, "detach link {id}"),
            Self::Program { id, .. } => write!(f, "unload program {id}"),
        }
    }
}

impl<S, K> fmt::Debug for DeleteError<S, K>
where
    S: bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpStore
        + bpfman_store::TcStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement
        + bpfman_kernel::TcLifecycle,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Observation(e) => f.debug_tuple("Observation").field(e).finish(),
            Self::BlockedProgram(id) => f.debug_tuple("BlockedProgram").field(id).finish(),
            Self::BlockedLink(id) => f.debug_tuple("BlockedLink").field(id).finish(),
            Self::Link { id, error } => f
                .debug_struct("Link")
                .field("id", id)
                .field("error", error)
                .finish(),
            Self::Program { id, error } => f
                .debug_struct("Program")
                .field("id", id)
                .field("error", error)
                .finish(),
        }
    }
}

impl<S: 'static, K: 'static> std::error::Error for DeleteError<S, K>
where
    S: bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpStore
        + bpfman_store::TcStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement
        + bpfman_kernel::TcLifecycle,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Observation(e) => Some(e),
            Self::BlockedProgram(_) | Self::BlockedLink(_) => None,
            Self::Link { error, .. } => Some(error.as_ref()),
            Self::Program { error, .. } => Some(error),
        }
    }
}
