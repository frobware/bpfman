use crate::{
    Bpfman, LoadError, PreparedTracepoint,
    kernel::LocalObject,
    load_error::{Failure, LoadCause, finish},
};
use bpfman_core::{EffectFailure, KernelAcquisitions, LoadProgram};
use bpfman_fs::RuntimeWriter;
use bpfman_lock::AcquireOptions;
use bpfman_model::{ObservedProgram, ProgramSpec, StoredProgramSummary, Symbol};
use std::{collections::BTreeMap, path::Path};

mod effects;
mod real;
#[cfg(test)]
mod tests;

use effects::Inputs;
pub(super) use effects::{CleanupEffects, FailureFor, LoadEffects};
pub(super) use real::Effects;

impl PreparedTracepoint {
    /// Validate the request and read the ELF without creating runtime state.
    pub fn new(
        source: &Path,
        name: Symbol,
        metadata: BTreeMap<String, String>,
    ) -> Result<Self, LoadError> {
        Self::new_with_cancellation(source, name, metadata, &crate::Cancellation::new())
    }

    /// Prepare inputs, checking cancellation before and after reading the ELF.
    pub fn new_with_cancellation(
        source: &Path,
        name: Symbol,
        metadata: BTreeMap<String, String>,
        cancellation: &crate::Cancellation,
    ) -> Result<Self, LoadError> {
        cancellation.check().map_err(|_| LoadCause::Cancelled)?;
        let source_text = source
            .to_str()
            .ok_or_else(|| LoadCause::Invalid("source path is not UTF-8".into()))?;

        let object = LocalObject::read(source, &name)?;
        cancellation.check().map_err(|_| LoadCause::Cancelled)?;

        Ok(Self {
            object,
            source: source_text.into(),
            name,
            metadata,
        })
    }
}

impl<S: bpfman_store::OpenStore + bpfman_store::CommitLoad> Bpfman<S> {
    /// Load one prepared local tracepoint without attaching it. Private maps are
    /// pinned; failures retain unresolved ownership for an explicit cleanup pass.
    pub fn load(&self, request: PreparedTracepoint) -> Result<ObservedProgram, LoadError> {
        self.load_with_cancellation(request, &crate::Cancellation::new())
    }

    /// Load with cancellation before commit. Acquired artifacts are compensated
    /// under the same writer lock; successful commit ends cancellation authority.
    #[tracing::instrument(name = "program.load", level = "debug", skip_all, err)]
    pub fn load_with_cancellation(
        &self,
        request: PreparedTracepoint,
        cancellation: &crate::Cancellation,
    ) -> Result<ObservedProgram, LoadError> {
        cancellation.check().map_err(|_| LoadCause::Cancelled)?;
        let PreparedTracepoint {
            object,
            source,
            name,
            metadata,
        } = request;
        let runtime = self.store.runtime();
        let store = &self.store;

        if runtime.layout().root().to_str().is_none() {
            return Err(LoadCause::Invalid(
                "load persistence requires a UTF-8 runtime path".into(),
            )
            .into());
        }

        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
        runtime
            .with_writer(
                AcquireOptions {
                    timeout: self.lock_timeout,
                    cancelled: Some(cancellation.flag()),
                },
                |writer| {
                    run(
                        &writer,
                        &mut Effects(store),
                        &Inputs {
                            cancellation,
                            object: &object,
                            source: &source,
                            name: &name,
                            metadata: &metadata,
                            created_at: &created_at,
                        },
                    )
                    .map_err(|failure| LoadError {
                        failure: Box::new(failure),
                        retry_lock_error: None,
                    })
                    .and_then(|stored| {
                        crate::observation::observe(
                            store,
                            writer.directory(),
                            stored.id(),
                            crate::observation::View::Load,
                        )
                        .map_err(|source| {
                            LoadCause::Observation {
                                id: stored.id(),
                                source,
                            }
                            .into()
                        })
                    })
                },
            )
            .map_err(LoadCause::from)?
    }
}

// This is the only forward interpreter, used by the CLI and fault-injection
// tests alike. The policy continuations and compensation driver are unchanged.
fn run<F: LoadEffects>(
    writer: &RuntimeWriter<'_>,
    effects: &mut F,
    input: &Inputs<'_>,
) -> Result<StoredProgramSummary, FailureFor<F>> {
    check_cancelled(effects, input)?;
    let _store = effects
        .open_store(writer)
        .map_err(Failure::NoOwnedArtifacts)?;

    check_cancelled(effects, input)?;
    let filesystem = effects.prepare(writer).map_err(Failure::NoOwnedArtifacts)?;
    let operation = LoadProgram::new(ProgramSpec::Tracepoint(input.name.clone()));

    check_cancelled(effects, input)?;
    let mut kernel = effects
        .load_kernel(writer, input)
        .map_err(Failure::NoOwnedArtifacts)?;
    let program_pin = match admission(effects, input, None)
        .and_then(|()| effects.pin_program(writer, &filesystem, &mut kernel, input.name))
    {
        Ok(pin) => pin,
        Err(failure) => {
            return Err(finish(
                writer,
                effects,
                operation.failed(EffectFailure {
                    cause: failure.cause,
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
    let id = F::program_id(&program_pin);
    let directory = match admission(effects, input, None)
        .and_then(|()| effects.create_map_directory(writer, &filesystem, id))
    {
        Ok(directory) => directory,
        Err(failure) => {
            return Err(finish(
                writer,
                effects,
                operation.failed(EffectFailure {
                    cause: failure.cause,
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

    for name in &input.object.maps {
        match admission(effects, input, None)
            .and_then(|()| effects.pin_map(writer, &kernel, &directory, name))
        {
            Ok(pin) => pins.push(pin),
            Err(failure) => {
                pins.extend(failure.remaining);

                return Err(finish(
                    writer,
                    effects,
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
    let bytecode =
        match admission(effects, input, vec![]).and_then(|()| effects.publish(writer, id, input)) {
            Ok(bytecode) => bytecode,
            Err(failure) => {
                return Err(finish(
                    writer,
                    effects,
                    publish.failed(failure),
                    Some(directory),
                    vec![],
                ));
            }
        };
    let persist = publish.published(bytecode);

    match cancellation_cause(effects, input).and_then(|()| effects.persist(writer, id, input)) {
        Ok(stored) => Ok(persist.committed(stored).stored),
        Err(cause) => Err(finish(
            writer,
            effects,
            persist.failed(cause),
            Some(directory),
            vec![],
        )),
    }
}

// Checks live in the interpreter so fake and real adapters follow identical policy.
// Cleanup has no token; cancellation can never skip receipt-bearing instructions.
fn cancellation_cause<F: LoadEffects>(effects: &F, input: &Inputs<'_>) -> Result<(), F::Error> {
    if input.cancellation.is_cancelled() {
        tracing::debug!("load cancelled before commit; compensating acquisitions");
        Err(effects.cancelled())
    } else {
        Ok(())
    }
}

fn check_cancelled<F: LoadEffects>(effects: &F, input: &Inputs<'_>) -> Result<(), FailureFor<F>> {
    cancellation_cause(effects, input).map_err(Failure::NoOwnedArtifacts)
}

fn admission<F: LoadEffects, R>(
    effects: &F,
    input: &Inputs<'_>,
    remaining: R,
) -> Result<(), EffectFailure<R, F::Error>> {
    cancellation_cause(effects, input).map_err(|cause| EffectFailure { cause, remaining })
}
