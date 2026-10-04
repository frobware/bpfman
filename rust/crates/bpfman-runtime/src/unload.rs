//! The single production interpreter for committed-state forward teardown.

use crate::{Bpfman, UnloadError, UnloadReport};
use bpfman_core::{EffectFailure, UnloadProgram, UnloadStep};
use bpfman_fs::RuntimeWriter;
use bpfman_lock::AcquireOptions;
use bpfman_store::{LinkReader, LinkStore, UnloadStore};
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

pub(super) type StoreReport<S> = bpfman_core::UnloadReport<
    bpfman_fs::ProgramPin,
    <S as UnloadStore>::ProgramReceipt,
    bpfman_fs::MapPin,
    bpfman_fs::MapDirectory,
    <S as UnloadStore>::MapSetReceipt,
    bpfman_fs::Bytecode,
    crate::UnloadCause,
    bpfman_fs::LinkPin,
    <S as LinkStore>::LinkReceipt,
>;

impl<S: bpfman_store::OpenStore + UnloadStore + LinkStore> Bpfman<S>
where
    S::Reader: LinkReader,
{
    /// Unload one committed tracepoint with a private map set.
    /// Pending and finalised links are cleaned before program teardown.
    /// Observations and teardown share one writer scope. Independent cleanup
    /// continues after record failure; post-record cleanup may return warnings.
    /// Retained receipts support explicit retry even after the row is gone.
    pub fn unload(&self, id: NonZeroU32) -> Result<UnloadReport<S>, UnloadError<S>> {
        self.unload_with_cancellation(id, &crate::Cancellation::new())
    }

    /// Cancel before teardown begins; once admitted, finish one complete pass
    /// under the writer lock, retaining every failed or blocked instruction.
    #[tracing::instrument(name = "program.unload", level = "debug", skip_all, fields(program_id = id.get()), err)]
    pub fn unload_with_cancellation(
        &self,
        id: NonZeroU32,
        cancellation: &crate::Cancellation,
    ) -> Result<UnloadReport<S>, UnloadError<S>> {
        self.store
            .runtime()
            .with_writer(
                AcquireOptions {
                    timeout: self.lock_timeout,
                    cancelled: Some(cancellation.flag()),
                },
                |writer| {
                    let report = run(&writer, &mut real::Effects(&self.store), id, cancellation)?;
                    finish::<S>(report)
                },
            )
            .map_err(crate::UnloadCause::from)?
    }
}

fn run<F: UnloadEffects>(
    writer: &RuntimeWriter<'_>,
    effects: &mut F,
    id: NonZeroU32,
    cancellation: &crate::Cancellation,
) -> Result<ReportFor<F>, F::Error> {
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
    Ok(drain(
        writer,
        effects,
        UnloadProgram::new_with_links(
            links,
            artifacts.pin,
            record,
            artifacts.maps,
            artifacts.directory,
            map_set,
            artifacts.bytecode,
        ),
    ))
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

pub(super) fn finish<S: UnloadStore + LinkStore>(
    report: StoreReport<S>,
) -> Result<UnloadReport<S>, UnloadError<S>> {
    let failed = report.failed();
    let report = UnloadReport { report };

    if failed {
        Err(UnloadError {
            failure: crate::unload_error::Failure::Incomplete(Box::new(report)),
        })
    } else {
        Ok(report)
    }
}
