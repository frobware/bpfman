use super::{Artifacts, UnloadEffects};
use crate::{UnloadCause, unload_error::Cause};
use bpfman_core::EffectFailure;
use bpfman_fs::{Bytecode, MapDirectory, MapPin, ProgramPin, RuntimeWriter};
use bpfman_store_sqlite::{PrivateMapSet, ProgramRecord};
use std::num::NonZeroU32;
pub(crate) struct Effects;
fn map_failure<R, E: Into<UnloadCause>>(
    failure: EffectFailure<R, E>,
) -> EffectFailure<R, UnloadCause> {
    EffectFailure {
        remaining: failure.remaining,
        cause: failure.cause.into(),
    }
}
impl UnloadEffects for Effects {
    type Pin = ProgramPin;
    type Record = ProgramRecord;
    type Map = MapPin;
    type Directory = MapDirectory;
    type MapSet = PrivateMapSet;
    type Bytecode = Bytecode;
    type Error = UnloadCause;
    fn observe_store(
        &mut self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<(ProgramRecord, PrivateMapSet), UnloadCause> {
        bpfman_store_sqlite::observe_unload(writer, id)?
            .map(|record| record.into_parts())
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
        receipt: ProgramRecord,
    ) -> Result<(), EffectFailure<ProgramRecord, UnloadCause>> {
        bpfman_store_sqlite::delete_unloaded_program(writer, receipt).map_err(map_failure)
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
        receipt: PrivateMapSet,
    ) -> Result<(), EffectFailure<PrivateMapSet, UnloadCause>> {
        bpfman_store_sqlite::delete_unused_map_set(writer, receipt).map_err(map_failure)
    }
    fn remove_bytecode(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Bytecode,
    ) -> Result<(), EffectFailure<Bytecode, UnloadCause>> {
        writer.remove_bytecode(receipt).map_err(map_failure)
    }
}
