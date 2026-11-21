use crate::{
    Bpfman, LoadError, PreparedProgram, PreparedPrograms,
    kernel::LocalObject,
    load_error::{Failure, LoadCause},
};
use bpfman_fs::RuntimeWriter;
use bpfman_lock::AcquireOptions;
use bpfman_model::{ObservedProgram, ProgramSpec, StoredProgramSummary};
use std::{collections::BTreeMap, path::Path};

mod acquisition;
mod effects;
mod real;
#[cfg(test)]
mod tests;

use acquisition::{PublishedBatch, acquire};
use effects::Inputs;
pub(super) use effects::{CleanupEffects, FailureFor, LoadEffects};
pub(super) use real::Effects;

impl PreparedProgram {
    /// Prepare exact image bytes through the same ELF validation boundary used
    /// for file sources. Provenance contains no transport or cache credentials.
    pub fn from_image<K: bpfman_kernel::ProgramLoad>(
        kernel: &K,
        bytes: Vec<u8>,
        source: bpfman_model::ProgramSource,
        spec: ProgramSpec,
        metadata: BTreeMap<String, String>,
        cancellation: &crate::Cancellation,
    ) -> Result<Self, LoadError<K>> {
        cancellation.check().map_err(|_| LoadCause::Cancelled)?;
        if !matches!(source, bpfman_model::ProgramSource::Image { .. }) {
            return Err(
                LoadCause::Invalid("image preparation requires image provenance".into()).into(),
            );
        }
        let object = LocalObject::from_bytes(kernel, bytes, &spec)?;
        cancellation.check().map_err(|_| LoadCause::Cancelled)?;
        Ok(Self {
            owner: String::new(),
            map_owner: None,
            object,
            source,
            spec,
            metadata,
        })
    }

    /// Attribute every program in this load to the caller-selected owner label.
    /// The default is unassigned; this is independent of map-set ownership.
    pub fn with_owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = owner.into();
        self
    }

    /// Join an existing map set. Validation and reuse happen under the writer lock.
    pub fn with_map_owner(mut self, owner: std::num::NonZeroU32) -> Self {
        self.map_owner = Some(owner);
        self
    }

    /// Add program selections from the same captured ELF. Validate
    /// every selection before opening runtime state; preserve input order. Each
    /// occurrence loads independently, including repeated selections, as in Go.
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

        let mut remaining = Vec::new();

        for spec in specs {
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
        let source_text = source
            .to_str()
            .ok_or_else(|| LoadCause::Invalid("source path is not UTF-8".into()))?;

        let object = LocalObject::read(kernel, source, &spec)?;
        cancellation.check().map_err(|_| LoadCause::Cancelled)?;

        Ok(Self {
            owner: String::new(),
            map_owner: None,
            object,
            source: bpfman_model::ProgramSource::File(Some(source_text.into())),
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

    /// Load a nonempty batch with one atomic program/map-membership commit.
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
            owner,
            map_owner,
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
                            owner: &owner,
                            cancellation,
                            map_owner,
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
                            owner: &owner,
                            cancellation,
                            map_owner,
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
    for input in std::iter::once(first).chain(remaining) {
        effects
            .admit_maps(writer, input)
            .map_err(Failure::NoOwnedArtifacts)?;
    }
    check_cancelled(effects, first)?;
    let filesystem = effects.prepare(writer).map_err(Failure::NoOwnedArtifacts)?;
    // Prepare maps, load and pin each program, then publish its bytecode.
    let first_program = acquire(writer, effects, &filesystem, first)?;
    let first_id = first_program.id();
    let mut records = vec![(first_id, first)];
    let mut batch = PublishedBatch::new(first_program);

    for input in remaining {
        match acquire(writer, effects, &filesystem, input) {
            Ok(program) => {
                records.push((program.id(), input));
                batch.push(program);
            }
            Err(primary) => return Err(batch.abort(writer, effects, primary)),
        }
    }

    // Construct results before the sole commit. Input order is preserved.
    let summary = |id, input: &Inputs<'_>| {
        StoredProgramSummary::new(
            id,
            input.spec.name().as_str().into(),
            input.spec.kind(),
            input.metadata.clone(),
            Vec::new(),
        )
    };
    let first_summary = summary(first_id, first);
    let remaining_summaries = records
        .iter()
        .skip(1)
        .map(|(id, input)| summary(*id, input))
        .collect();

    if let Err(cause) =
        cancellation_cause(effects, first).and_then(|()| effects.persist(writer, &records))
    {
        return Err(batch.commit_failed(writer, effects, cause));
    }

    batch.committed();
    Ok((first_summary, remaining_summaries))
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
