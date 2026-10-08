//! Adopt every TC prerequisite before admitting destructive program teardown.
use crate::{Bpfman, LinkCause, TcError, TcReport, UnloadCause, tc::Teardown, unload_error::Cause};
use bpfman_fs::{RuntimeIdentity, RuntimeWriter};
use bpfman_kernel::TcLifecycle;
use bpfman_model::{LinkDetails, StoredLink};
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
    pending: Vec<(StoredLink, Option<Teardown<S, K>>)>,
    attempts: Vec<UnloadTcAttempt<S, K>>,
}

impl<S: TcStore, K: TcLifecycle> Progress<S, K> {
    pub(super) fn unresolved(&self) -> usize {
        self.pending
            .iter()
            .map(|(_, t)| t.as_ref().map_or(1, Teardown::unresolved))
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
            .or_else(|| {
                self.attempts
                    .iter()
                    .filter_map(|a| a.outcome.as_ref().ok())
                    .find_map(TcReport::primary_failure)
            })
    }

    pub(super) fn advance(
        &mut self,
        app: &Bpfman<S, K>,
        w: &RuntimeWriter<'_>,
    ) -> Result<(), UnloadCause>
    where
        S::Reader: LinkReader,
    {
        if self.pending.is_empty() && self.attempts.is_empty() {
            return Ok(());
        }
        if w.identity()? != self.root {
            return Err(Cause::Invalid("TC unload belongs to another runtime").into());
        }
        // Independent interfaces still get one pass each after a failure. Members
        // of one dispatcher reobserve its latest revision after preceding detaches.
        let mut retained = Vec::new();
        let mut blocked = std::collections::BTreeSet::new();
        for (record, teardown) in self.pending.drain(..) {
            let LinkDetails::Tc(details) = &record.details else {
                return Err(Cause::Invalid("non-TC unload member").into());
            };
            let key = details.key;
            if blocked.contains(&key) {
                retained.push((record, teardown));
                continue;
            }
            let index = self.attempts.iter().position(|a| a.link_id == record.id);
            let recovering = index.is_some_and(|i| {
                self.attempts[i]
                    .outcome
                    .as_ref()
                    .is_err_and(|e| e.unresolved() != 0)
            });
            let outcome = if recovering {
                let old = self.attempts.remove(index.unwrap_or(0));
                match old.outcome {
                    Err(error) => app.retry_tc_locked(w, error),
                    Ok(report) => Ok(report),
                }
            } else {
                let current = app
                    .store
                    .open(w)
                    .and_then(|mut reader| reader.read_links())
                    .map(|records| records.into_iter().find(|r| r.id == record.id));
                match current {
                    Ok(Some(current)) if same_member(&current, &record) => {
                        let plan = if current == record {
                            teardown.map(Ok).unwrap_or_else(|| {
                                app.observe_tc_locked(w, record.id).map(|(_, t)| t)
                            })
                        } else {
                            app.observe_tc_locked(w, record.id).map(|(_, t)| t)
                        };
                        match plan {
                            Ok(t) => t.run(app, w),
                            Err(cause) => Err(cause.into()),
                        }
                    }
                    Err(cause) => Err(LinkCause::from(cause).into()),
                    _ => Err(LinkCause::from(crate::link_error::Cause::Invalid(
                        "TC unload member changed since admission",
                    ))
                    .into()),
                }
            };
            let detached = outcome
                .as_ref()
                .is_ok_and(|r| r.primary_failure().is_none() || r.committed_snapshot().is_some());
            if let Some(index) = self.attempts.iter().position(|a| a.link_id == record.id) {
                self.attempts.remove(index);
            }
            self.attempts.push(UnloadTcAttempt {
                link_id: record.id,
                outcome,
            });
            if !detached {
                blocked.insert(key);
                retained.push((record, None));
            }
        }
        self.pending = retained;
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
        pending.push((record, Some(teardown)));
    }
    Ok(Progress {
        root: w.identity()?,
        pending,
        attempts: Vec::new(),
    })
}

fn same_member(a: &StoredLink, b: &StoredLink) -> bool {
    let (LinkDetails::Tc(a_details), LinkDetails::Tc(b_details)) = (&a.details, &b.details) else {
        return false;
    };
    a.id == b.id
        && a.program_id == b.program_id
        && a.metadata == b.metadata
        && a.created_at == b.created_at
        && a_details.key == b_details.key
        && a_details.interface == b_details.interface
        && a_details.netns == b_details.netns
        && a_details.priority == b_details.priority
        && a_details.proceed_on == b_details.proceed_on
}
