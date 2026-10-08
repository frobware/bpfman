//! Preserve the application category before erasing diagnostic causes into anyhow.

use std::fmt;

#[derive(Debug)]
pub(crate) struct Error {
    cause: anyhow::Error,
    cancelled: bool,
}

impl Error {
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.cause, f)
    }
}

impl From<anyhow::Error> for Error {
    fn from(cause: anyhow::Error) -> Self {
        Self {
            cause,
            cancelled: false,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(cause: std::io::Error) -> Self {
        anyhow::Error::from(cause).into()
    }
}

impl From<bpfman_runtime::Error> for Error {
    fn from(cause: bpfman_runtime::Error) -> Self {
        let cancelled = cause.kind() == bpfman_runtime::ErrorKind::Cancelled;
        Self {
            cause: cause.into(),
            cancelled,
        }
    }
}

impl From<bpfman_runtime::LoadError<bpfman_kernel_aya::Kernel>> for Error {
    fn from(cause: bpfman_runtime::LoadError<bpfman_kernel_aya::Kernel>) -> Self {
        let cancelled = cause.kind() == bpfman_runtime::LoadErrorKind::Cancelled;
        Self {
            cause: cause.into(),
            cancelled,
        }
    }
}

impl From<bpfman_runtime::ObservationError> for Error {
    fn from(cause: bpfman_runtime::ObservationError) -> Self {
        let cancelled = cause.kind() == bpfman_runtime::ObservationErrorKind::Cancelled;
        Self {
            cause: cause.into(),
            cancelled,
        }
    }
}

impl<
    S: bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpStore
        + bpfman_store::TcStore
        + 'static,
> From<bpfman_runtime::UnloadError<S, bpfman_kernel_aya::Kernel>> for Error
{
    fn from(cause: bpfman_runtime::UnloadError<S, bpfman_kernel_aya::Kernel>) -> Self {
        let cancelled = cause.kind() == bpfman_runtime::UnloadErrorKind::Cancelled;
        Self {
            cause: cause.into(),
            cancelled,
        }
    }
}

impl From<bpfman_runtime::LinkCause> for Error {
    fn from(cause: bpfman_runtime::LinkCause) -> Self {
        let cancelled = cause.kind() == bpfman_runtime::LinkErrorKind::Cancelled;
        Self {
            cause: cause.into(),
            cancelled,
        }
    }
}

impl<S: bpfman_store::LinkStore + 'static>
    From<bpfman_runtime::LinkError<S, bpfman_kernel_aya::Kernel>> for Error
{
    fn from(cause: bpfman_runtime::LinkError<S, bpfman_kernel_aya::Kernel>) -> Self {
        let cancelled = cause.kind() == bpfman_runtime::LinkErrorKind::Cancelled;
        Self {
            cause: cause.into(),
            cancelled,
        }
    }
}

impl<S: bpfman_store::XdpStore + bpfman_store::TcStore + 'static>
    From<bpfman_runtime::XdpError<S, bpfman_kernel_aya::Kernel>> for Error
{
    fn from(cause: bpfman_runtime::XdpError<S, bpfman_kernel_aya::Kernel>) -> Self {
        let cancelled = cause.kind() == bpfman_runtime::LinkErrorKind::Cancelled;
        Self {
            cause: cause.into(),
            cancelled,
        }
    }
}

impl<S: bpfman_store::TcStore + 'static> From<bpfman_runtime::TcError<S, bpfman_kernel_aya::Kernel>>
    for Error
{
    fn from(cause: bpfman_runtime::TcError<S, bpfman_kernel_aya::Kernel>) -> Self {
        let cancelled = cause.kind() == bpfman_runtime::LinkErrorKind::Cancelled;
        Self {
            cause: cause.into(),
            cancelled,
        }
    }
}
