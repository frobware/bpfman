//! The single production interpreter for committed-state forward teardown.

use crate::{
    Bpfman, UnloadCause, UnloadError, UnloadReport,
    unload_error::{Cause, Failure},
};
use bpfman_core::{EffectFailure, UnloadProgram, UnloadStep};
use bpfman_fs::RuntimeWriter;
use bpfman_kernel::{ProgramResources, TracepointLinks, XdpReplacement};
use bpfman_lock::AcquireOptions;
use bpfman_store::{LinkReader, LinkStore, OpenStore, UnloadStore, XdpReplacementStore};
use std::num::NonZeroU32;

pub(super) mod real;
#[cfg(test)]
mod tests;

pub(super) struct Artifacts<P, M, D, B> {
    pub pin: Option<P>,
    pub maps: Vec<M>,
    pub directory: Option<D>,
    pub bytecode: Option<B>,
}

pub(super) trait UnloadEffects {
    type LinkPin;
    type LinkRecord;
    type Pin;
    type Record;
    type Map;
    type Directory;
    type MapSet;
    type Bytecode;
    type Error;

    fn cancelled(&self) -> Self::Error;

    fn observe_store(
        &mut self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<(Self::Record, Self::MapSet), Self::Error>;

    fn observe_artifacts(
        &mut self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<ArtifactsFor<Self>, Self::Error>;

    fn observe_links(
        &mut self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<LinksFor<Self>, Self::Error>;

    fn unpin_link(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::LinkPin,
    ) -> Result<(), EffectFailure<Self::LinkPin, Self::Error>>;

    fn delete_link(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::LinkRecord,
    ) -> Result<(), EffectFailure<Self::LinkRecord, Self::Error>>;

    fn unpin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Pin,
    ) -> Result<(), EffectFailure<Self::Pin, Self::Error>>;

    fn delete_record(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Record,
    ) -> Result<(), EffectFailure<Self::Record, Self::Error>>;

    fn remove_map(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Map,
    ) -> Result<(), EffectFailure<Self::Map, Self::Error>>;

    fn remove_directory(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Directory,
    ) -> Result<(), EffectFailure<Self::Directory, Self::Error>>;

    fn delete_map_set(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::MapSet,
    ) -> Result<(), EffectFailure<Self::MapSet, Self::Error>>;

    fn remove_bytecode(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Bytecode,
    ) -> Result<(), EffectFailure<Self::Bytecode, Self::Error>>;
}

type LinksFor<F> =
    Vec<bpfman_core::UnloadLink<<F as UnloadEffects>::LinkPin, <F as UnloadEffects>::LinkRecord>>;
type ArtifactsFor<F> = Artifacts<
    <F as UnloadEffects>::Pin,
    <F as UnloadEffects>::Map,
    <F as UnloadEffects>::Directory,
    <F as UnloadEffects>::Bytecode,
>;
type OperationFor<F> = UnloadProgram<
    <F as UnloadEffects>::Pin,
    <F as UnloadEffects>::Record,
    <F as UnloadEffects>::Map,
    <F as UnloadEffects>::Directory,
    <F as UnloadEffects>::MapSet,
    <F as UnloadEffects>::Bytecode,
    <F as UnloadEffects>::Error,
    <F as UnloadEffects>::LinkPin,
    <F as UnloadEffects>::LinkRecord,
>;
pub(super) type ReportFor<F> = bpfman_core::UnloadReport<
    <F as UnloadEffects>::Pin,
    <F as UnloadEffects>::Record,
    <F as UnloadEffects>::Map,
    <F as UnloadEffects>::Directory,
    <F as UnloadEffects>::MapSet,
    <F as UnloadEffects>::Bytecode,
    <F as UnloadEffects>::Error,
    <F as UnloadEffects>::LinkPin,
    <F as UnloadEffects>::LinkRecord,
>;

pub(super) type StoreReport<S, K> = bpfman_core::UnloadReport<
    <K as bpfman_kernel::ProgramResources>::ProgramPin,
    <S as UnloadStore>::ProgramReceipt,
    <K as bpfman_kernel::ProgramResources>::MapPin,
    <K as bpfman_kernel::ProgramResources>::MapDirectory,
    <S as UnloadStore>::MapSetReceipt,
    bpfman_fs::Bytecode,
    crate::UnloadCause,
    <K as bpfman_kernel::TracepointLinks>::LinkPin,
    <S as LinkStore>::LinkReceipt,
>;

impl<
    S: bpfman_store::XdpReplacementStore + bpfman_store::TcStore + UnloadStore + LinkStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement
        + bpfman_kernel::TcLifecycle,
> Bpfman<S, K>
where
    S::Reader: LinkReader,
{
    /// Unload one committed tracepoint, XDP or TC extension with a private map set.
    /// Pending and finalised links are cleaned before program teardown.
    /// Observations and teardown share one writer scope. Independent cleanup
    /// continues after record failure; post-record cleanup may return warnings.
    /// Retained receipts support explicit retry even after the row is gone.
    pub fn unload(&self, id: NonZeroU32) -> Result<UnloadReport<S, K>, UnloadError<S, K>> {
        self.unload_with_cancellation(id, &crate::Cancellation::new())
    }

    /// Cancel before teardown begins; once admitted, finish one complete pass
    /// under the writer lock, retaining every failed or blocked instruction.
    #[tracing::instrument(name = "program.unload", level = "debug", skip_all, fields(program_id = id.get()), err)]
    pub fn unload_with_cancellation(
        &self,
        id: NonZeroU32,
        cancellation: &crate::Cancellation,
    ) -> Result<UnloadReport<S, K>, UnloadError<S, K>> {
        self.store
            .runtime()
            .with_writer(
                AcquireOptions {
                    timeout: self.lock_timeout,
                    cancelled: Some(cancellation.flag()),
                },
                |writer| {
                    if cancellation.is_cancelled() {
                        return Err(crate::UnloadCause::from(
                            crate::unload_error::Cause::Cancelled,
                        )
                        .into());
                    }
                    let operation = prepare(
                        &writer,
                        &mut real::Effects(&self.store, &self.kernel),
                        id,
                        cancellation,
                    )?;
                    let xdp = crate::unload_xdp::observe(self, &writer, id)?;
                    let tc = crate::unload_tc::observe(self, &writer, id)?;
                    if cancellation.is_cancelled() {
                        return Err(crate::UnloadCause::from(
                            crate::unload_error::Cause::Cancelled,
                        )
                        .into());
                    }
                    resume(
                        self,
                        &writer,
                        UnloadReport {
                            report: operation.defer(),
                            xdp,
                            tc,
                        },
                    )
                },
            )
            .map_err(crate::UnloadCause::from)?
    }
}

pub(super) fn resume<S, K>(
    app: &Bpfman<S, K>,
    w: &RuntimeWriter<'_>,
    mut report: UnloadReport<S, K>,
) -> Result<UnloadReport<S, K>, UnloadError<S, K>>
where
    S: XdpReplacementStore + bpfman_store::TcStore + UnloadStore + LinkStore,
    S::Reader: LinkReader,
    K: XdpReplacement + ProgramResources + TracepointLinks + bpfman_kernel::TcLifecycle,
{
    if let Err(cause) = crate::unload_xdp::advance(app, w, &mut report.xdp) {
        return Err(UnloadError {
            failure: Failure::RetryBlocked {
                cause,
                report: Box::new(report),
            },
        });
    }
    if report.xdp.blocked() {
        return Err(UnloadError {
            failure: Failure::Incomplete(Box::new(report)),
        });
    }
    if let Err(cause) = report.tc.advance(app, w) {
        return Err(UnloadError {
            failure: Failure::RetryBlocked {
                cause,
                report: Box::new(report),
            },
        });
    }
    if report.tc.unresolved() != 0 {
        return Err(UnloadError {
            failure: Failure::Incomplete(Box::new(report)),
        });
    }
    // A later writer may have attached another member while recovery was retained.
    // Refuse program teardown until that new prerequisite is handled explicitly.
    if !report.xdp.attempts().is_empty() || !report.tc.attempts().is_empty() {
        let checked = (|| -> Result<(), UnloadCause> {
            if app
                .store
                .open(w)?
                .read_links()?
                .iter()
                .any(|l| l.program_id == report.xdp.program_id())
            {
                return Err(Cause::Invalid(
                    "program acquired new links during dispatcher unload recovery",
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
    finish(
        drain(
            w,
            &mut real::Effects(&app.store, &app.kernel),
            report.report.retry(),
        ),
        report.xdp,
        report.tc,
    )
}

fn prepare<F: UnloadEffects>(
    writer: &RuntimeWriter<'_>,
    effects: &mut F,
    id: NonZeroU32,
    cancellation: &crate::Cancellation,
) -> Result<OperationFor<F>, F::Error> {
    let check = |effects: &F| {
        if cancellation.is_cancelled() {
            Err(effects.cancelled())
        } else {
            Ok(())
        }
    };
    check(effects)?;
    let (record, map_set) = effects.observe_store(writer, id)?;
    check(effects)?;
    let links = effects.observe_links(writer, id)?;
    check(effects)?;
    let artifacts = effects.observe_artifacts(writer, id)?;

    check(effects)?;

    // Removing an existing pin is irreversible. Cancellation after admission
    // must not strand a half-completed teardown.
    Ok(UnloadProgram::new_with_links(
        links,
        artifacts.pin,
        record,
        artifacts.maps,
        artifacts.directory,
        map_set,
        artifacts.bytecode,
    ))
}

#[cfg(test)]
fn run<F: UnloadEffects>(
    writer: &RuntimeWriter<'_>,
    effects: &mut F,
    id: NonZeroU32,
    cancellation: &crate::Cancellation,
) -> Result<ReportFor<F>, F::Error> {
    let operation = prepare(writer, effects, id, cancellation)?;
    Ok(drain(writer, effects, operation))
}

pub(super) fn drain<F: UnloadEffects>(
    writer: &RuntimeWriter<'_>,
    effects: &mut F,
    mut operation: OperationFor<F>,
) -> ReportFor<F> {
    loop {
        operation =
            match operation.next() {
                UnloadStep::LinkPin { receipt, next } => {
                    let id = receipt.id;
                    next.completed(
                        effects
                            .unpin_link(writer, receipt.receipt)
                            .map_err(|failure| EffectFailure {
                                remaining: bpfman_core::UnloadLinkReceipt {
                                    id,
                                    receipt: failure.remaining,
                                },
                                cause: failure.cause,
                            }),
                    )
                }
                UnloadStep::LinkRecord { receipt, next } => {
                    let id = receipt.id;
                    next.completed(effects.delete_link(writer, receipt.receipt).map_err(
                        |failure| EffectFailure {
                            remaining: bpfman_core::UnloadLinkReceipt {
                                id,
                                receipt: failure.remaining,
                            },
                            cause: failure.cause,
                        },
                    ))
                }
                UnloadStep::ProgramPin { receipt, next } => {
                    next.completed(effects.unpin(writer, receipt))
                }
                UnloadStep::ProgramRecord { receipt, next } => {
                    next.completed(effects.delete_record(writer, receipt))
                }
                UnloadStep::MapPin { receipt, next } => {
                    next.completed(effects.remove_map(writer, receipt))
                }
                UnloadStep::MapDirectory { receipt, next } => {
                    next.completed(effects.remove_directory(writer, receipt))
                }
                UnloadStep::MapSet { receipt, next } => {
                    next.completed(effects.delete_map_set(writer, receipt))
                }
                UnloadStep::Bytecode { receipt, next } => {
                    next.completed(effects.remove_bytecode(writer, receipt))
                }
                UnloadStep::Complete(report) => return report,
            };
    }
}

pub(super) fn finish<
    S: UnloadStore + LinkStore + bpfman_store::XdpStore + bpfman_store::TcStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement
        + bpfman_kernel::TcLifecycle,
>(
    report: StoreReport<S, K>,
    xdp: crate::unload_xdp::Progress<S, K>,
    tc: crate::unload_tc::Progress<S, K>,
) -> Result<UnloadReport<S, K>, UnloadError<S, K>> {
    let failed = report.failed();
    let report = UnloadReport { report, xdp, tc };

    if failed {
        Err(UnloadError {
            failure: crate::unload_error::Failure::Incomplete(Box::new(report)),
        })
    } else {
        Ok(report)
    }
}
