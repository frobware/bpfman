//! Production adapters for the injectable load orchestration.
use super::{LoadEffects, effects::Inputs};
use crate::{LoadCleanup, load_error::LoadCause};
use bpfman_core::EffectFailure;
use bpfman_fs::{Bytecode, MapDirectory, MapPin, PreparedLoad, ProgramPin, RuntimeWriter};
use bpfman_model::{StoredProgramSummary, Symbol};
use std::num::NonZeroU32;

fn map_failure<T>(failure: EffectFailure<T, bpfman_fs::Error>) -> EffectFailure<T, LoadCause> {
    EffectFailure {
        cause: failure.cause.into(),
        remaining: failure.remaining,
    }
}

pub(crate) struct Effects;
impl LoadCleanup for Effects {
    type ProgramPin = ProgramPin;
    type MapPin = MapPin;
    type Bytecode = Bytecode;
    type Error = LoadCause;
    fn remove_bytecode(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: Bytecode,
    ) -> Result<(), EffectFailure<Bytecode, LoadCause>> {
        writer.remove_bytecode(receipt).map_err(map_failure)
    }
    fn remove_program_pin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: ProgramPin,
    ) -> Result<(), EffectFailure<ProgramPin, LoadCause>> {
        writer.remove_program_pin(receipt).map_err(map_failure)
    }
    fn remove_map_pin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: MapPin,
    ) -> Result<(), EffectFailure<MapPin, LoadCause>> {
        writer.remove_map_pin(receipt).map_err(map_failure)
    }
}

impl LoadEffects for Effects {
    type Store = bpfman_store_sqlite::Store;
    type Prepared = PreparedLoad;
    type Kernel = aya::Ebpf;
    type MapDirectory = MapDirectory;

    fn open_store(&mut self, writer: &RuntimeWriter<'_>) -> Result<Self::Store, LoadCause> {
        crate::store::open_store(writer).map_err(LoadCause::Open)
    }
    fn prepare(&mut self, writer: &RuntimeWriter<'_>) -> Result<Self::Prepared, LoadCause> {
        writer.prepare_load().map_err(LoadCause::from)
    }
    fn load_kernel(
        &mut self,
        _writer: &RuntimeWriter<'_>,
        input: &Inputs<'_>,
    ) -> Result<Self::Kernel, LoadCause> {
        input.object.load(input.name)
    }
    fn pin_program(
        &mut self,
        writer: &RuntimeWriter<'_>,
        prepared: &PreparedLoad,
        kernel: &mut aya::Ebpf,
        name: &Symbol,
    ) -> Result<ProgramPin, EffectFailure<Option<ProgramPin>, LoadCause>> {
        let program = kernel
            .program_mut(name.as_str())
            .ok_or_else(|| EffectFailure {
                cause: LoadCause::Invalid("missing loaded program".into()),
                remaining: None,
            })?;
        prepared.pin_program(writer, program).map_err(map_failure)
    }
    fn program_id(pin: &ProgramPin) -> NonZeroU32 {
        pin.id()
    }
    fn create_map_directory(
        &mut self,
        writer: &RuntimeWriter<'_>,
        prepared: &PreparedLoad,
        id: NonZeroU32,
    ) -> Result<MapDirectory, EffectFailure<Option<MapDirectory>, LoadCause>> {
        prepared
            .create_map_directory(writer, id)
            .map_err(map_failure)
    }
    fn pin_map(
        &mut self,
        writer: &RuntimeWriter<'_>,
        kernel: &aya::Ebpf,
        directory: &MapDirectory,
        name: &str,
    ) -> Result<MapPin, EffectFailure<Option<MapPin>, LoadCause>> {
        let map = kernel.map(name).ok_or_else(|| EffectFailure {
            cause: LoadCause::Invalid(format!("missing loaded map {name}")),
            remaining: None,
        })?;
        directory.pin_map(writer, name, map).map_err(map_failure)
    }
    fn publish(
        &mut self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
        input: &Inputs<'_>,
    ) -> Result<Bytecode, EffectFailure<Vec<Bytecode>, LoadCause>> {
        let provenance = serde_json::to_vec_pretty(&serde_json::json!({
            "version": 1, "program_id": id.get(), "program_name": input.name.as_str(),
            "source": input.source, "source_kind": "file", "loaded_at": input.created_at,
        }))
        .map_err(|cause| EffectFailure {
            cause: LoadCause::Json(cause),
            remaining: Vec::new(),
        })?;
        writer
            .publish_bytecode(id, &input.object.bytes, &provenance)
            .map_err(map_failure)
    }
    fn persist(
        &mut self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
        input: &Inputs<'_>,
    ) -> Result<StoredProgramSummary, LoadCause> {
        bpfman_store_sqlite::persist_tracepoint(
            writer,
            bpfman_store_sqlite::TracepointRecord {
                id,
                name: input.name,
                source: input.source,
                license: &input.object.license,
                created_at: input.created_at,
                metadata: input.metadata,
            },
        )
        .map_err(LoadCause::from)
    }
    fn remove_map_directory(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: MapDirectory,
    ) -> Result<(), EffectFailure<MapDirectory, LoadCause>> {
        writer
            .remove_empty_map_directory(receipt)
            .map_err(map_failure)
    }
}
