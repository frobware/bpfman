//! Ordered detach with family selection and teardown in one writer scope.
use crate::{
    Bpfman, Cancellation, DetachError, DetachOutcome, DetachReport, LinkCause, LinkErrorKind,
    link_error::Cause,
};
use bpfman_fs::RuntimeWriter;
use bpfman_kernel::{TcLifecycle, TracepointLinks, XdpReplacement};
use bpfman_lock::AcquireOptions;
use bpfman_model::LinkDetails;
use bpfman_store::{LinkReader, LinkStore, OpenStore, TcStore, XdpReplacementStore, XdpStore};
use std::{fmt, num::NonZeroU64};

impl<S, K> Bpfman<S, K>
where
    S: LinkStore + XdpReplacementStore + TcStore,
    S::Reader: LinkReader,
    K: TracepointLinks + XdpReplacement + TcLifecycle,
{
    /// Attempt all link IDs in order under one writer lock, including duplicates.
    /// Individual failures retain original cleanup ownership and do not stop later IDs.
    pub fn detach_many(
        &self,
        ids: &[NonZeroU64],
    ) -> Result<Vec<DetachOutcome<S, K>>, crate::Error> {
        self.detach_many_with_cancellation(ids, &Cancellation::new())
    }

    /// Select each attachment family from fresh intent under the same writer as
    /// teardown. Cancellation prevents a new destructive pass; an admitted pass
    /// finishes. Lock admission fails before any operand starts.
    #[tracing::instrument(name = "link.detach_many", level = "debug", skip_all, fields(count = ids.len()), err)]
    pub fn detach_many_with_cancellation(
        &self,
        ids: &[NonZeroU64],
        cancellation: &Cancellation,
    ) -> Result<Vec<DetachOutcome<S, K>>, crate::Error> {
        self.store
            .runtime()
            .with_writer(
                AcquireOptions {
                    timeout: self.lock_timeout,
                    cancelled: Some(cancellation.flag()),
                },
                |writer| {
                    ids.iter()
                        .map(|&id| DetachOutcome {
                            id,
                            outcome: self.detach_under_writer(&writer, id, cancellation),
                        })
                        .collect()
                },
            )
            .map_err(crate::error::filesystem_error)
    }

    pub(super) fn detach_under_writer(
        &self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU64,
        cancellation: &Cancellation,
    ) -> Result<DetachReport<S, K>, DetachError<S, K>> {
        let observe = || -> Result<_, LinkCause> {
            if cancellation.is_cancelled() {
                return Err(Cause::Cancelled.into());
            }
            let record = self
                .store
                .open(writer)?
                .read_links()?
                .into_iter()
                .find(|r| r.id == id);
            // A cancellation during observation must not be mistaken for absence.
            if cancellation.is_cancelled() {
                return Err(Cause::Cancelled.into());
            }
            record.ok_or_else(|| Cause::LinkNotFound(id).into())
        };
        match observe().map_err(DetachError::Observation)?.details {
            LinkDetails::Xdp(_) => self
                .detach_xdp_cancellable_locked(writer, id, cancellation)
                .map(DetachReport::Xdp)
                .map_err(DetachError::Xdp),
            LinkDetails::Tc(_) => self
                .detach_tc_locked(writer, id, cancellation)
                .map(DetachReport::Tc)
                .map_err(DetachError::Tc),
            _ => self
                .detach_standalone_locked(writer, id, cancellation)
                .map(DetachReport::Standalone)
                .map_err(DetachError::Standalone),
        }
    }
}

impl<S, K> DetachError<S, K>
where
    S: LinkStore + XdpStore + TcStore,
    K: TracepointLinks + XdpReplacement + TcLifecycle,
{
    /// Stable failure category, including cancellation before an operand starts.
    pub fn kind(&self) -> LinkErrorKind {
        match self {
            Self::Observation(cause) => cause.kind(),
            Self::Standalone(error) => error.kind(),
            Self::Xdp(error) => error.kind(),
            Self::Tc(error) => error.kind(),
        }
    }

    /// True only for an absent managed link during initial selection. Missing
    /// programs, dispatcher evidence, pins and failed cleanup are never suppressed.
    pub fn is_link_missing(&self) -> bool {
        matches!(
            self,
            Self::Observation(LinkCause {
                cause: Cause::LinkNotFound(_)
            })
        )
    }
}

impl<S, K> fmt::Debug for DetachError<S, K>
where
    S: LinkStore + XdpStore + TcStore,
    K: TracepointLinks + XdpReplacement + TcLifecycle,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Observation(cause) => f.debug_tuple("Observation").field(cause).finish(),
            Self::Standalone(error) => f.debug_tuple("Standalone").field(error).finish(),
            Self::Xdp(error) => f.debug_tuple("Xdp").field(error).finish(),
            Self::Tc(error) => f.debug_tuple("Tc").field(error).finish(),
        }
    }
}

impl<S, K> fmt::Display for DetachError<S, K>
where
    S: LinkStore + XdpStore + TcStore,
    K: TracepointLinks + XdpReplacement + TcLifecycle,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("link detach failed")
    }
}

impl<S, K> std::error::Error for DetachError<S, K>
where
    S: LinkStore + XdpStore + TcStore + 'static,
    K: TracepointLinks + XdpReplacement + TcLifecycle + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::Observation(cause) => cause,
            Self::Standalone(error) => error,
            Self::Xdp(error) => error,
            Self::Tc(error) => error,
        })
    }
}

impl<S, K> fmt::Debug for DetachReport<S, K>
where
    S: LinkStore + XdpStore + TcStore,
    K: TracepointLinks + XdpReplacement + TcLifecycle,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Standalone(report) => f.debug_tuple("Standalone").field(report).finish(),
            Self::Xdp(report) => f.debug_tuple("Xdp").field(report).finish(),
            Self::Tc(report) => f.debug_tuple("Tc").field(report).finish(),
        }
    }
}
