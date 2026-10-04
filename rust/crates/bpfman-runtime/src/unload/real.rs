use super::{Artifacts, UnloadEffects};
use crate::{UnloadCause, unload_error::Cause};
use bpfman_core::EffectFailure;
use bpfman_fs::{Bytecode, MapDirectory, MapPin, ProgramPin, RuntimeWriter};
use bpfman_store::UnloadStore;
use std::num::NonZeroU32;

pub(crate) struct Effects<'a, S>(pub(crate) &'a S);

fn map_failure<R, E: Into<UnloadCause>>(
    failure: EffectFailure<R, E>,
) -> EffectFailure<R, UnloadCause> {
    EffectFailure {
        remaining: failure.remaining,
        cause: failure.cause.into(),
    }
}

impl<S: UnloadStore> UnloadEffects for Effects<'_, S> {
    type Pin = ProgramPin;
    type Record = S::ProgramReceipt;
    type Map = MapPin;
    type Directory = MapDirectory;
    type MapSet = S::MapSetReceipt;
    type Bytecode = Bytecode;
    type Error = UnloadCause;

    fn cancelled(&self) -> UnloadCause {
        Cause::Cancelled.into()
    }

    fn observe_store(
        &mut self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<(S::ProgramReceipt, S::MapSetReceipt), UnloadCause> {
        self.0
            .observe_unload(writer, id)?
            .ok_or_else(|| Cause::NotFound(id).into())
    }

    fn observe_artifacts(
        &mut self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<Artifacts<ProgramPin, MapPin, MapDirectory, Bytecode>, UnloadCause> {
        let found = writer.observe_unload(id)?;

        Ok(Artifacts {
            pin: found.program,
            maps: found.maps,
            directory: found.directory,
            bytecode: found.bytecode,
        })
    }

    fn unpin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: ProgramPin,
    ) -> Result<(), EffectFailure<ProgramPin, UnloadCause>> {
        writer.remove_program_pin(receipt).map_err(map_failure)
    }

    fn delete_record(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: S::ProgramReceipt,
    ) -> Result<(), EffectFailure<S::ProgramReceipt, UnloadCause>> {
        self.0.delete_program(writer, receipt).map_err(map_failure)
    }

    fn remove_map(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: MapPin,
    ) -> Result<(), EffectFailure<MapPin, UnloadCause>> {
        writer.remove_map_pin(receipt).map_err(map_failure)
    }

    fn remove_directory(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: MapDirectory,
    ) -> Result<(), EffectFailure<MapDirectory, UnloadCause>> {
        writer
            .remove_empty_map_directory(receipt)
            .map_err(map_failure)
    }

    fn delete_map_set(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: S::MapSetReceipt,
    ) -> Result<(), EffectFailure<S::MapSetReceipt, UnloadCause>> {
        self.0.delete_map_set(writer, receipt).map_err(map_failure)
    }

    fn remove_bytecode(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Bytecode,
    ) -> Result<(), EffectFailure<Bytecode, UnloadCause>> {
        writer.remove_bytecode(receipt).map_err(map_failure)
    }
}
