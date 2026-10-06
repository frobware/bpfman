//! Production adapters for the injectable load orchestration.

use super::{LoadEffects, effects::Inputs};
use crate::{LoadCleanup, load_error::LoadCause};
use bpfman_core::EffectFailure;
use bpfman_fs::{Bytecode, RuntimeWriter};
use bpfman_model::Symbol;
use std::num::NonZeroU32;

fn map_failure<T, E: Into<LoadCause>>(failure: EffectFailure<T, E>) -> EffectFailure<T, LoadCause> {
    EffectFailure {
        cause: failure.cause.into(),
        remaining: failure.remaining,
    }
}

pub(crate) struct Effects<'a, S, K>(pub(crate) &'a S, pub(crate) &'a K);
impl<S, K: bpfman_kernel::ProgramResources> LoadCleanup for Effects<'_, S, K> {
    type ProgramPin = K::ProgramPin;
    type MapPin = K::MapPin;
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
        receipt: K::ProgramPin,
    ) -> Result<(), EffectFailure<K::ProgramPin, LoadCause>> {
        self.1.remove_program(writer, receipt).map_err(map_failure)
    }

    fn remove_map_pin(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: K::MapPin,
    ) -> Result<(), EffectFailure<K::MapPin, LoadCause>> {
        self.1.remove_map(writer, receipt).map_err(map_failure)
    }
}

impl<S: bpfman_store::OpenStore + bpfman_store::CommitLoad, K: bpfman_kernel::ProgramLoad>
    LoadEffects for Effects<'_, S, K>
{
    type Store = S::Reader;
    type Prepared = K::Prepared;
    type Kernel = K::Loaded;

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
        self.1.prepare_load(writer).map_err(LoadCause::from)
    }

    fn load_kernel(
        &mut self,
        writer: &RuntimeWriter<'_>,
        input: &Inputs<'_>,
    ) -> Result<Self::Kernel, LoadCause> {
        self.1
            .load_program(
                writer,
                &input.object.bytes,
                &input.object.maps,
                &input.object.globals,
                input.spec,
            )
            .map_err(Into::into)
    }

    fn pin_program(
        &mut self,
        writer: &RuntimeWriter<'_>,
        prepared: &K::Prepared,
        kernel: &mut Self::Kernel,
        name: &Symbol,
    ) -> Result<K::ProgramPin, EffectFailure<Option<K::ProgramPin>, LoadCause>> {
        self.1
            .pin_program(writer, prepared, kernel, name)
            .map_err(map_failure)
    }

    fn program_id(pin: &K::ProgramPin) -> NonZeroU32 {
        K::program_id(pin)
    }

    fn map_names(kernel: &Self::Kernel) -> &[String] {
        K::map_names(kernel)
    }

    fn create_map_directory(
        &mut self,
        writer: &RuntimeWriter<'_>,
        prepared: &K::Prepared,
        id: NonZeroU32,
    ) -> Result<K::MapDirectory, EffectFailure<Option<K::MapDirectory>, LoadCause>> {
        self.1
            .create_map_directory(writer, prepared, id)
            .map_err(map_failure)
    }

    fn pin_map(
        &mut self,
        writer: &RuntimeWriter<'_>,
        kernel: &Self::Kernel,
        directory: &K::MapDirectory,
        name: &str,
    ) -> Result<K::MapPin, EffectFailure<Option<K::MapPin>, LoadCause>> {
        self.1
            .pin_map(writer, kernel, directory, name)
            .map_err(map_failure)
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

impl<S, K: bpfman_kernel::ProgramResources> super::CleanupEffects for Effects<'_, S, K> {
    type MapDirectory = K::MapDirectory;

    fn remove_map_directory(
        &mut self,
        writer: &RuntimeWriter<'_>,
        receipt: K::MapDirectory,
    ) -> Result<(), EffectFailure<K::MapDirectory, LoadCause>> {
        self.1
            .remove_map_directory(writer, receipt)
            .map_err(map_failure)
    }
}
