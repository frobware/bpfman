use alloc::{vec, vec::Vec};
use bpfman_model::ProgramSpec;

use crate::{
    EffectFailure, KernelAcquisitions, LoadCompensation, LoadComplete, LoadProgram, LoadRollback,
    PersistProgram, PublishBytecode,
};

impl LoadProgram {
    /// Begin a single-program load, without performing any effects.
    pub fn new(spec: ProgramSpec) -> Self {
        Self { spec }
    }

    /// Selection to pass to the kernel adapter.
    pub fn spec(&self) -> &ProgramSpec {
        &self.spec
    }

    /// Successful loading requires a program receipt and individually owned maps.
    /// Supply map pins in acquisition order, excluding shared pins.
    pub fn loaded<P, M>(self, program_pin: P, map_pins: Vec<M>) -> PublishBytecode<P, M> {
        PublishBytecode {
            load: self,
            program_pin,
            map_pins,
        }
    }

    /// Failed loading schedules every unresolved acquisition for compensation.
    /// `B` is the interpreter's bytecode receipt type; none was acquired here.
    pub fn failed<P, M, B, E>(
        self,
        failure: EffectFailure<KernelAcquisitions<P, M>, E>,
    ) -> LoadRollback<P, M, B, E> {
        let mut instructions = Vec::new();
        if let Some(pin) = failure.remaining.program_pin {
            instructions.push(LoadCompensation::RemoveProgramPin(pin));
        }
        instructions.extend(
            failure
                .remaining
                .map_pins
                .into_iter()
                .rev()
                .map(LoadCompensation::RemoveMapPin),
        );
        LoadRollback::new(self.spec, failure.cause, instructions)
    }
}

impl<P, M> PublishBytecode<P, M> {
    /// Loaded program evidence identifying the publication's owner.
    pub fn program_pin(&self) -> &P {
        &self.program_pin
    }

    /// Successful publication permits an atomic store operation next.
    pub fn published<B>(self, bytecode: B) -> PersistProgram<P, M, B> {
        PersistProgram {
            published: self,
            bytecode,
        }
    }

    /// Failed publication schedules unresolved staging before kernel pins.
    /// Supply artifacts in acquisition order. Return an empty list only if no
    /// artifacts remain. Retain failed local cleanup in the cause and receipts,
    /// never only in logs.
    pub fn failed<B, E>(self, failure: EffectFailure<Vec<B>, E>) -> LoadRollback<P, M, B, E> {
        self.rollback(failure.cause, failure.remaining)
    }

    fn rollback<B, E>(self, primary: E, bytecode: Vec<B>) -> LoadRollback<P, M, B, E> {
        let mut instructions: Vec<_> = bytecode
            .into_iter()
            .rev()
            .map(LoadCompensation::RemoveBytecode)
            .collect();
        instructions.push(LoadCompensation::RemoveProgramPin(self.program_pin));
        instructions.extend(
            self.map_pins
                .into_iter()
                .rev()
                .map(LoadCompensation::RemoveMapPin),
        );
        LoadRollback::new(self.load.spec, primary, instructions)
    }
}

impl<P, M, B> PersistProgram<P, M, B> {
    /// Selection and acquired resources needed by the persistence adapter.
    pub fn inputs(&self) -> (&ProgramSpec, &P, &[M], &B) {
        (
            &self.published.load.spec,
            &self.published.program_pin,
            &self.published.map_pins,
            &self.bytecode,
        )
    }

    /// Persistence succeeded; output-delivery errors must not undo this load.
    pub fn committed<S>(self, stored: S) -> LoadComplete<P, M, B, S> {
        LoadComplete {
            spec: self.published.load.spec,
            program_pin: self.published.program_pin,
            map_pins: self.published.map_pins,
            bytecode: self.bytecode,
            stored,
        }
    }

    /// Persistence failed without a commit; compensate bytecode and every pin.
    pub fn failed<E>(self, error: E) -> LoadRollback<P, M, B, E> {
        self.published.rollback(error, vec![self.bytecode])
    }
}
