//! TC ingress publication and consuming recovery under one writer lock.
use crate::{Bpfman, Cancellation, LinkCause, link_error::Cause};
use bpfman_core::{
    EffectFailure, XdpCleanup, XdpCleanupKind, XdpCleanupReport, XdpCleanupStep, XdpResource,
};
use bpfman_fs::RuntimeWriter;
use bpfman_kernel::TcLifecycle;
use bpfman_lock::AcquireOptions;
use bpfman_model::{InterfaceName, NetworkNamespace, ProgramSpec, StoredLink, TcLink, TcProceedOn};
use bpfman_store::{OpenStore, ProgramReader, TcCommit, TcStore};
use std::{
    collections::BTreeMap,
    fmt,
    num::{NonZeroU32, NonZeroU64},
};

/// Legacy TC ingress attachment through a complete dispatcher revision.
pub struct TcAttach {
    /// Managed classifier extension.
    pub program_id: NonZeroU32,
    /// Interface within the selected namespace.
    pub interface: InterfaceName,
    /// Namespace selection; empty selects the caller's namespace.
    pub netns: NetworkNamespace,
    /// Member priority bounded by i32::MAX.
    pub priority: u32,
    /// Signed TC return-code continuation mask.
    pub proceed_on: TcProceedOn,
    /// Operator labels, kept separate from qdisc ownership evidence.
    pub metadata: BTreeMap<String, String>,
}

mod replacement;

enum Resource<S: TcStore, K: TcLifecycle> {
    Filter(K::Filter),
    Stage(K::Stage),
    Record(S::TcReceipt),
}

impl<S: TcStore, K: TcLifecycle> XdpResource for Resource<S, K> {
    fn kind(&self) -> XdpCleanupKind {
        match self {
            Self::Filter(_) => XdpCleanupKind::Outer,
            Self::Stage(_) => XdpCleanupKind::Program,
            Self::Record(_) => XdpCleanupKind::Record,
        }
    }
}

/// Validated, unattempted detach ownership. Observation has no destructive effects.
pub(super) struct Teardown<S: TcStore, K: TcLifecycle>(Detach<S, K>);

enum Detach<S: TcStore, K: TcLifecycle> {
    Last(Vec<Resource<S, K>>),
    Replace(replacement::Removal<S, K>),
}

impl<S: TcStore, K: TcLifecycle> Teardown<S, K> {
    pub(super) fn run(
        self,
        app: &Bpfman<S, K>,
        w: &RuntimeWriter<'_>,
    ) -> Result<TcReport<S, K>, TcError<S, K>> {
        match self.0 {
            Detach::Last(resources) => finish(None, cleanup(app, w, XdpCleanup::new(resources))),
            Detach::Replace(removal) => replacement::remove(app, w, removal),
        }
    }

    pub(super) fn unresolved(&self) -> usize {
        match &self.0 {
            Detach::Last(resources) => resources.len(),
            Detach::Replace(_) => 1,
        }
    }
}

enum Recovery<S: TcStore, K: TcLifecycle> {
    Cleanup(Box<XdpCleanupReport<Resource<S, K>, LinkCause>>),
    Restore(Box<replacement::RestoreFailure<S, K>>),
}

/// TC failure retaining all unresolved ownership and every actual cleanup attempt.
pub struct TcError<S: TcStore, K: TcLifecycle> {
    cause: Option<LinkCause>,
    recovery: Option<Recovery<S, K>>,
    admission: Option<LinkCause>,
    restorations: Vec<Result<(), LinkCause>>,
    committed: Option<bpfman_model::TcDispatcherSnapshot>,
}

/// Completed TC cleanup, including original forward failure and retry history.
pub struct TcReport<S: TcStore, K: TcLifecycle> {
    cause: Option<LinkCause>,
    cleanup: XdpCleanupReport<Resource<S, K>, LinkCause>,
    restorations: Vec<Result<(), LinkCause>>,
    committed: Option<bpfman_model::TcDispatcherSnapshot>,
}

impl<S: TcStore, K: TcLifecycle> TcError<S, K> {
    pub(super) fn primary(&self) -> Option<&LinkCause> {
        self.admission
            .as_ref()
            .or(self.cause.as_ref())
            .or_else(|| {
                self.recovery.as_ref().and_then(|r| match r {
                    Recovery::Restore(f) => Some(f.primary()),
                    Recovery::Cleanup(_) => None,
                })
            })
            .or_else(|| {
                self.attempts()
                    .iter()
                    .find_map(|a| a.outcome.as_ref().err())
            })
    }

    /// Portable application category.
    pub fn kind(&self) -> crate::LinkErrorKind {
        self.primary()
            .map_or(crate::LinkErrorKind::Unavailable, LinkCause::kind)
    }

    /// Remaining dependent effects; blocked effects are retained without attempted cleanup.
    pub fn unresolved(&self) -> usize {
        match &self.recovery {
            Some(Recovery::Cleanup(r)) => r.unresolved(),
            Some(Recovery::Restore(_)) => 3,
            None => 0,
        }
    }

    /// Every actual restoration attempt; blocked pin removal is not an attempt.
    pub fn restoration_attempts(&self) -> &[Result<(), LinkCause>] {
        match &self.recovery {
            Some(Recovery::Restore(r)) => r.attempts(),
            _ => &self.restorations,
        }
    }

    /// Successful publication retained when only retirement failed.
    pub fn committed_snapshot(&self) -> Option<&bpfman_model::TcDispatcherSnapshot> {
        self.committed.as_ref()
    }

    /// Actual cleanup attempts across explicit retry passes.
    pub fn attempts(&self) -> &[bpfman_core::XdpAttempt<LinkCause>] {
        match &self.recovery {
            Some(Recovery::Cleanup(r)) => r.attempts(),
            _ => &[],
        }
    }
}

impl<S: TcStore, K: TcLifecycle> From<LinkCause> for TcError<S, K> {
    fn from(cause: LinkCause) -> Self {
        Self {
            cause: Some(cause),
            recovery: None,
            admission: None,
            restorations: Vec::new(),
            committed: None,
        }
    }
}

impl<S: TcStore, K: TcLifecycle> fmt::Display for TcError<S, K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.committed.is_some() {
            write!(
                f,
                "TC replacement committed; {} unresolved retirement effects",
                self.unresolved()
            )
        } else {
            write!(
                f,
                "TC ingress operation failed; {} unresolved recovery effects",
                self.unresolved()
            )
        }
    }
}

impl<S: TcStore, K: TcLifecycle> fmt::Debug for TcError<S, K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcError")
            .field("cause", &self.primary())
            .field("attempts", &self.attempts())
            .field("restorations", &self.restoration_attempts())
            .field("committed", &self.committed)
            .field("unresolved", &self.unresolved())
            .finish()
    }
}

impl<S: TcStore, K: TcLifecycle> std::error::Error for TcError<S, K> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.primary().map(|e| e as _)
    }
}

impl<S: TcStore, K: TcLifecycle> TcReport<S, K> {
    /// Successful publication retained across retirement recovery passes.
    pub fn committed_snapshot(&self) -> Option<&bpfman_model::TcDispatcherSnapshot> {
        self.committed.as_ref()
    }

    /// Every restoration attempt, including earlier failed passes.
    pub fn restoration_attempts(&self) -> &[Result<(), LinkCause>] {
        &self.restorations
    }

    /// Remaining cleanup after this completed pass.
    pub fn unresolved(&self) -> usize {
        self.cleanup.unresolved()
    }

    /// Every cleanup attempt, including earlier failures.
    pub fn attempts(&self) -> &[bpfman_core::XdpAttempt<LinkCause>] {
        self.cleanup.attempts()
    }

    /// Original acquisition/publication failure after recovery completes.
    pub fn primary_failure(&self) -> Option<&LinkCause> {
        self.cause.as_ref()
    }
}

impl<S: TcStore, K: TcLifecycle> fmt::Debug for TcReport<S, K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcReport")
            .field("attempts", &self.attempts())
            .field("restorations", &self.restoration_attempts())
            .field("committed", &self.committed)
            .field("cause", &self.cause)
            .finish()
    }
}

fn check(c: &Cancellation) -> Result<(), LinkCause> {
    c.check().map_err(|_| Cause::Cancelled.into())
}

fn cleanup<S: TcStore, K: TcLifecycle>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    mut state: XdpCleanup<Resource<S, K>, LinkCause>,
) -> XdpCleanupReport<Resource<S, K>, LinkCause> {
    loop {
        state = match state.next() {
            XdpCleanupStep::Complete(report) => return report,
            XdpCleanupStep::Effect { receipt, next } => {
                let outcome = match receipt {
                    Resource::Filter(r) => {
                        app.kernel
                            .remove_tc_filter(w, r)
                            .map_err(|e| EffectFailure {
                                cause: e.cause.into(),
                                remaining: Resource::Filter(e.remaining),
                            })
                    }
                    Resource::Stage(r) => {
                        app.kernel.remove_tc_stage(w, r).map_err(|e| EffectFailure {
                            cause: e.cause.into(),
                            remaining: Resource::Stage(e.remaining),
                        })
                    }
                    Resource::Record(r) => app.store.delete_tc(w, r).map_err(|e| EffectFailure {
                        cause: e.cause.into(),
                        remaining: Resource::Record(e.remaining),
                    }),
                };
                next.completed(outcome)
            }
        };
    }
}

fn finish<S: TcStore, K: TcLifecycle>(
    cause: Option<LinkCause>,
    report: XdpCleanupReport<Resource<S, K>, LinkCause>,
) -> Result<TcReport<S, K>, TcError<S, K>> {
    if report.unresolved() == 0 {
        Ok(TcReport {
            cause,
            cleanup: report,
            restorations: Vec::new(),
            committed: None,
        })
    } else {
        Err(TcError {
            cause,
            recovery: Some(Recovery::Cleanup(Box::new(report))),
            admission: None,
            restorations: Vec::new(),
            committed: None,
        })
    }
}

impl<S: TcStore, K: TcLifecycle> Bpfman<S, K> {
    /// Attach a member, rebuilding an occupied dispatcher in priority order.
    pub fn attach_tc(&self, r: TcAttach) -> Result<StoredLink, TcError<S, K>> {
        self.attach_tc_with_cancellation(r, &Cancellation::new())
    }

    /// Every cancellation before publication compensates owned acquisitions.
    pub fn attach_tc_with_cancellation(
        &self,
        r: TcAttach,
        c: &Cancellation,
    ) -> Result<StoredLink, TcError<S, K>> {
        self.store
            .runtime()
            .with_writer(
                AcquireOptions {
                    timeout: self.lock_timeout,
                    cancelled: Some(c.flag()),
                },
                |w| replacement::attach(self, &w, &r, c),
            )
            .map_err(|e| TcError::from(LinkCause::from(e)))?
    }

    fn attach_tc_first(
        &self,
        w: &RuntimeWriter<'_>,
        r: &TcAttach,
        c: &Cancellation,
        key: bpfman_model::XdpKey,
        prepared: K::Prepared,
    ) -> Result<StoredLink, TcError<S, K>> {
        let mut resources = Vec::new();
        let result = (|| -> Result<StoredLink, LinkCause> {
            check(c)?;
            self.store.preflight_tc(w, key, r.program_id)?;
            check(c)?;
            let stage = match self.kernel.stage_tc(w, prepared, r.proceed_on) {
                Ok(v) => v,
                Err(f) => {
                    resources.extend(f.remaining.map(Resource::Stage));
                    return Err(f.cause.into());
                }
            };
            resources.push(Resource::Stage(stage));
            check(c)?;
            let Some(Resource::Stage(stage)) = resources.first() else {
                return Err(Cause::Invalid("missing TC stage").into());
            };
            let filter = match self.kernel.attach_tc_filter(w, stage) {
                Ok(v) => v,
                Err(f) => {
                    resources.extend(f.remaining.map(Resource::Filter));
                    return Err(f.cause.into());
                }
            };
            resources.push(Resource::Filter(filter));
            check(c)?;
            let [Resource::Stage(stage), Resource::Filter(filter)] = resources.as_slice() else {
                return Err(Cause::Invalid("incomplete TC acquisitions").into());
            };
            let (dispatcher_id, extension_link_id) = K::tc_ids(stage)?;
            let (filter_priority, filter_handle) = K::tc_filter(filter)?;
            let details = TcLink {
                revision: NonZeroU32::MIN,
                slot: bpfman_model::XdpSlot::FIRST,
                netns: r.netns.clone(),
                key,
                interface: r.interface.clone(),
                priority: r.priority,
                proceed_on: r.proceed_on,
                dispatcher_id,
                filter_priority,
                filter_handle,
            };
            self.store
                .commit_tc(
                    w,
                    TcCommit {
                        program_id: r.program_id,
                        details: &details,
                        extension_link_id,
                        metadata: &r.metadata,
                        created_at: &chrono::Utc::now()
                            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
                    },
                )
                .map_err(Into::into)
        })();
        result.map_err(|cause| TcError {
            cause: Some(cause),
            recovery: Some(Recovery::Cleanup(Box::new(cleanup(
                self,
                w,
                XdpCleanup::new(resources),
            )))),
            admission: None,
            restorations: Vec::new(),
            committed: None,
        })
    }

    /// Detach the exact stored TC filter and preserve unrelated ingress/egress filters.
    pub fn detach_tc(&self, id: NonZeroU64) -> Result<TcReport<S, K>, TcError<S, K>> {
        self.detach_tc_with_cancellation(id, &Cancellation::new())
    }

    /// Cancellation applies before destructive cleanup; admitted cleanup finishes its pass.
    pub fn detach_tc_with_cancellation(
        &self,
        id: NonZeroU64,
        c: &Cancellation,
    ) -> Result<TcReport<S, K>, TcError<S, K>> {
        self.store
            .runtime()
            .with_writer(
                AcquireOptions {
                    timeout: self.lock_timeout,
                    cancelled: Some(c.flag()),
                },
                |w| {
                    check(c).map_err(TcError::from)?;
                    let (_, teardown) = self.observe_tc_locked(&w, id).map_err(TcError::from)?;
                    check(c).map_err(TcError::from)?;
                    teardown.run(self, &w)
                },
            )
            .map_err(|e| TcError::from(LinkCause::from(e)))?
    }

    /// Retry only unresolved cleanup once; admission failure returns the original receipts.
    pub fn retry_tc_cleanup(&self, error: TcError<S, K>) -> Result<TcReport<S, K>, TcError<S, K>> {
        self.retry_tc_cleanup_with_cancellation(error, &Cancellation::new())
    }

    /// Cancel retry admission without losing receipts; admitted cleanup completes its pass.
    pub fn retry_tc_cleanup_with_cancellation(
        &self,
        error: TcError<S, K>,
        c: &Cancellation,
    ) -> Result<TcReport<S, K>, TcError<S, K>> {
        let mut pending = Some(error);
        match self.store.runtime().with_writer(
            AcquireOptions {
                timeout: self.lock_timeout,
                cancelled: Some(c.flag()),
            },
            |w| {
                let Some(error) = pending.take() else {
                    return Err(TcError::from(LinkCause::from(Cause::Invalid(
                        "missing TC retry ownership",
                    ))));
                };
                self.retry_tc_locked(&w, error)
            },
        ) {
            Ok(result) => result,
            Err(cause) => match pending {
                Some(mut error) => {
                    error.admission = Some(cause.into());
                    Err(error)
                }
                None => Err(TcError::from(LinkCause::from(cause))),
            },
        }
    }

    pub(super) fn observe_tc_locked(
        &self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU64,
    ) -> Result<(StoredLink, Teardown<S, K>), LinkCause> {
        replacement::observe_removal(self, w, id)
    }

    pub(super) fn retry_tc_locked(
        &self,
        w: &RuntimeWriter<'_>,
        mut error: TcError<S, K>,
    ) -> Result<TcReport<S, K>, TcError<S, K>> {
        let report = match error.recovery.take() {
            Some(Recovery::Restore(restoration)) => {
                let mut error = replacement::restore(self, w, restoration.retry());
                if error.unresolved() == 0 {
                    if let Some(Recovery::Cleanup(cleanup)) = error.recovery.take() {
                        return Ok(TcReport {
                            cause: error.cause,
                            cleanup: *cleanup,
                            restorations: error.restorations,
                            committed: error.committed,
                        });
                    }
                }
                return Err(error);
            }
            Some(Recovery::Cleanup(report)) => report,
            None => return Err(error),
        };
        let result = finish(error.cause, cleanup(self, w, report.retry()));
        match result {
            Ok(mut report) => {
                report.restorations = error.restorations;
                report.committed = error.committed;
                Ok(report)
            }
            Err(mut failure) => {
                failure.restorations = error.restorations;
                failure.committed = error.committed;
                Err(failure)
            }
        }
    }
}

impl<S: OpenStore, K> Bpfman<S, K>
where
    S::Reader: bpfman_store::LinkReader,
{
    /// Read complete committed TC ingress dispatchers without a writer lock.
    pub fn list_tc_dispatchers(
        &self,
    ) -> Result<Vec<bpfman_model::TcDispatcherSnapshot>, LinkCause> {
        use bpfman_store::LinkReader;
        self.store
            .reader()
            .read_tc_dispatchers()
            .map_err(Into::into)
    }

    /// Read one complete TC ingress dispatcher without acquiring the writer lock.
    pub fn get_tc_dispatcher(
        &self,
        key: bpfman_model::XdpKey,
    ) -> Result<bpfman_model::TcDispatcherSnapshot, LinkCause> {
        self.list_tc_dispatchers()?
            .into_iter()
            .find(|s| s.members().first().is_some_and(|m| m.details.key == key))
            .ok_or_else(|| Cause::NotFound.into())
    }
}
