use crate::{
    LoadCleanup, LoadError,
    kernel::LocalObject,
    load_error::{LoadCause, finish},
};
use bpfman_core::{EffectFailure, KernelAcquisitions, LoadProgram};
use bpfman_fs::{Bytecode, MapPin, ProgramPin, RuntimeDirectory, RuntimeLayout, RuntimeWriter};
use bpfman_lock::AcquireOptions;
use bpfman_model::{ProgramSpec, StoredProgramSummary, Symbol};
use std::{collections::BTreeMap, path::Path, time::Duration};

/// Load one tracepoint from a local ELF, without attaching it. The exact bytes
/// parsed before runtime setup are both loaded and published. Private maps are
/// pinned; PinByName maps are rejected before runtime or kernel effects.
/// Failure retains unresolved ownership and supports an explicit cleanup retry.
pub fn load_tracepoint(
    layout: &RuntimeLayout,
    source: &Path,
    name: Symbol,
    metadata: &BTreeMap<String, String>,
    timeout: Duration,
) -> Result<StoredProgramSummary, LoadError> {
    let source_text = source
        .to_str()
        .ok_or_else(|| LoadCause::Invalid("source path is not UTF-8".into()))?;
    if layout.root().to_str().is_none() {
        return Err(
            LoadCause::Invalid("load persistence requires a UTF-8 runtime path".into()).into(),
        );
    }
    let object = LocalObject::read(source, &name)?;
    let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    let runtime = RuntimeDirectory::open_or_create(layout.clone()).map_err(LoadCause::from)?;
    runtime
        .with_writer(
            AcquireOptions {
                timeout,
                cancelled: None,
            },
            |writer| load(&writer, &object, source_text, name, metadata, &created_at),
        )
        .map_err(LoadCause::from)?
}

fn load(
    writer: &RuntimeWriter<'_>,
    object: &LocalObject,
    source: &str,
    name: Symbol,
    metadata: &BTreeMap<String, String>,
    created_at: &str,
) -> Result<StoredProgramSummary, LoadError> {
    // Observe/open once under the same writer scope as all later mutations.
    let _store = crate::store::open_store(writer).map_err(LoadCause::Open)?;
    let filesystem = writer.prepare_load().map_err(LoadCause::from)?;
    let operation = LoadProgram::new(ProgramSpec::Tracepoint(name.clone()));
    let mut bpf = object.load(&name)?;
    let program = bpf
        .program_mut(name.as_str())
        .ok_or_else(|| LoadCause::Invalid("missing loaded program".into()))?;
    let program_pin = match filesystem.pin_program(writer, program) {
        Ok(pin) => pin,
        Err(failure) => {
            return Err(finish(
                writer,
                operation.failed(EffectFailure {
                    cause: failure.cause.into(),
                    remaining: KernelAcquisitions {
                        program_pin: failure.remaining,
                        map_pins: vec![],
                    },
                }),
                None,
                vec![],
            ));
        }
    };
    let id = program_pin.id();
    let directory = match filesystem.create_map_directory(writer, id) {
        Ok(directory) => directory,
        Err(failure) => {
            return Err(finish(
                writer,
                operation.failed(EffectFailure {
                    cause: failure.cause.into(),
                    remaining: KernelAcquisitions {
                        program_pin: Some(program_pin),
                        map_pins: vec![],
                    },
                }),
                failure.remaining,
                vec![],
            ));
        }
    };
    let mut pins = Vec::new();
    for map_name in &object.maps {
        let result = bpf
            .map(map_name)
            .ok_or_else(|| EffectFailure {
                cause: LoadCause::Invalid(format!("missing loaded map {map_name}")),
                remaining: None,
            })
            .and_then(|map| {
                directory
                    .pin_map(writer, map_name, map)
                    .map_err(map_failure)
            });
        match result {
            Ok(pin) => pins.push(pin),
            Err(failure) => {
                pins.extend(failure.remaining);
                return Err(finish(
                    writer,
                    operation.failed(EffectFailure {
                        cause: failure.cause,
                        remaining: KernelAcquisitions {
                            program_pin: Some(program_pin),
                            map_pins: pins,
                        },
                    }),
                    Some(directory),
                    vec![],
                ));
            }
        }
    }
    let publish = operation.loaded(program_pin, pins);
    let provenance = serde_json::to_vec_pretty(&serde_json::json!({
        "version": 1, "program_id": id.get(), "program_name": name.as_str(),
        "source": source, "source_kind": "file", "loaded_at": created_at,
    }));
    let bytecode = match provenance
        .map_err(|cause| EffectFailure {
            cause: LoadCause::Json(cause),
            remaining: Vec::new(),
        })
        .and_then(|provenance| {
            writer
                .publish_bytecode(id, &object.bytes, &provenance)
                .map_err(map_failure)
        }) {
        Ok(bytecode) => bytecode,
        Err(failure) => {
            return Err(finish(
                writer,
                publish.failed(failure),
                Some(directory),
                vec![],
            ));
        }
    };
    let persist = publish.published(bytecode);
    match bpfman_store_sqlite::persist_tracepoint(
        writer,
        bpfman_store_sqlite::TracepointRecord {
            id,
            name: &name,
            source,
            license: &object.license,
            created_at,
            metadata,
        },
    ) {
        Ok(stored) => Ok(persist.committed(stored).stored),
        Err(cause) => Err(finish(
            writer,
            persist.failed(cause.into()),
            Some(directory),
            vec![],
        )),
    }
    // Dropping Aya descriptors never removes pins. On commit the store becomes
    // their owner; on failure receipts retain any unresolved pinned resources.
}

fn map_failure<T>(failure: EffectFailure<T, bpfman_fs::Error>) -> EffectFailure<T, LoadCause> {
    EffectFailure {
        cause: failure.cause.into(),
        remaining: failure.remaining,
    }
}

pub(super) struct FilesystemCleanup;
impl LoadCleanup for FilesystemCleanup {
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
