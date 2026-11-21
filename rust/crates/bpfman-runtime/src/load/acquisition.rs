//! Forward acquisition and ownership until the batch's store commit.

mod batch;

pub(super) use batch::PublishedBatch;

use super::{FailureFor, Inputs, LoadEffects, cancellation_cause, check_cancelled};
use crate::load_error::finish;
use bpfman_core::{EffectFailure, KernelAcquisitions, LoadProgram, PersistProgram};
use bpfman_fs::RuntimeWriter;
use std::num::NonZeroU32;

pub(super) fn acquire<F: LoadEffects>(
    writer: &RuntimeWriter<'_>,
    effects: &mut F,
    filesystem: &F::Prepared,
    input: &Inputs<'_>,
) -> Result<Published<F>, FailureFor<F>> {
    let mut acquisition = Acquisition {
        writer,
        effects,
        filesystem,
        input,
    };
    let maps = acquisition.prepare_maps()?;
    let loaded = acquisition.load_kernel(maps)?;
    let pinned = acquisition.pin(loaded)?;
    acquisition.publish_bytecode(pinned)
}

struct Acquisition<'a, 'root, 'input, F: LoadEffects> {
    writer: &'a RuntimeWriter<'root>,
    effects: &'a mut F,
    filesystem: &'a F::Prepared,
    input: &'a Inputs<'input>,
}

struct PreparedMaps<F: LoadEffects> {
    operation: LoadProgram,
    pins: Vec<F::MapPin>,
}

struct Loaded<F: LoadEffects> {
    maps: PreparedMaps<F>,
    kernel: F::Kernel,
}

struct Pinned<F: LoadEffects> {
    loaded: Loaded<F>,
    program: F::ProgramPin,
    id: NonZeroU32,
    directory: Option<F::MapDirectory>,
}

pub(super) struct Published<F: LoadEffects> {
    program: PersistProgram<F::ProgramPin, F::MapPin, F::Bytecode>,
    directory: Option<F::MapDirectory>,
}

impl<F: LoadEffects> Published<F> {
    pub(super) fn id(&self) -> NonZeroU32 {
        F::program_id(self.program.inputs().1)
    }

    fn abort(self, writer: &RuntimeWriter<'_>, effects: &mut F, cause: F::Error) -> FailureFor<F> {
        finish(
            writer,
            effects,
            self.program.failed(cause),
            self.directory,
            vec![],
        )
    }

    fn committed(self) {
        let _committed = self.program.committed(());
    }
}

impl<F: LoadEffects> Acquisition<'_, '_, '_, F> {
    fn prepare_maps(&mut self) -> Result<PreparedMaps<F>, FailureFor<F>> {
        check_cancelled(self.effects, self.input)?;
        let operation = LoadProgram::new(self.input.spec.clone());

        check_cancelled(self.effects, self.input)?;
        let mut maps = PreparedMaps::<F> {
            operation,
            pins: Vec::new(),
        };
        if self.input.map_owner.is_none() {
            for name in &self.input.object.shared_maps {
                match self.admit(Vec::new()).and_then(|()| {
                    self.effects.prepare_maps(
                        self.writer,
                        self.filesystem,
                        self.input,
                        std::slice::from_ref(name),
                    )
                }) {
                    Ok(pins) => maps.pins.extend(pins),
                    Err(failure) => {
                        maps.pins.extend(failure.remaining);
                        return Err(self.abort(maps, None, None, failure.cause));
                    }
                }
            }
        }
        Ok(maps)
    }

    fn load_kernel(&mut self, maps: PreparedMaps<F>) -> Result<Loaded<F>, FailureFor<F>> {
        match cancellation_cause(self.effects, self.input).and_then(|()| {
            self.effects
                .load_kernel(self.writer, self.input, &maps.pins)
        }) {
            Ok(kernel) => Ok(Loaded { maps, kernel }),
            Err(cause) => Err(self.abort(maps, None, None, cause)),
        }
    }

    fn pin(&mut self, mut loaded: Loaded<F>) -> Result<Pinned<F>, FailureFor<F>> {
        let program = match self.admit(None).and_then(|()| {
            self.effects.pin_program(
                self.writer,
                self.filesystem,
                &mut loaded.kernel,
                self.input.spec.name(),
            )
        }) {
            Ok(program) => program,
            Err(failure) => {
                return Err(self.abort(loaded.maps, failure.remaining, None, failure.cause));
            }
        };
        let id = F::program_id(&program);
        let directory = if self.input.map_owner.is_some() {
            None
        } else {
            match self.admit(None).and_then(|()| {
                self.effects
                    .create_map_directory(self.writer, self.filesystem, id)
            }) {
                Ok(directory) => Some(directory),
                Err(failure) => {
                    return Err(self.abort(
                        loaded.maps,
                        Some(program),
                        failure.remaining,
                        failure.cause,
                    ));
                }
            }
        };

        for name in F::map_names(&loaded.kernel) {
            let Some(ref container) = directory else {
                break;
            };
            match self.admit(None).and_then(|()| {
                self.effects
                    .pin_map(self.writer, &loaded.kernel, container, name)
            }) {
                Ok(pin) => loaded.maps.pins.push(pin),
                Err(failure) => {
                    loaded.maps.pins.extend(failure.remaining);
                    return Err(self.abort(loaded.maps, Some(program), directory, failure.cause));
                }
            }
        }

        Ok(Pinned {
            loaded,
            program,
            id,
            directory,
        })
    }

    fn publish_bytecode(&mut self, pinned: Pinned<F>) -> Result<Published<F>, FailureFor<F>> {
        let Pinned {
            loaded,
            program,
            id,
            directory,
        } = pinned;
        let publish = loaded.maps.operation.loaded(program, loaded.maps.pins);
        match self.admit(Vec::new()).and_then(|()| {
            self.effects
                .publish(self.writer, &loaded.kernel, id, self.input)
        }) {
            Ok(bytecode) => Ok(Published {
                program: publish.published(bytecode),
                directory,
            }),
            Err(failure) => Err(finish(
                self.writer,
                self.effects,
                publish.failed(failure),
                directory,
                vec![],
            )),
        }
    }

    fn admit<R>(&self, remaining: R) -> Result<(), EffectFailure<R, F::Error>> {
        cancellation_cause(self.effects, self.input)
            .map_err(|cause| EffectFailure { cause, remaining })
    }

    fn abort(
        &mut self,
        maps: PreparedMaps<F>,
        program_pin: Option<F::ProgramPin>,
        directory: Option<F::MapDirectory>,
        cause: F::Error,
    ) -> FailureFor<F> {
        let rollback = maps.operation.failed(EffectFailure {
            cause,
            remaining: KernelAcquisitions {
                program_pin,
                map_pins: maps.pins,
            },
        });
        finish(self.writer, self.effects, rollback, directory, vec![])
    }
}
