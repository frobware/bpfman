//! The single production interpreter for committed-state forward teardown.

use crate::{Bpfman, UnloadError, UnloadReport};
use bpfman_core::{EffectFailure, UnloadProgram, UnloadStep};
use bpfman_fs::RuntimeWriter;
use bpfman_lock::AcquireOptions;
use bpfman_store::UnloadStore;
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
    type Pin;
    type Record;
    type Map;
    type Directory;
    type MapSet;
    type Bytecode;
    type Error;

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
>;
pub(super) type ReportFor<F> = bpfman_core::UnloadReport<
    <F as UnloadEffects>::Pin,
    <F as UnloadEffects>::Record,
    <F as UnloadEffects>::Map,
    <F as UnloadEffects>::Directory,
    <F as UnloadEffects>::MapSet,
    <F as UnloadEffects>::Bytecode,
    <F as UnloadEffects>::Error,
>;

pub(super) type StoreReport<S> = bpfman_core::UnloadReport<
    bpfman_fs::ProgramPin,
    <S as UnloadStore>::ProgramReceipt,
    bpfman_fs::MapPin,
    bpfman_fs::MapDirectory,
    <S as UnloadStore>::MapSetReceipt,
    bpfman_fs::Bytecode,
    crate::UnloadCause,
>;

impl<S: bpfman_store::OpenStore + UnloadStore> Bpfman<S> {
    /// Unload one committed, unattached tracepoint with a private map set.
    /// Observations and teardown share one writer scope. Independent cleanup
    /// continues after record failure; post-record cleanup may return warnings.
    /// Retained receipts support explicit retry even after the row is gone.
    #[tracing::instrument(name = "program.unload", level = "debug", skip_all, fields(program_id = id.get()), err)]
    pub fn unload(&self, id: NonZeroU32) -> Result<UnloadReport<S>, UnloadError<S>> {
        self.store
            .runtime()
            .with_writer(
                AcquireOptions {
                    timeout: self.lock_timeout,
                    cancelled: None,
                },
                |writer| {
                    let report = run(&writer, &mut real::Effects(&self.store), id)?;
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
) -> Result<ReportFor<F>, F::Error> {
    let (record, map_set) = effects.observe_store(writer, id)?;
    let artifacts = effects.observe_artifacts(writer, id)?;

    Ok(drain(
        writer,
        effects,
        UnloadProgram::new(
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
        operation = match operation.next() {
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

pub(super) fn finish<S: UnloadStore>(
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
