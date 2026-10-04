//! One production interpreter for tracepoint attachment, detach, and cleanup.

use crate::{Bpfman, Cancellation, LinkCause, LinkError, LinkReport, TracepointAttach};
use bpfman_core::{EffectFailure, LinkCleanup, LinkCleanupReport, LinkCleanupStep, LinkResource};
use bpfman_fs::RuntimeWriter;
use bpfman_lock::AcquireOptions;
use bpfman_model::StoredLink;
use bpfman_store::{LinkStore, OpenStore};
use std::num::{NonZeroU32, NonZeroU64};

mod real;
#[cfg(test)]
mod tests;

trait Effects {
    type Prepared;
    type Live;
    type Pin;
    type Receipt;

    fn prepare(
        &mut self,
        writer: &RuntimeWriter<'_>,
        program: NonZeroU32,
    ) -> Result<Self::Prepared, LinkCause>;

    fn create(
        &mut self,
        writer: &RuntimeWriter<'_>,
        request: &TracepointAttach,
        created: &str,
    ) -> Result<(StoredLink, Self::Receipt), LinkCause>;

    fn attach(
        &mut self,
        writer: &RuntimeWriter<'_>,
        prepared: Self::Prepared,
        request: &TracepointAttach,
    ) -> Result<Self::Live, LinkCause>;

    // A failed pin closes an unpinned local handle, or returns ownership of a
    // partially acquired pin. No live attachment may be hidden by Err(None).
    fn pin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        live: Self::Live,
        id: NonZeroU64,
    ) -> Result<Self::Pin, EffectFailure<Option<Self::Pin>, LinkCause>>;

    fn finalise(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Receipt,
        pin: &Self::Pin,
    ) -> Result<StoredLink, EffectFailure<Self::Receipt, LinkCause>>;

    fn observe(
        &mut self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU64,
    ) -> Result<(StoredLink, Self::Receipt), LinkCause>;

    fn observe_pin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        record: &StoredLink,
    ) -> Result<Option<Self::Pin>, LinkCause>;

    fn release(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Live,
    ) -> Result<(), EffectFailure<Self::Live, LinkCause>>;

    fn unpin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Pin,
    ) -> Result<(), EffectFailure<Self::Pin, LinkCause>>;

    fn delete(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Receipt,
    ) -> Result<(), EffectFailure<Self::Receipt, LinkCause>>;
}

type ReportFor<F> = LinkCleanupReport<
    <F as Effects>::Live,
    <F as Effects>::Pin,
    <F as Effects>::Receipt,
    LinkCause,
>;
pub(super) type StoreReport<S> = LinkCleanupReport<
    bpfman_fs::LiveTracepoint,
    bpfman_fs::LinkPin,
    <S as LinkStore>::LinkReceipt,
    LinkCause,
>;

enum Failed<F: Effects> {
    Before(LinkCause),
    After {
        id: NonZeroU64,
        primary: LinkCause,
        report: ReportFor<F>,
    },
}

fn check(cancellation: &Cancellation) -> Result<(), LinkCause> {
    if cancellation.is_cancelled() {
        return Err(super::link_error::Cause::Cancelled.into());
    }
    Ok(())
}

fn cleanup<F: Effects>(
    writer: &RuntimeWriter<'_>,
    effects: &mut F,
    mut operation: LinkCleanup<F::Live, F::Pin, F::Receipt, LinkCause>,
) -> ReportFor<F> {
    loop {
        operation = match operation.next() {
            LinkCleanupStep::Live { receipt, next } => {
                next.completed(effects.release(writer, receipt))
            }
            LinkCleanupStep::Pin { receipt, next } => {
                next.completed(effects.unpin(writer, receipt))
            }
            LinkCleanupStep::Record { receipt, next } => {
                next.completed(effects.delete(writer, receipt))
            }
            LinkCleanupStep::Complete(report) => return report,
        };
    }
}

fn compensate<F: Effects>(
    writer: &RuntimeWriter<'_>,
    effects: &mut F,
    id: NonZeroU64,
    cause: LinkCause,
    receipt: F::Receipt,
    resource: Option<LinkResource<F::Live, F::Pin>>,
) -> Failed<F> {
    Failed::After {
        id,
        primary: cause,
        report: cleanup(writer, effects, LinkCleanup::new(resource, receipt)),
    }
}

fn attach<F: Effects>(
    writer: &RuntimeWriter<'_>,
    effects: &mut F,
    request: &TracepointAttach,
    created: &str,
    cancellation: &Cancellation,
) -> Result<StoredLink, Failed<F>> {
    check(cancellation).map_err(Failed::Before)?;
    let prepared = effects
        .prepare(writer, request.program_id)
        .map_err(Failed::Before)?;
    check(cancellation).map_err(Failed::Before)?;
    let (record, receipt) = effects
        .create(writer, request, created)
        .map_err(Failed::Before)?;
    let id = record.id;
    let live = match check(cancellation).and_then(|()| effects.attach(writer, prepared, request)) {
        Ok(live) => live,
        Err(cause) => return Err(compensate(writer, effects, id, cause, receipt, None)),
    };

    if let Err(cause) = check(cancellation) {
        return Err(compensate(
            writer,
            effects,
            id,
            cause,
            receipt,
            Some(LinkResource::Live(live)),
        ));
    }

    let pin = match effects.pin(writer, live, id) {
        Ok(pin) => pin,
        Err(failure) => {
            return Err(compensate(
                writer,
                effects,
                id,
                failure.cause,
                receipt,
                failure.remaining.map(LinkResource::Pin),
            ));
        }
    };

    if let Err(cause) = check(cancellation) {
        return Err(compensate(
            writer,
            effects,
            id,
            cause,
            receipt,
            Some(LinkResource::Pin(pin)),
        ));
    }

    // In-flight finalisation determines the result; do not compensate a committed
    // attachment merely because cancellation arrived during the transaction.
    match effects.finalise(writer, receipt, &pin) {
        Ok(record) => Ok(record),
        Err(failure) => Err(compensate(
            writer,
            effects,
            id,
            failure.cause,
            failure.remaining,
            Some(LinkResource::Pin(pin)),
        )),
    }
}

fn detach<F: Effects>(
    writer: &RuntimeWriter<'_>,
    effects: &mut F,
    id: NonZeroU64,
    cancellation: &Cancellation,
) -> Result<ReportFor<F>, LinkCause> {
    check(cancellation)?;
    let (record, receipt) = effects.observe(writer, id)?;
    check(cancellation)?;
    let pin = effects.observe_pin(writer, &record)?;
    check(cancellation)?;

    // Once admitted, teardown is destructive. Finish this pass regardless of
    // later cancellation, keeping record deletion dependent on successful unpin.
    Ok(cleanup(
        writer,
        effects,
        LinkCleanup::new(pin.map(LinkResource::Pin), receipt),
    ))
}

impl<S: OpenStore + LinkStore> Bpfman<S> {
    /// Attach a managed tracepoint, preserving pending intent if cleanup fails.
    pub fn attach_tracepoint(&self, request: TracepointAttach) -> Result<StoredLink, LinkError<S>> {
        self.attach_tracepoint_with_cancellation(request, &Cancellation::new())
    }

    /// Check cancellation between forward effects. Compensation retains writer
    /// authority and ignores the forward token; an in-flight commit wins.
    #[tracing::instrument(name = "link.attach_tracepoint", level = "debug", skip_all, err)]
    pub fn attach_tracepoint_with_cancellation(
        &self,
        request: TracepointAttach,
        cancellation: &Cancellation,
    ) -> Result<StoredLink, LinkError<S>> {
        let created = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
        self.store
            .runtime()
            .with_writer(
                AcquireOptions {
                    timeout: self.lock_timeout,
                    cancelled: Some(cancellation.flag()),
                },
                |writer| {
                    attach(
                        &writer,
                        &mut real::Adapter(&self.store),
                        &request,
                        &created,
                        cancellation,
                    )
                    .map_err(|failure| match failure {
                        Failed::Before(cause) => LinkError::from(cause),
                        Failed::After {
                            id,
                            primary,
                            report,
                        } => LinkError {
                            failure: super::link_error::Failure::After(Box::new(LinkReport {
                                id,
                                primary: Some(primary),
                                cleanup: report,
                            })),
                        },
                    })
                },
            )
            .map_err(LinkCause::from)?
    }

    /// Detach a stored standalone link, keeping the program loaded.
    pub fn detach(&self, id: NonZeroU64) -> Result<LinkReport<S>, LinkError<S>> {
        self.detach_with_cancellation(id, &Cancellation::new())
    }

    /// Cancel during admission and observation; admitted teardown finishes its pass.
    #[tracing::instrument(name = "link.detach", level = "debug", skip_all, fields(link_id = id.get()), err)]
    pub fn detach_with_cancellation(
        &self,
        id: NonZeroU64,
        cancellation: &Cancellation,
    ) -> Result<LinkReport<S>, LinkError<S>> {
        self.store
            .runtime()
            .with_writer(
                AcquireOptions {
                    timeout: self.lock_timeout,
                    cancelled: Some(cancellation.flag()),
                },
                |writer| {
                    let cleanup =
                        detach(&writer, &mut real::Adapter(&self.store), id, cancellation)?;
                    finish(LinkReport {
                        id,
                        primary: None,
                        cleanup,
                    })
                },
            )
            .map_err(LinkCause::from)?
    }

    /// Attempt only unresolved cleanup once. A preflight error has no owned work.
    pub fn retry_link_cleanup(&self, error: LinkError<S>) -> Result<LinkReport<S>, LinkError<S>> {
        self.retry_link_cleanup_with_cancellation(error, &Cancellation::new())
    }

    /// Cancel retry admission without losing receipts. An admitted pass ignores
    /// cancellation and retains original failures and all earlier attempt history.
    #[tracing::instrument(name = "link.retry_cleanup", level = "debug", skip_all, err)]
    pub fn retry_link_cleanup_with_cancellation(
        &self,
        error: LinkError<S>,
        cancellation: &Cancellation,
    ) -> Result<LinkReport<S>, LinkError<S>> {
        use super::link_error::Failure;
        let report = match error.failure {
            Failure::Before(_) => return Err(error),
            Failure::After(report) | Failure::Admission { report, .. } => report,
        };
        let mut pending = Some(*report);
        let result = self.store.runtime().with_writer(
            AcquireOptions {
                timeout: self.lock_timeout,
                cancelled: Some(cancellation.flag()),
            },
            |writer| {
                pending.take().map(|mut report| {
                    report.cleanup = cleanup(
                        &writer,
                        &mut real::Adapter(&self.store),
                        report.cleanup.retry(),
                    );
                    finish(report)
                })
            },
        );

        match result {
            Ok(Some(result)) => result,
            Err(cause) => {
                let cause = LinkCause::from(cause);
                Err(LinkError {
                    failure: match pending {
                        Some(report) => Failure::Admission {
                            cause,
                            report: Box::new(report),
                        },
                        None => Failure::Before(cause),
                    },
                })
            }
            Ok(None) => Err(LinkCause::from(super::link_error::Cause::Invalid(
                "missing cleanup report",
            ))
            .into()),
        }
    }
}

impl<S: OpenStore> Bpfman<S>
where
    S::Reader: bpfman_store::LinkReader,
{
    /// Read committed link intent without the writer lock. These records do not
    /// claim current kernel presence; pending links are included for recovery.
    pub fn list_link_records(&self) -> Result<Vec<StoredLink>, LinkCause> {
        self.list_link_records_with_cancellation(&Cancellation::new())
    }

    /// Read one fresh snapshot with cancellation before and after observation.
    #[tracing::instrument(name = "link.list_records", level = "debug", skip_all, err)]
    pub fn list_link_records_with_cancellation(
        &self,
        cancellation: &Cancellation,
    ) -> Result<Vec<StoredLink>, LinkCause> {
        use bpfman_store::LinkReader;
        check(cancellation)?;
        let records = self.store.reader().read_links()?;
        check(cancellation)?;

        Ok(records)
    }
}

fn finish<S: LinkStore>(report: LinkReport<S>) -> Result<LinkReport<S>, LinkError<S>> {
    if report.unresolved() == 0 {
        Ok(report)
    } else {
        Err(LinkError {
            failure: super::link_error::Failure::After(Box::new(report)),
        })
    }
}
