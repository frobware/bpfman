//! Adopt every TC prerequisite before admitting destructive program teardown.
use crate::{Bpfman, LinkCause, TcError, TcReport, UnloadCause, tc::Teardown, unload_error::Cause};
use bpfman_fs::{RuntimeIdentity, RuntimeWriter};
use bpfman_kernel::TcLifecycle;
use bpfman_model::LinkDetails;
use bpfman_store::{LinkReader, OpenStore, TcStore};
use std::num::{NonZeroU32, NonZeroU64};

/// One TC link's consuming detach result, including all explicit cleanup retries.
pub struct UnloadTcAttempt<S: TcStore, K: TcLifecycle> {
    link_id: NonZeroU64,
    outcome: Result<TcReport<S, K>, TcError<S, K>>,
}

impl<S: TcStore, K: TcLifecycle> UnloadTcAttempt<S, K> {
    /// Managed link selected during unload admission.
    pub fn link_id(&self) -> NonZeroU64 {
        self.link_id
    }

    /// Filter, dispatcher and record cleanup with retained attempt history.
    pub fn outcome(&self) -> Result<&TcReport<S, K>, &TcError<S, K>> {
        self.outcome.as_ref()
    }
}

pub(super) struct Progress<S: TcStore, K: TcLifecycle> {
    root: RuntimeIdentity,
    pending: Vec<(NonZeroU64, Teardown<S, K>)>,
    attempts: Vec<UnloadTcAttempt<S, K>>,
}

impl<S: TcStore, K: TcLifecycle> Progress<S, K> {
    pub(super) fn unresolved(&self) -> usize {
        self.pending
            .iter()
            .map(|(_, t)| t.unresolved())
            .sum::<usize>()
            + self
                .attempts
                .iter()
                .filter_map(|a| a.outcome.as_ref().err())
                .map(TcError::unresolved)
                .sum::<usize>()
    }

    pub(super) fn attempts(&self) -> &[UnloadTcAttempt<S, K>] {
        &self.attempts
    }

    pub(super) fn cause(&self) -> Option<&LinkCause> {
        self.attempts
            .iter()
            .filter_map(|a| a.outcome.as_ref().err())
            .find_map(TcError::primary)
    }

    pub(super) fn advance(
        &mut self,
        app: &Bpfman<S, K>,
        w: &RuntimeWriter<'_>,
    ) -> Result<(), UnloadCause> {
        if self.pending.is_empty() && self.attempts.is_empty() {
            return Ok(());
        }
        if w.identity()? != self.root {
            return Err(Cause::Invalid("TC unload belongs to another runtime").into());
        }
        // Each independent link gets one pass. Failure within one link retains
        // its dependent stage/record and blocks all managed program teardown.
        self.attempts = std::mem::take(&mut self.attempts)
            .into_iter()
            .map(|attempt| UnloadTcAttempt {
                link_id: attempt.link_id,
                outcome: match attempt.outcome {
                    Ok(report) => Ok(report),
                    Err(error) => app.retry_tc_locked(w, error),
                },
            })
            .collect();
        for (link_id, teardown) in self.pending.drain(..) {
            self.attempts.push(UnloadTcAttempt {
                link_id,
                outcome: teardown.run(app, w),
            });
        }
        Ok(())
    }
}

pub(super) fn observe<S: TcStore, K: TcLifecycle>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    id: NonZeroU32,
) -> Result<Progress<S, K>, UnloadCause>
where
    S::Reader: LinkReader,
{
    let mut records = app.store.open(w)?.read_links()?;
    records.retain(|r| r.program_id == id && matches!(r.details, LinkDetails::Tc(_)));
    records.sort_by_key(|r| r.id);
    let mut pending = Vec::new();
    for record in records {
        // Observe every exact filter, pin, namespace and clsact ownership receipt
        // before any detach. A bad later attachment leaves all earlier ones live.
        let (snapshot, teardown) = app.observe_tc_locked(w, record.id).map_err(|cause| {
            // Preserve the portable error category and its source chain.
            UnloadCause::from(Cause::Link(cause))
        })?;
        if snapshot != record {
            return Err(Cause::Invalid("TC member changed during unload preflight").into());
        }
        pending.push((record.id, teardown));
    }
    Ok(Progress {
        root: w.identity()?,
        pending,
        attempts: Vec::new(),
    })
}
