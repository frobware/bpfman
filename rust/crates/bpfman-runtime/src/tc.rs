//! Singleton TC ingress publication and consuming recovery under one writer lock.
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

/// First-member legacy TC ingress attachment. Replacement and TCX are unsupported.
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

/// TC failure retaining all unresolved ownership and every actual cleanup attempt.
pub struct TcError<S: TcStore, K: TcLifecycle> {
    cause: Option<LinkCause>,
    cleanup: Option<XdpCleanupReport<Resource<S, K>, LinkCause>>,
    admission: Option<LinkCause>,
}

/// Completed TC cleanup, including original forward failure and retry history.
pub struct TcReport<S: TcStore, K: TcLifecycle> {
    cause: Option<LinkCause>,
    cleanup: XdpCleanupReport<Resource<S, K>, LinkCause>,
}

impl<S: TcStore, K: TcLifecycle> TcError<S, K> {
    fn primary(&self) -> Option<&LinkCause> {
        self.admission.as_ref().or(self.cause.as_ref()).or_else(|| {
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
        self.cleanup
            .as_ref()
            .map_or(0, XdpCleanupReport::unresolved)
    }

    /// Actual cleanup attempts across explicit retry passes.
    pub fn attempts(&self) -> &[bpfman_core::XdpAttempt<LinkCause>] {
        self.cleanup.as_ref().map_or(&[], |c| c.attempts())
    }
}

impl<S: TcStore, K: TcLifecycle> From<LinkCause> for TcError<S, K> {
    fn from(cause: LinkCause) -> Self {
        Self {
            cause: Some(cause),
            cleanup: None,
            admission: None,
        }
    }
}

impl<S: TcStore, K: TcLifecycle> fmt::Display for TcError<S, K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "TC ingress operation failed; {} unresolved cleanup effects",
            self.unresolved()
        )
    }
}

impl<S: TcStore, K: TcLifecycle> fmt::Debug for TcError<S, K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TcError")
            .field("cause", &self.primary())
            .field("attempts", &self.attempts())
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
        })
    } else {
        Err(TcError {
            cause,
            cleanup: Some(report),
            admission: None,
        })
    }
}

impl<S: TcStore, K: TcLifecycle> Bpfman<S, K> {
    /// Attach only to a vacant TC ingress dispatcher point.
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
                |w| {
                    let mut resources = Vec::new();
                    let result = (|| -> Result<StoredLink, LinkCause> {
                        check(c)?;
                        if r.priority > i32::MAX as u32 {
                            return Err(Cause::Invalid("priority exceeds i32::MAX").into());
                        }
                        let program = self
                            .store
                            .open(&w)?
                            .read_records()?
                            .into_iter()
                            .find(|p| p.id == r.program_id)
                            .ok_or(Cause::NotFound)?;
                        if !matches!(program.spec, ProgramSpec::Tc(_)) {
                            return Err(Cause::Unsupported.into());
                        }
                        if w.layout().program_pin_path(program.id).to_str()
                            != Some(program.pin_path.as_str())
                        {
                            return Err(Cause::Invalid("noncanonical managed TC pin").into());
                        }
                        let (key, prepared) =
                            self.kernel
                                .prepare_tc(&w, r.program_id, &r.interface, &r.netns)?;
                        self.store.preflight_tc(&w, key, r.program_id)?;
                        check(c)?;
                        let stage = match self.kernel.stage_tc(&w, prepared, r.proceed_on) {
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
                        let filter = match self.kernel.attach_tc_filter(&w, stage) {
                            Ok(v) => v,
                            Err(f) => {
                                resources.extend(f.remaining.map(Resource::Filter));
                                return Err(f.cause.into());
                            }
                        };
                        resources.push(Resource::Filter(filter));
                        check(c)?;
                        let [Resource::Stage(stage), Resource::Filter(filter)] =
                            resources.as_slice()
                        else {
                            return Err(Cause::Invalid("incomplete TC acquisitions").into());
                        };
                        let (dispatcher_id, extension_link_id) = K::tc_ids(stage)?;
                        let (filter_priority, filter_handle) = K::tc_filter(filter)?;
                        let details = TcLink {
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
                                &w,
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
                        cleanup: Some(cleanup(self, &w, XdpCleanup::new(resources))),
                        admission: None,
                    })
                },
            )
            .map_err(|e| TcError::from(LinkCause::from(e)))?
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
                    let (snapshot, record) = self
                        .store
                        .observe_tc(&w, id)
                        .map_err(|e| TcError::from(LinkCause::from(e)))?
                        .ok_or_else(|| TcError::from(LinkCause::from(Cause::NotFound)))?;
                    let (stage, filter) = self
                        .kernel
                        .observe_tc(&w, &snapshot)
                        .map_err(|e| TcError::from(LinkCause::from(e)))?;
                    check(c).map_err(TcError::from)?;
                    finish(
                        None,
                        cleanup(
                            self,
                            &w,
                            XdpCleanup::new(vec![
                                Resource::Record(record),
                                Resource::Stage(stage),
                                Resource::Filter(filter),
                            ]),
                        ),
                    )
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
                let Some(mut error) = pending.take() else {
                    return Err(TcError::from(LinkCause::from(Cause::Invalid(
                        "missing TC retry ownership",
                    ))));
                };
                let Some(report) = error.cleanup.take() else {
                    return Err(error);
                };
                finish(error.cause, cleanup(self, &w, report.retry()))
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
}
