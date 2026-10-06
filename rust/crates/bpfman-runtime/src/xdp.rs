//! XDP lifecycle and explicit replacement recovery under one writer scope.

use crate::{Bpfman, Cancellation, LinkCause, link_error::Cause};
use bpfman_core::{
    EffectFailure, XdpCleanup, XdpCleanupKind, XdpCleanupReport, XdpCleanupStep, XdpResource,
};
use bpfman_fs::RuntimeWriter;
use bpfman_lock::AcquireOptions;
use bpfman_model::{
    InterfaceName, StoredLink, XdpDispatcherSnapshot, XdpKey, XdpLink, XdpProceedOn,
};
use bpfman_store::{XdpDispatcherReader, XdpStore};
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
};
mod real;
mod replacement;

/// XDP attachment in the selected network namespace.
pub struct XdpAttach {
    /// Namespace selection; default selects the caller's namespace.
    pub netns: bpfman_model::NetworkNamespace,
    /// Requested mode for creating a new outer link; non-SKB modes fall back to SKB.
    pub mode: bpfman_model::XdpMode,
    /// Managed extension program.
    pub program_id: NonZeroU32,
    /// Validated interface name.
    pub interface: InterfaceName,
    /// Nonnegative priority, bounded by i32::MAX at admission.
    pub priority: u32,
    /// Return-code mask; default is PASS and dispatcher-return.
    pub proceed_on: XdpProceedOn,
    /// Operator labels.
    pub metadata: BTreeMap<String, String>,
}

enum Resource<O, E, P, D, R> {
    Outer(O),
    Extension(E),
    Program(P),
    Directory(D),
    Record(R),
}

impl<O, E, P, D, R> XdpResource for Resource<O, E, P, D, R> {
    fn kind(&self) -> XdpCleanupKind {
        match self {
            Self::Outer(_) => XdpCleanupKind::Outer,
            Self::Extension(_) => XdpCleanupKind::Extension,
            Self::Program(_) => XdpCleanupKind::Program,
            Self::Directory(_) => XdpCleanupKind::Directory,
            Self::Record(_) => XdpCleanupKind::Record,
        }
    }
}

type Owned<S, K> = Resource<
    <K as bpfman_kernel::XdpLifecycle>::Outer,
    <K as bpfman_kernel::XdpLifecycle>::Extension,
    <K as bpfman_kernel::XdpLifecycle>::DispatcherPin,
    <K as bpfman_kernel::XdpLifecycle>::Revision,
    <S as XdpStore>::XdpReceipt,
>;

type Restore<S, K> = bpfman_core::XdpRestoreFailure<
    Vec<Owned<S, K>>,
    Vec<Owned<S, K>>,
    <K as bpfman_kernel::XdpReplacement>::Switch,
    LinkCause,
>;

enum Recovery<S: XdpStore, K: bpfman_kernel::XdpReplacement> {
    Cleanup(Box<XdpCleanupReport<Owned<S, K>, LinkCause>>),
    Restore {
        failure: Box<Restore<S, K>>,
        blocked: usize,
    },
}

/// Failure retaining original cause, restoration evidence, and unresolved cleanup.
pub struct XdpError<S: XdpStore, K: bpfman_kernel::XdpReplacement> {
    primary: Option<LinkCause>,
    recovery: Option<Recovery<S, K>>,
    admission: Option<LinkCause>,
    restorations: Vec<Result<(), LinkCause>>,
    committed: Option<bpfman_model::XdpDispatcherSnapshot>,
}

/// Completed teardown or recovery, including previous failures and publication.
pub struct XdpReport<S: XdpStore, K: bpfman_kernel::XdpReplacement> {
    report: XdpCleanupReport<Owned<S, K>, LinkCause>,
    primary: Option<LinkCause>,
    restorations: Vec<Result<(), LinkCause>>,
    committed: Option<bpfman_model::XdpDispatcherSnapshot>,
}

impl<S: XdpStore, K: bpfman_kernel::XdpReplacement> XdpError<S, K> {
    pub(crate) fn cause(&self) -> Option<&LinkCause> {
        self.admission
            .as_ref()
            .or(self.primary.as_ref())
            .or_else(|| match &self.recovery {
                Some(Recovery::Restore { failure, .. }) => Some(failure.primary()),
                _ => self
                    .attempts()
                    .iter()
                    .find_map(|a| a.outcome.as_ref().err()),
            })
    }

    fn cleaned(
        primary: Option<LinkCause>,
        report: XdpCleanupReport<Owned<S, K>, LinkCause>,
        restorations: Vec<Result<(), LinkCause>>,
        committed: Option<bpfman_model::XdpDispatcherSnapshot>,
    ) -> Self {
        Self {
            primary,
            recovery: Some(Recovery::Cleanup(Box::new(report))),
            admission: None,
            restorations,
            committed,
        }
    }

    /// Backend-independent failure classification.
    pub fn kind(&self) -> crate::LinkErrorKind {
        self.cause()
            .map_or(crate::LinkErrorKind::Unavailable, LinkCause::kind)
    }

    /// Unresolved removals plus any restoration blocking staged cleanup.
    pub fn unresolved(&self) -> usize {
        match &self.recovery {
            Some(Recovery::Cleanup(r)) => r.unresolved(),
            Some(Recovery::Restore { blocked, .. }) => *blocked + 1,
            None => 0,
        }
    }

    /// Every actual cleanup attempt across passes; blocked removals are absent.
    pub fn attempts(&self) -> &[bpfman_core::XdpAttempt<LinkCause>] {
        match &self.recovery {
            Some(Recovery::Cleanup(r)) => r.attempts(),
            _ => &[],
        }
    }

    /// Every actual restoration attempt, including earlier failed passes.
    pub fn restoration_attempts(&self) -> &[Result<(), LinkCause>] {
        match &self.recovery {
            Some(Recovery::Restore { failure, .. }) => failure.attempts(),
            _ => &self.restorations,
        }
    }

    /// Successful publication when only retirement failed. Never restore this revision.
    pub fn committed_snapshot(&self) -> Option<&bpfman_model::XdpDispatcherSnapshot> {
        self.committed.as_ref()
    }
}

impl<S: XdpStore, K: bpfman_kernel::XdpReplacement> XdpReport<S, K> {
    /// Every cleanup attempt.
    pub fn attempts(&self) -> &[bpfman_core::XdpAttempt<LinkCause>] {
        self.report.attempts()
    }
    /// Remaining ownership after this successful pass.
    pub fn unresolved(&self) -> usize {
        self.report.unresolved()
    }
    /// Original forward failure, retained after successful recovery.
    pub fn primary_failure(&self) -> Option<&LinkCause> {
        self.primary.as_ref()
    }
    /// All restoration attempts, including previous failed passes.
    pub fn restoration_attempts(&self) -> &[Result<(), LinkCause>] {
        &self.restorations
    }
    /// Successful publication retained across retirement retries.
    pub fn committed_snapshot(&self) -> Option<&bpfman_model::XdpDispatcherSnapshot> {
        self.committed.as_ref()
    }
}

impl<S: XdpStore, K: bpfman_kernel::XdpReplacement> std::fmt::Debug for XdpError<S, K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XdpError")
            .field("cause", &self.cause())
            .field("restorations", &self.restoration_attempts())
            .field("attempts", &self.attempts())
            .field("committed", &self.committed)
            .field("unresolved", &self.unresolved())
            .finish()
    }
}
impl<S: XdpStore, K: bpfman_kernel::XdpReplacement> std::fmt::Display for XdpError<S, K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.committed.is_some() {
            write!(
                f,
                "XDP replacement committed; {} unresolved retirement effects",
                self.unresolved()
            )
        } else {
            write!(
                f,
                "XDP operation failed; {} unresolved recovery effects",
                self.unresolved()
            )
        }
    }
}
impl<S: XdpStore, K: bpfman_kernel::XdpReplacement> std::error::Error for XdpError<S, K> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause().map(|c| c as _)
    }
}
impl<S: XdpStore, K: bpfman_kernel::XdpReplacement> From<LinkCause> for XdpError<S, K> {
    fn from(primary: LinkCause) -> Self {
        Self {
            primary: Some(primary),
            recovery: None,
            admission: None,
            restorations: Vec::new(),
            committed: None,
        }
    }
}
impl<S: XdpStore, K: bpfman_kernel::XdpReplacement> std::fmt::Debug for XdpReport<S, K> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XdpReport")
            .field("primary", &self.primary)
            .field("restorations", &self.restorations)
            .field("attempts", &self.attempts())
            .field("committed", &self.committed)
            .finish()
    }
}

trait Effects: Sized {
    type Prepared;

    type Kernel;

    type Outer;

    type Extension;

    type Program;

    type Directory;

    type Record;

    fn prepare(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: &XdpAttach,
    ) -> Result<(XdpKey, Self::Prepared), LinkCause>;

    fn load(&mut self, r: &XdpAttach) -> Result<Self::Kernel, LinkCause>;

    fn directory(
        &mut self,
        w: &RuntimeWriter<'_>,
        p: &Self::Prepared,
    ) -> Result<Self::Directory, EffectFailure<Option<Self::Directory>, LinkCause>>;

    fn program(
        &mut self,
        w: &RuntimeWriter<'_>,
        d: &Self::Directory,
        k: &mut Self::Kernel,
    ) -> Result<Self::Program, EffectFailure<Option<Self::Program>, LinkCause>>;

    fn extension(
        &mut self,
        w: &RuntimeWriter<'_>,
        p: &mut Self::Prepared,
        d: &Self::Directory,
        k: &Self::Kernel,
    ) -> Result<Self::Extension, EffectFailure<Option<Self::Extension>, LinkCause>>;

    fn outer(
        &mut self,
        w: &RuntimeWriter<'_>,
        p: &Self::Prepared,
        k: &Self::Kernel,
        mode: bpfman_model::XdpMode,
    ) -> Result<Self::Outer, EffectFailure<Option<Self::Outer>, LinkCause>>;

    fn commit(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: &XdpAttach,
        key: XdpKey,
        p: &Self::Program,
        e: &Self::Extension,
        o: &Self::Outer,
    ) -> Result<StoredLink, LinkCause>;

    fn observe(
        &mut self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU64,
    ) -> Result<Vec<ResourceFor<Self>>, LinkCause>;

    fn remove_outer(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Self::Outer,
    ) -> Result<(), EffectFailure<Self::Outer, LinkCause>>;

    fn remove_extension(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Self::Extension,
    ) -> Result<(), EffectFailure<Self::Extension, LinkCause>>;

    fn remove_program(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Self::Program,
    ) -> Result<(), EffectFailure<Self::Program, LinkCause>>;

    fn remove_directory(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Self::Directory,
    ) -> Result<(), EffectFailure<Self::Directory, LinkCause>>;

    fn remove_record(
        &mut self,
        w: &RuntimeWriter<'_>,
        r: Self::Record,
    ) -> Result<(), EffectFailure<Self::Record, LinkCause>>;
}

type ResourceFor<F> = Resource<
    <F as Effects>::Outer,
    <F as Effects>::Extension,
    <F as Effects>::Program,
    <F as Effects>::Directory,
    <F as Effects>::Record,
>;

type ReportFor<F> = XdpCleanupReport<ResourceFor<F>, LinkCause>;

fn check(c: &Cancellation) -> Result<(), LinkCause> {
    c.check().map_err(|_| Cause::Cancelled.into())
}

fn cleanup<F: Effects>(
    w: &RuntimeWriter<'_>,
    f: &mut F,
    mut c: XdpCleanup<ResourceFor<F>, LinkCause>,
) -> ReportFor<F> {
    loop {
        c = match c.next() {
            XdpCleanupStep::Complete(r) => return r,
            XdpCleanupStep::Effect { receipt, next } => {
                let result = match receipt {
                    Resource::Outer(r) => f.remove_outer(w, r).map_err(|e| EffectFailure {
                        cause: e.cause,
                        remaining: Resource::Outer(e.remaining),
                    }),
                    Resource::Extension(r) => f.remove_extension(w, r).map_err(|e| EffectFailure {
                        cause: e.cause,
                        remaining: Resource::Extension(e.remaining),
                    }),
                    Resource::Program(r) => f.remove_program(w, r).map_err(|e| EffectFailure {
                        cause: e.cause,
                        remaining: Resource::Program(e.remaining),
                    }),
                    Resource::Directory(r) => f.remove_directory(w, r).map_err(|e| EffectFailure {
                        cause: e.cause,
                        remaining: Resource::Directory(e.remaining),
                    }),
                    Resource::Record(r) => f.remove_record(w, r).map_err(|e| EffectFailure {
                        cause: e.cause,
                        remaining: Resource::Record(e.remaining),
                    }),
                };
                next.completed(result)
            }
        };
    }
}

fn attach<F: Effects>(
    w: &RuntimeWriter<'_>,
    f: &mut F,
    r: &XdpAttach,
    c: &Cancellation,
    prepared: Option<(XdpKey, F::Prepared)>,
) -> Result<StoredLink, (LinkCause, ReportFor<F>)> {
    let mut resources = Vec::new();
    let result = (|| -> Result<StoredLink, LinkCause> {
        check(c)?;
        if r.priority > i32::MAX as u32 {
            return Err(Cause::Invalid("priority exceeds i32::MAX").into());
        }
        let (key, mut prepared) = match prepared {
            Some(prepared) => prepared,
            None => f.prepare(w, r)?,
        };
        check(c)?;
        let mut kernel = f.load(r)?;
        macro_rules! acquire {
            ($call:expr,$variant:ident) => {
                match $call {
                    Ok(value) => value,
                    Err(failure) => {
                        resources.extend(failure.remaining.map(Resource::$variant));
                        return Err(failure.cause);
                    }
                }
            };
        }
        check(c)?;
        let directory = acquire!(f.directory(w, &prepared), Directory);
        resources.push(Resource::Directory(directory));
        let Some(Resource::Directory(directory)) = resources.first() else {
            return Err(Cause::Invalid("missing revision receipt").into());
        };
        check(c)?;
        let program = acquire!(f.program(w, directory, &mut kernel), Program);
        resources.push(Resource::Program(program));
        let Some(Resource::Directory(directory)) = resources.first() else {
            return Err(Cause::Invalid("missing revision receipt").into());
        };
        check(c)?;
        let extension = acquire!(f.extension(w, &mut prepared, directory, &kernel), Extension);
        resources.push(Resource::Extension(extension));
        check(c)?;
        let outer = acquire!(f.outer(w, &prepared, &kernel, r.mode), Outer);
        resources.push(Resource::Outer(outer));
        check(c)?;
        let [
            Resource::Directory(_),
            Resource::Program(program),
            Resource::Extension(extension),
            Resource::Outer(outer),
        ] = resources.as_slice()
        else {
            return Err(Cause::Invalid("incomplete XDP acquisitions").into());
        };
        // A commit in flight determines ownership even when cancellation arrives.
        f.commit(w, r, key, program, extension, outer)
    })();
    result.map_err(|cause| (cause, cleanup(w, f, XdpCleanup::new(resources))))
}

impl<S: bpfman_store::XdpReplacementStore, K: bpfman_kernel::XdpReplacement> Bpfman<S, K>
where
    S::Reader: bpfman_store::LinkReader,
{
    /// Attach an extension, replacing a managed dispatcher when already occupied.
    pub fn attach_xdp(&self, request: XdpAttach) -> Result<StoredLink, XdpError<S, K>> {
        self.attach_xdp_with_cancellation(request, &Cancellation::new())
    }

    /// Cancellation before atomic publication uses dependent compensation.
    pub fn attach_xdp_with_cancellation(
        &self,
        request: XdpAttach,
        c: &Cancellation,
    ) -> Result<StoredLink, XdpError<S, K>> {
        self.store
            .runtime()
            .with_writer(
                AcquireOptions {
                    timeout: self.lock_timeout,
                    cancelled: Some(c.flag()),
                },
                |w| replacement::attach(self, &w, &request, c),
            )
            .map_err(|e| XdpError::from(LinkCause::from(e)))?
    }

    /// Detach a member; replace the revision or remove the final dispatcher.
    pub fn detach_xdp(&self, id: NonZeroU64) -> Result<XdpReport<S, K>, XdpError<S, K>> {
        self.detach_xdp_with_cancellation(id, &Cancellation::new())
    }

    /// Cancellation applies only before destructive teardown begins.
    pub fn detach_xdp_with_cancellation(
        &self,
        id: NonZeroU64,
        c: &Cancellation,
    ) -> Result<XdpReport<S, K>, XdpError<S, K>> {
        self.store
            .runtime()
            .with_writer(
                AcquireOptions {
                    timeout: self.lock_timeout,
                    cancelled: Some(c.flag()),
                },
                |w| replacement::detach(self, &w, id, c),
            )
            .map_err(|e| XdpError::from(LinkCause::from(e)))?
    }

    pub(crate) fn detach_xdp_locked(
        &self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU64,
    ) -> Result<XdpReport<S, K>, XdpError<S, K>> {
        replacement::detach(self, w, id, &Cancellation::new())
    }

    pub(crate) fn retry_xdp_locked(
        &self,
        w: &RuntimeWriter<'_>,
        mut error: XdpError<S, K>,
    ) -> Result<XdpReport<S, K>, XdpError<S, K>> {
        let Some(recovery) = error.recovery.take() else {
            return Err(error);
        };
        let report = match recovery {
            Recovery::Cleanup(report) => cleanup(
                w,
                &mut real::Adapter(&self.store, &self.kernel),
                report.retry(),
            ),
            Recovery::Restore { failure, blocked } => {
                error = replacement::restore(self, w, failure.retry(), blocked);
                match error.recovery.take() {
                    Some(Recovery::Cleanup(report)) => *report,
                    other => {
                        error.recovery = other;
                        return Err(error);
                    }
                }
            }
        };
        if report.unresolved() == 0 {
            Ok(XdpReport {
                report,
                primary: error.primary,
                restorations: error.restorations,
                committed: error.committed,
            })
        } else {
            error.recovery = Some(Recovery::Cleanup(Box::new(report)));
            error.admission = None;
            Err(error)
        }
    }

    /// Retry unresolved cleanup once, retaining history and original failure.
    pub fn retry_xdp_cleanup(
        &self,
        error: XdpError<S, K>,
    ) -> Result<XdpReport<S, K>, XdpError<S, K>> {
        self.retry_xdp_cleanup_with_cancellation(error, &Cancellation::new())
    }

    /// A failed admission retains all resources; an admitted pass ignores cancellation.
    pub fn retry_xdp_cleanup_with_cancellation(
        &self,
        error: XdpError<S, K>,
        c: &Cancellation,
    ) -> Result<XdpReport<S, K>, XdpError<S, K>> {
        if error.recovery.is_none() {
            return Err(error);
        }
        let mut pending = Some(error);
        let result = self.store.runtime().with_writer(
            AcquireOptions {
                timeout: self.lock_timeout,
                cancelled: Some(c.flag()),
            },
            |w| {
                let Some(error) = pending.take() else {
                    return Err(XdpError::from(LinkCause::from(Cause::Invalid(
                        "missing retry ownership",
                    ))));
                };
                self.retry_xdp_locked(&w, error)
            },
        );
        match result {
            Ok(r) => r,
            Err(cause) => {
                let cause = LinkCause::from(cause);
                match pending {
                    Some(mut error) => {
                        error.admission = Some(cause);
                        Err(error)
                    }
                    None => Err(cause.into()),
                }
            }
        }
    }
}

fn finish<S: XdpStore, K: bpfman_kernel::XdpReplacement>(
    report: XdpCleanupReport<Owned<S, K>, LinkCause>,
) -> Result<XdpReport<S, K>, XdpError<S, K>> {
    if report.unresolved() == 0 {
        Ok(XdpReport {
            report,
            primary: None,
            restorations: Vec::new(),
            committed: None,
        })
    } else {
        Err(XdpError::cleaned(None, report, Vec::new(), None))
    }
}

impl<S: bpfman_store::OpenStore, K> Bpfman<S, K>
where
    S::Reader: XdpDispatcherReader,
{
    /// List complete supported dispatchers from one store snapshot, without the writer lock.
    pub fn list_xdp_dispatchers(&self) -> Result<Vec<XdpDispatcherSnapshot>, LinkCause> {
        self.store
            .reader()
            .read_xdp_dispatchers()
            .map_err(Into::into)
    }

    /// Read a complete dispatcher snapshot without acquiring the writer lock.
    pub fn get_xdp_dispatcher(&self, key: XdpKey) -> Result<XdpDispatcherSnapshot, LinkCause> {
        self.store
            .reader()
            .read_xdp_dispatcher(key)?
            .ok_or_else(|| Cause::NotFound.into())
    }
}

#[cfg(test)]
mod tests;
