//! Production adapters for the injectable load orchestration.

use super::{LoadEffects, effects::Inputs};
use crate::{LoadCleanup, load_error::LoadCause};
use bpfman_core::EffectFailure;
use bpfman_fs::{Bytecode, MapDirectory, MapPin, PreparedLoad, ProgramPin, RuntimeWriter};
use bpfman_model::Symbol;
use std::num::NonZeroU32;

fn map_failure<T>(failure: EffectFailure<T, bpfman_fs::Error>) -> EffectFailure<T, LoadCause> {
    EffectFailure {
        cause: failure.cause.into(),
        remaining: failure.remaining,
    }
}

pub(crate) struct Effects<'a, S>(pub(crate) &'a S);
impl<S> LoadCleanup for Effects<'_, S> {
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

impl<S: bpfman_store::OpenStore + bpfman_store::CommitLoad> LoadEffects for Effects<'_, S> {
    type Store = S::Reader;
    type Prepared = PreparedLoad;
    type Kernel = crate::kernel::LoadedObject;

    fn cancelled(&self) -> LoadCause {
        LoadCause::Cancelled
    }

    fn batch_aborted(&self) -> LoadCause {
        LoadCause::BatchAborted
    }

    fn open_store(&mut self, writer: &RuntimeWriter<'_>) -> Result<Self::Store, LoadCause> {
        crate::store::open_store(self.0, writer).map_err(LoadCause::Open)
    }

    fn prepare(&mut self, writer: &RuntimeWriter<'_>) -> Result<Self::Prepared, LoadCause> {
        writer.prepare_load().map_err(LoadCause::from)
    }

    fn load_kernel(
        &mut self,
        _writer: &RuntimeWriter<'_>,
        input: &Inputs<'_>,
    ) -> Result<Self::Kernel, LoadCause> {
        input.object.load(input.spec)
    }

    fn pin_program(
        &mut self,
        writer: &RuntimeWriter<'_>,
        prepared: &PreparedLoad,
        kernel: &mut Self::Kernel,
        name: &Symbol,
    ) -> Result<ProgramPin, EffectFailure<Option<ProgramPin>, LoadCause>> {
        let program = kernel
            .bpf
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

    fn map_names(kernel: &Self::Kernel) -> &[String] {
        &kernel.maps
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
        kernel: &Self::Kernel,
        directory: &MapDirectory,
        name: &str,
    ) -> Result<MapPin, EffectFailure<Option<MapPin>, LoadCause>> {
        let map = kernel.bpf.map(name).ok_or_else(|| EffectFailure {
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
            "version": 1, "program_id": id.get(), "program_name": input.spec.name().as_str(),
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
        inputs: &[(NonZeroU32, &Inputs<'_>)],
    ) -> Result<(), LoadCause> {
        let records: Vec<_> = inputs
            .iter()
            .map(|(id, input)| bpfman_store::LoadRecord {
                id: *id,
                spec: input.spec,
                source: input.source,
                license: &input.object.license,
                created_at: input.created_at,
                metadata: input.metadata,
                globals: &input.object.globals,
            })
            .collect();
        self.0
            .commit_programs(writer, &records)
            .map_err(LoadCause::from)
    }
}

impl<S> super::CleanupEffects for Effects<'_, S> {
    type MapDirectory = MapDirectory;

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
