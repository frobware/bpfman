//! Dispatcher prerequisites for program teardown, sharing its writer and retry budget.
use crate::{
    Bpfman, LinkCause, UnloadCause, UnloadError, UnloadReport, XdpError, XdpReport, unload,
    unload_error::{Cause, Failure},
};
use bpfman_fs::{RuntimeIdentity, RuntimeWriter};
use bpfman_kernel::{ProgramResources, TracepointLinks, XdpReplacement};
use bpfman_model::{LinkDetails, StoredLink};
use bpfman_store::{LinkReader, LinkStore, OpenStore, UnloadStore, XdpReplacementStore, XdpStore};
use std::{
    collections::VecDeque,
    num::{NonZeroU32, NonZeroU64},
};

/// One XDP member's detach or recovery outcome during program unload.
/// Explicit recovery updates the same entry and retains its nested attempt history.
pub struct UnloadXdpAttempt<S: XdpStore, K: XdpReplacement> {
    link_id: NonZeroU64,
    outcome: Result<XdpReport<S, K>, XdpError<S, K>>,
}

impl<S: XdpStore, K: XdpReplacement> UnloadXdpAttempt<S, K> {
    /// Managed link selected for detach.
    pub fn link_id(&self) -> NonZeroU64 {
        self.link_id
    }

    /// Actual detach/recovery result, including publication and restoration evidence.
    pub fn outcome(&self) -> Result<&XdpReport<S, K>, &XdpError<S, K>> {
        self.outcome.as_ref()
    }
}

struct Pending<K: XdpReplacement> {
    record: StoredLink,
    prepared: K::PreparedXdp,
}

pub(super) struct Progress<S: XdpStore, K: XdpReplacement> {
    root: RuntimeIdentity,
    program_id: NonZeroU32,
    pending: VecDeque<Pending<K>>,
    attempts: Vec<UnloadXdpAttempt<S, K>>,
}

impl<S: XdpStore, K: XdpReplacement> Progress<S, K> {
    pub(super) fn unresolved(&self) -> usize {
        self.pending.len()
            + self
                .attempts
                .iter()
                .filter_map(|a| a.outcome.as_ref().err())
                .map(XdpError::unresolved)
                .sum::<usize>()
    }

    pub(super) fn attempts(&self) -> &[UnloadXdpAttempt<S, K>] {
        &self.attempts
    }

    pub(super) fn cause(&self) -> Option<&LinkCause> {
        if self.pending.is_empty() {
            return None;
        }
        self.attempts.last().and_then(|a| match &a.outcome {
            Err(e) => e.cause(),
            Ok(r) => r.primary_failure(),
        })
    }
}

pub(super) fn observe<S: XdpReplacementStore, K: XdpReplacement>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    id: NonZeroU32,
) -> Result<Progress<S, K>, UnloadCause>
where
    S::Reader: LinkReader,
{
    let root = w.identity()?;
    let mut pending = VecDeque::new();
    for record in app
        .store
        .open(w)?
        .read_links()?
        .into_iter()
        .filter(|r| r.program_id == id)
    {
        let LinkDetails::Xdp(details) = &record.details else {
            continue;
        };
        let (snapshot, _) = app
            .store
            .observe_xdp_dispatcher(w, details.key)?
            .ok_or(Cause::Invalid("missing XDP dispatcher"))?;
        if !snapshot.members().iter().any(|m| m.member == record) {
            return Err(Cause::Invalid("XDP member changed during unload preflight").into());
        }
        // Preflight every involved dispatcher before admitting destructive teardown.
        app.kernel.observe_dispatcher(w, &snapshot)?;
        let (key, prepared) = app
            .kernel
            .prepare_xdp(w, id, &details.interface, &details.netns)?;
        if key != details.key {
            return Err(Cause::Invalid("XDP interface changed during unload preflight").into());
        }
        pending.push_back(Pending { record, prepared });
    }
    Ok(Progress {
        root,
        program_id: id,
        pending,
        attempts: Vec::new(),
    })
}

// Revision, slot, extension ID, and pin location change during preceding detaches.
// The logical link identity and operator fields must still match the admitted request.
fn same_member(a: &StoredLink, b: &StoredLink) -> bool {
    let (LinkDetails::Xdp(a_details), LinkDetails::Xdp(b_details)) = (&a.details, &b.details)
    else {
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

pub(super) fn resume<S, K>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    mut report: UnloadReport<S, K>,
) -> Result<UnloadReport<S, K>, UnloadError<S, K>>
where
    S: XdpReplacementStore + UnloadStore + LinkStore,
    S::Reader: LinkReader,
    K: XdpReplacement + ProgramResources + TracepointLinks,
{
    if let Err(cause) = advance(app, w, &mut report.xdp) {
        return Err(UnloadError {
            failure: Failure::RetryBlocked {
                cause,
                report: Box::new(report),
            },
        });
    }
    if !report.xdp.pending.is_empty() {
        return Err(UnloadError {
            failure: Failure::Incomplete(Box::new(report)),
        });
    }
    // A later writer may have attached another member while recovery was retained.
    // Refuse program teardown until that new prerequisite is handled explicitly.
    if !report.xdp.attempts.is_empty() {
        let checked = (|| -> Result<(), UnloadCause> {
            if app
                .store
                .open(w)?
                .read_links()?
                .iter()
                .any(|l| l.program_id == report.xdp.program_id)
            {
                return Err(Cause::Invalid(
                    "program acquired new links during XDP unload recovery",
                )
                .into());
            }
            Ok(())
        })();
        if let Err(cause) = checked {
            return Err(UnloadError {
                failure: Failure::RetryBlocked {
                    cause,
                    report: Box::new(report),
                },
            });
        }
    }
    unload::finish(
        unload::drain(
            w,
            &mut unload::real::Effects(&app.store, &app.kernel),
            report.report.retry(),
        ),
        report.xdp,
    )
}

fn advance<S: XdpReplacementStore, K: XdpReplacement>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    progress: &mut Progress<S, K>,
) -> Result<(), UnloadCause>
where
    S::Reader: LinkReader,
{
    if progress.pending.is_empty() && progress.attempts.is_empty() {
        return Ok(());
    }
    if w.identity()? != progress.root {
        return Err(Cause::Invalid("XDP unload belongs to another runtime").into());
    }
    while let Some(pending) = progress.pending.front() {
        let id = pending.record.id;
        // This retained evidence rejects a foreign kernel instance or replaced program pin.
        app.kernel.validate_prepared_xdp(w, &pending.prepared)?;
        let recovering = progress.attempts.last().is_some_and(|a| {
            a.link_id == id && a.outcome.as_ref().is_err_and(|e| e.unresolved() != 0)
        });
        let outcome = if recovering {
            let Some(UnloadXdpAttempt {
                outcome: Err(error),
                ..
            }) = progress.attempts.pop()
            else {
                return Err(Cause::Invalid("missing XDP recovery evidence").into());
            };
            app.retry_xdp_locked(w, error)
        } else {
            let current = app
                .store
                .open(w)?
                .read_links()?
                .into_iter()
                .find(|r| r.id == id)
                .ok_or(Cause::Invalid(
                    "XDP unload member disappeared before detach",
                ))?;
            if !same_member(&current, &pending.record) {
                return Err(Cause::Invalid("XDP unload member changed since admission").into());
            }
            app.detach_xdp_locked(w, id)
        };
        let detached = outcome
            .as_ref()
            .is_ok_and(|r| r.primary_failure().is_none() || r.committed_snapshot().is_some());
        progress.attempts.push(UnloadXdpAttempt {
            link_id: id,
            outcome,
        });
        if !detached {
            // Successful rollback is recovery, not permission to retry the failed
            // forward detach inline. A separate caller-budgeted pass may resume it.
            return Ok(());
        }
        progress.pending.pop_front();
    }
    Ok(())
}
