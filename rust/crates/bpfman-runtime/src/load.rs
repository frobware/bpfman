use crate::{
    Bpfman, LoadError, PreparedProgram, PreparedPrograms,
    kernel::LocalObject,
    load_error::{Failure, LoadCause, finish},
};
use bpfman_core::{EffectFailure, KernelAcquisitions, LoadProgram};
use bpfman_fs::RuntimeWriter;
use bpfman_lock::AcquireOptions;
use bpfman_model::{ObservedProgram, ProgramSpec, StoredProgramSummary};
use std::{collections::BTreeMap, path::Path};

mod effects;
mod real;
#[cfg(test)]
mod tests;

use effects::Inputs;
pub(super) use effects::{CleanupEffects, FailureFor, LoadEffects};
pub(super) use real::Effects;

impl PreparedProgram {
    /// Add distinct program selections from the same captured ELF. Validate
    /// every selection before opening runtime state; preserve input order.
    pub fn with_additional_programs<K: bpfman_kernel::ProgramLoad>(
        self,
        kernel: &K,
        specs: Vec<ProgramSpec>,
    ) -> Result<PreparedPrograms, LoadError<K>> {
        if specs.is_empty() {
            return Ok(PreparedPrograms {
                first: self,
                remaining: specs,
            });
        }

        let mut seen = std::collections::BTreeSet::from([self.spec.name().clone()]);
        let mut remaining = Vec::new();

        for spec in specs {
            let name = spec.name();
            if !seen.insert(name.clone()) {
                return Err(LoadCause::Invalid(
                    "each ELF program must be selected only once".into(),
                )
                .into());
            }
            kernel
                .validate_object(&self.object.bytes, &spec)
                .map_err(LoadCause::from)?;

            remaining.push(spec);
        }

        Ok(PreparedPrograms {
            first: self,
            remaining,
        })
    }

    /// Validate global names and byte lengths against the captured ELF before
    /// runtime creation. Values are applied when loading and retained in the store.
    pub fn with_globals<K: bpfman_kernel::ProgramLoad>(
        mut self,
        kernel: &K,
        globals: BTreeMap<String, Vec<u8>>,
    ) -> Result<Self, LoadError<K>> {
        if globals.is_empty() {
            self.object.globals.clear();
            return Ok(self);
        }

        kernel
            .validate_globals(&self.object.bytes, &globals)
            .map_err(LoadCause::from)?;
        self.object.globals = globals;

        Ok(self)
    }

    /// Validate a tracepoint or XDP selection and read the ELF without creating
    /// runtime state. Other program kinds are rejected before source access.
    pub fn new<K: bpfman_kernel::ProgramLoad>(
        kernel: &K,
        source: &Path,
        spec: ProgramSpec,
        metadata: BTreeMap<String, String>,
    ) -> Result<Self, LoadError<K>> {
        Self::new_with_cancellation(kernel, source, spec, metadata, &crate::Cancellation::new())
    }

    /// Prepare inputs, checking cancellation before and after reading the ELF.
    pub fn new_with_cancellation<K: bpfman_kernel::ProgramLoad>(
        kernel: &K,
        source: &Path,
        spec: ProgramSpec,
        metadata: BTreeMap<String, String>,
        cancellation: &crate::Cancellation,
    ) -> Result<Self, LoadError<K>> {
        cancellation.check().map_err(|_| LoadCause::Cancelled)?;
        if !matches!(
            spec,
            ProgramSpec::Tracepoint(_) | ProgramSpec::Xdp(_) | ProgramSpec::Tc(_)
        ) {
            return Err(LoadCause::Unsupported("program type").into());
        }
        let source_text = source
            .to_str()
            .ok_or_else(|| LoadCause::Invalid("source path is not UTF-8".into()))?;

        let object = LocalObject::read(kernel, source, &spec)?;
        cancellation.check().map_err(|_| LoadCause::Cancelled)?;

        Ok(Self {
            object,
            source: source_text.into(),
            spec,
            metadata,
        })
    }
}

impl<
    S: bpfman_store::OpenStore + bpfman_store::CommitLoad,
    K: bpfman_kernel::ProgramObservations + bpfman_kernel::ProgramLoad,
> Bpfman<S, K>
{
    /// Load one prepared local program without attaching it. Private maps are
    /// pinned; failures retain unresolved ownership for an explicit cleanup pass.
    pub fn load(&self, request: PreparedProgram) -> Result<ObservedProgram, LoadError<K>> {
        self.load_with_cancellation(request, &crate::Cancellation::new())
    }

    /// Load with cancellation before commit. Acquired artifacts are compensated
    /// under the same writer lock; successful commit ends cancellation authority.
    #[tracing::instrument(name = "program.load", level = "debug", skip_all, err)]
    pub fn load_with_cancellation(
        &self,
        request: PreparedProgram,
        cancellation: &crate::Cancellation,
    ) -> Result<ObservedProgram, LoadError<K>> {
        self.load_prepared(request, Vec::new(), cancellation)
            .map(|(first, _)| first)
    }

    /// Load a nonempty batch with private maps and a single atomic store commit.
    pub fn load_batch(
        &self,
        request: PreparedPrograms,
    ) -> Result<Vec<ObservedProgram>, LoadError<K>> {
        self.load_batch_with_cancellation(request, &crate::Cancellation::new())
    }

    /// Cancellation before commit compensates all members under the same writer
    /// lock. Failures retain every unresolved receipt for explicit cleanup retry.
    #[tracing::instrument(name = "program.load_batch", level = "debug", skip_all, err)]
    pub fn load_batch_with_cancellation(
        &self,
        request: PreparedPrograms,
        cancellation: &crate::Cancellation,
    ) -> Result<Vec<ObservedProgram>, LoadError<K>> {
        let (first, remaining) =
            self.load_prepared(request.first, request.remaining, cancellation)?;
        Ok(std::iter::once(first).chain(remaining).collect())
    }

    fn load_prepared(
        &self,
        request: PreparedProgram,
        remaining: Vec<ProgramSpec>,
        cancellation: &crate::Cancellation,
    ) -> Result<(ObservedProgram, Vec<ObservedProgram>), LoadError<K>> {
        cancellation.check().map_err(|_| LoadCause::Cancelled)?;
        let PreparedProgram {
            object,
            source,
            spec,
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
                    let remaining: Vec<_> = remaining
                        .iter()
                        .map(|spec| Inputs {
                            cancellation,
                            object: &object,
                            source: &source,
                            spec,
                            metadata: &metadata,
                            created_at: &created_at,
                        })
                        .collect();
                    run_batch(
                        &writer,
                        &mut Effects(store, &self.kernel),
                        &Inputs {
                            cancellation,
                            object: &object,
                            source: &source,
                            spec: &spec,
                            metadata: &metadata,
                            created_at: &created_at,
                        },
                        &remaining,
                    )
                    .map_err(|failure| LoadError {
                        failure: Box::new(failure),
                        retry_lock_error: None,
                    })
                    .and_then(|(first, remaining)| {
                        let committed: Vec<_> = std::iter::once(&first)
                            .chain(&remaining)
                            .map(|p| p.id())
                            .collect();
                        let observe = |stored: StoredProgramSummary| {
                            crate::observation::observe(
                                &self.kernel,
                                store,
                                writer.directory(),
                                stored.id(),
                                crate::observation::View::Load,
                            )
                            .map_err(|source| {
                                LoadError::<K>::from(LoadCause::Observation {
                                    id: stored.id(),
                                    committed: committed.clone(),
                                    source,
                                })
                            })
                        };
                        Ok((
                            observe(first)?,
                            remaining
                                .into_iter()
                                .map(observe)
                                .collect::<Result<_, _>>()?,
                        ))
                    })
                },
            )
            .map_err(LoadCause::from)?
    }
}

// Both single loads and batches use this forward interpreter and finaliser.
#[cfg(test)]
fn run<F: LoadEffects>(
    writer: &RuntimeWriter<'_>,
    effects: &mut F,
    input: &Inputs<'_>,
) -> Result<StoredProgramSummary, FailureFor<F>> {
    run_batch(writer, effects, input, &[]).map(|(first, _)| first)
}

type Acquired<F> = (
    bpfman_core::PersistProgram<
        <F as crate::LoadCleanup>::ProgramPin,
        <F as crate::LoadCleanup>::MapPin,
        <F as crate::LoadCleanup>::Bytecode,
    >,
    <F as CleanupEffects>::MapDirectory,
);

fn run_batch<F: LoadEffects>(
    writer: &RuntimeWriter<'_>,
    effects: &mut F,
    first: &Inputs<'_>,
    remaining: &[Inputs<'_>],
) -> Result<(StoredProgramSummary, Vec<StoredProgramSummary>), FailureFor<F>> {
    check_cancelled(effects, first)?;
    let _store = effects
        .open_store(writer)
        .map_err(Failure::NoOwnedArtifacts)?;
    check_cancelled(effects, first)?;
    let filesystem = effects.prepare(writer).map_err(Failure::NoOwnedArtifacts)?;
    let mut acquired = Vec::new();
    let mut records = Vec::new();

    for input in std::iter::once(first).chain(remaining) {
        match acquire(writer, effects, &filesystem, input) {
            Ok(item) => {
                records.push((F::program_id(item.0.inputs().1), input));
                acquired.push(item);
            }
            Err(primary) => return Err(abort_batch(writer, effects, primary, acquired)),
        }
    }

    // Construct results before the sole commit. Input order is preserved.
    let summaries: Vec<_> = records
        .iter()
        .map(|(id, input)| {
            StoredProgramSummary::new(
                *id,
                input.spec.name().as_str().into(),
                input.spec.kind(),
                input.metadata.clone(),
                Vec::new(),
            )
        })
        .collect();
    let mut summaries = summaries.into_iter();
    let Some(first_summary) = summaries.next() else {
        let cause = effects.batch_aborted();
        return Err(abort_batch(
            writer,
            effects,
            Failure::NoOwnedArtifacts(cause),
            acquired,
        ));
    };
    let remaining_summaries = summaries.collect();

    if let Err(cause) =
        cancellation_cause(effects, first).and_then(|()| effects.persist(writer, &records))
    {
        let primary = match acquired.pop() {
            Some((persist, directory)) => finish(
                writer,
                effects,
                persist.failed(cause),
                Some(directory),
                vec![],
            ),
            None => Failure::NoOwnedArtifacts(cause),
        };
        return Err(abort_batch(writer, effects, primary, acquired));
    }

    for (persist, _directory) in acquired {
        let _committed = persist.committed(());
    }

    Ok((first_summary, remaining_summaries))
}

fn abort_batch<F: LoadEffects>(
    writer: &RuntimeWriter<'_>,
    effects: &mut F,
    primary: FailureFor<F>,
    acquired: Vec<Acquired<F>>,
) -> FailureFor<F> {
    if acquired.is_empty() {
        return primary;
    }

    let mut previous = Vec::new();
    for (persist, directory) in acquired.into_iter().rev() {
        let cause = effects.batch_aborted();
        previous.push(finish(
            writer,
            effects,
            persist.failed(cause),
            Some(directory),
            vec![],
        ));
    }

    Failure::Batch {
        primary: Box::new(primary),
        previous,
    }
}

fn acquire<F: LoadEffects>(
    writer: &RuntimeWriter<'_>,
    effects: &mut F,
    filesystem: &F::Prepared,
    input: &Inputs<'_>,
) -> Result<Acquired<F>, FailureFor<F>> {
    check_cancelled(effects, input)?;
    let operation = LoadProgram::new(input.spec.clone());

    check_cancelled(effects, input)?;
    let mut kernel = effects
        .load_kernel(writer, input)
        .map_err(Failure::NoOwnedArtifacts)?;
    let program_pin = match admission(effects, input, None)
        .and_then(|()| effects.pin_program(writer, filesystem, &mut kernel, input.spec.name()))
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
        .and_then(|()| effects.create_map_directory(writer, filesystem, id))
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

    for name in F::map_names(&kernel) {
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
    Ok((publish.published(bytecode), directory))
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
