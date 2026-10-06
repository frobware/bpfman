//! First-attach/last-detach interpreter. No dispatcher replacement is hidden here.

use crate::{Bpfman, Cancellation, LinkCause, link_error::Cause};
use bpfman_core::{
    EffectFailure, XdpCleanup, XdpCleanupKind, XdpCleanupReport, XdpCleanupStep, XdpResource,
};
use bpfman_fs::RuntimeWriter;
use bpfman_lock::AcquireOptions;
use bpfman_model::{InterfaceName, StoredLink, XdpKey, XdpLink, XdpProceedOn, XdpSnapshot};
use bpfman_store::{XdpReader, XdpStore};
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
};
mod real;

/// First XDP attachment in the current network namespace.
pub struct XdpAttach {
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

type Owned<S> = Resource<
    bpfman_fs::XdpOuter,
    bpfman_fs::XdpExtensionPin,
    bpfman_fs::XdpProgramPin,
    bpfman_fs::XdpRevision,
    <S as XdpStore>::XdpReceipt,
>;

/// Failure retaining original cause, all cleanup attempts, and unresolved ownership.
pub struct XdpError<S: XdpStore> {
    primary: Option<LinkCause>,
    report: Option<XdpCleanupReport<Owned<S>, LinkCause>>,
    admission: Option<LinkCause>,
}

/// Completed teardown report, including all actual cleanup attempts.
pub struct XdpReport<S: XdpStore> {
    report: XdpCleanupReport<Owned<S>, LinkCause>,
}

impl<S: XdpStore> XdpError<S> {
    fn cause(&self) -> Option<&LinkCause> {
        self.admission
            .as_ref()
            .or(self.primary.as_ref())
            .or_else(|| {
                self.attempts()
                    .iter()
                    .find_map(|a| a.outcome.as_ref().err())
            })
    }

    /// Backend-independent failure classification.
    pub fn kind(&self) -> crate::LinkErrorKind {
        self.cause()
            .map_or(crate::LinkErrorKind::Unavailable, LinkCause::kind)
    }

    /// Number of unresolved effects including blocked dependents.
    pub fn unresolved(&self) -> usize {
        self.report.as_ref().map_or(0, |r| r.unresolved())
    }

    /// Every attempted cleanup, including previous passes.
    pub fn attempts(&self) -> &[bpfman_core::XdpAttempt<LinkCause>] {
        self.report.as_ref().map_or(&[], |r| r.attempts())
    }
}

impl<S: XdpStore> XdpReport<S> {
    /// Every cleanup attempt.
    pub fn attempts(&self) -> &[bpfman_core::XdpAttempt<LinkCause>] {
        self.report.attempts()
    }

    /// Remaining ownership after this successful pass.
    pub fn unresolved(&self) -> usize {
        self.report.unresolved()
    }
}

impl<S: XdpStore> std::fmt::Debug for XdpError<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XdpError")
            .field("primary", &self.primary)
            .field("attempts", &self.attempts())
            .field("unresolved", &self.unresolved())
            .finish()
    }
}

impl<S: XdpStore> std::fmt::Display for XdpError<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "XDP operation failed; {} unresolved cleanup effects",
            self.unresolved()
        )
    }
}

impl<S: XdpStore> std::error::Error for XdpError<S> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause().map(|c| c as _)
    }
}

impl<S: XdpStore> From<LinkCause> for XdpError<S> {
    fn from(primary: LinkCause) -> Self {
        Self {
            primary: Some(primary),
            report: None,
            admission: None,
        }
    }
}

impl<S: XdpStore> std::fmt::Debug for XdpReport<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XdpReport")
            .field("attempts", &self.attempts())
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
) -> Result<StoredLink, (LinkCause, ReportFor<F>)> {
    let mut resources = Vec::new();
    let result = (|| -> Result<StoredLink, LinkCause> {
        check(c)?;
        if r.priority > i32::MAX as u32 {
            return Err(Cause::Invalid("priority exceeds i32::MAX").into());
        }
        let (key, mut prepared) = f.prepare(w, r)?;
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
        let outer = acquire!(f.outer(w, &prepared, &kernel), Outer);
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

impl<S: XdpStore, K> Bpfman<S, K> {
    /// Attach the first extension to an unoccupied interface in this namespace.
    pub fn attach_xdp(&self, request: XdpAttach) -> Result<StoredLink, XdpError<S>> {
        self.attach_xdp_with_cancellation(request, &Cancellation::new())
    }

    /// Cancellation before atomic publication uses dependent compensation.
    pub fn attach_xdp_with_cancellation(
        &self,
        request: XdpAttach,
        c: &Cancellation,
    ) -> Result<StoredLink, XdpError<S>> {
        self.store
            .runtime()
            .with_writer(
                AcquireOptions {
                    timeout: self.lock_timeout,
                    cancelled: Some(c.flag()),
                },
                |w| {
                    attach(&w, &mut real::Adapter(&self.store), &request, c).map_err(
                        |(primary, report)| XdpError {
                            primary: Some(primary),
                            report: Some(report),
                            admission: None,
                        },
                    )
                },
            )
            .map_err(|e| XdpError::from(LinkCause::from(e)))?
    }

    /// Detach the sole member and remove the dispatcher, preserving failed receipts.
    pub fn detach_xdp(&self, id: NonZeroU64) -> Result<XdpReport<S>, XdpError<S>> {
        self.detach_xdp_with_cancellation(id, &Cancellation::new())
    }

    /// Cancellation applies only before destructive teardown begins.
    pub fn detach_xdp_with_cancellation(
        &self,
        id: NonZeroU64,
        c: &Cancellation,
    ) -> Result<XdpReport<S>, XdpError<S>> {
        self.store
            .runtime()
            .with_writer(
                AcquireOptions {
                    timeout: self.lock_timeout,
                    cancelled: Some(c.flag()),
                },
                |w| {
                    check(c)?;
                    let mut f = real::Adapter(&self.store);
                    let resources = f.observe(&w, id)?;
                    check(c)?;
                    finish(cleanup(&w, &mut f, XdpCleanup::new(resources)))
                },
            )
            .map_err(|e| XdpError::from(LinkCause::from(e)))?
    }

    /// Retry unresolved cleanup once, retaining history and original failure.
    pub fn retry_xdp_cleanup(&self, error: XdpError<S>) -> Result<XdpReport<S>, XdpError<S>> {
        self.retry_xdp_cleanup_with_cancellation(error, &Cancellation::new())
    }

    /// A failed admission retains all resources; an admitted pass ignores cancellation.
    pub fn retry_xdp_cleanup_with_cancellation(
        &self,
        error: XdpError<S>,
        c: &Cancellation,
    ) -> Result<XdpReport<S>, XdpError<S>> {
        if error.report.is_none() {
            return Err(error);
        }
        let mut pending = Some(error);
        let result = self.store.runtime().with_writer(
            AcquireOptions {
                timeout: self.lock_timeout,
                cancelled: Some(c.flag()),
            },
            |w| {
                let Some(mut error) = pending.take() else {
                    return Err(XdpError::from(LinkCause::from(Cause::Invalid(
                        "missing retry ownership",
                    ))));
                };
                let Some(report) = error.report.take() else {
                    return Err(error);
                };
                let report = cleanup(&w, &mut real::Adapter(&self.store), report.retry());
                if report.unresolved() == 0 {
                    Ok(XdpReport { report })
                } else {
                    error.report = Some(report);
                    error.admission = None;
                    Err(error)
                }
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

fn finish<S: XdpStore>(
    report: XdpCleanupReport<Owned<S>, LinkCause>,
) -> Result<XdpReport<S>, XdpError<S>> {
    if report.unresolved() == 0 {
        Ok(XdpReport { report })
    } else {
        Err(XdpError {
            primary: None,
            report: Some(report),
            admission: None,
        })
    }
}

impl<S: bpfman_store::OpenStore, K> Bpfman<S, K>
where
    S::Reader: XdpReader,
{
    /// Read a complete dispatcher snapshot without acquiring the writer lock.
    pub fn get_xdp_dispatcher(&self, key: XdpKey) -> Result<XdpSnapshot, LinkCause> {
        self.store
            .reader()
            .read_xdp(key)?
            .ok_or_else(|| Cause::NotFound.into())
    }
}

#[cfg(test)]
mod tests;
