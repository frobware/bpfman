use crate::{
    EffectFailure, PendingUnload, UnloadAttempt, UnloadContinuation, UnloadInstruction, UnloadKind,
    UnloadProgram, UnloadReport, UnloadStep,
};
use alloc::{collections::VecDeque, vec::Vec};

impl<P, R, M, D, S, B> UnloadInstruction<P, R, M, D, S, B> {
    /// Diagnostic label for this instruction.
    pub fn kind(&self) -> UnloadKind {
        match self {
            Self::ProgramPin(..) => UnloadKind::ProgramPin,
            Self::ProgramRecord(..) => UnloadKind::ProgramRecord,
            Self::MapPin(..) => UnloadKind::MapPin,
            Self::MapDirectory(..) => UnloadKind::MapDirectory,
            Self::MapSet(..) => UnloadKind::MapSet,
            Self::Bytecode(..) => UnloadKind::Bytecode,
        }
    }
}
impl<P, R, M, D, S, B> PendingUnload<P, R, M, D, S, B> {
    /// Stable instruction identity.
    pub fn id(&self) -> usize {
        self.id
    }
    /// Receipt-bearing instruction, borrowed without transferring authority.
    pub fn instruction(&self) -> &UnloadInstruction<P, R, M, D, S, B> {
        &self.instruction
    }
}
impl<P, R, M, D, S, B, E> UnloadProgram<P, R, M, D, S, B, E> {
    /// Begin teardown from a fully validated snapshot under the writer lock.
    /// Missing filesystem artifacts are already satisfied, not deletion targets.
    pub fn new(
        pin: Option<P>,
        record: R,
        maps: Vec<M>,
        directory: Option<D>,
        map_set: S,
        bytecode: Option<B>,
    ) -> Self {
        let mut instructions = Vec::new();
        instructions.extend(pin.map(UnloadInstruction::ProgramPin));
        instructions.push(UnloadInstruction::ProgramRecord(record));
        instructions.extend(maps.into_iter().map(UnloadInstruction::MapPin));
        instructions.extend(directory.map(UnloadInstruction::MapDirectory));
        instructions.push(UnloadInstruction::MapSet(map_set));
        instructions.extend(bytecode.map(UnloadInstruction::Bytecode));
        Self {
            pending: instructions
                .into_iter()
                .enumerate()
                .map(|(id, instruction)| PendingUnload { id, instruction })
                .collect(),
            remaining: Vec::new(),
            attempts: Vec::new(),
        }
    }
    /// Dispatch the next effect whose prerequisites succeeded in this pass.
    pub fn next(mut self) -> UnloadStep<P, R, M, D, S, B, E> {
        while let Some(work) = self.pending.pop_front() {
            let kind = work.instruction.kind();
            let blocked = self
                .remaining
                .iter()
                .any(|earlier| match earlier.instruction.kind() {
                    UnloadKind::ProgramPin => true,
                    UnloadKind::ProgramRecord => !matches!(kind, UnloadKind::Bytecode),
                    UnloadKind::MapPin => {
                        matches!(kind, UnloadKind::MapDirectory | UnloadKind::MapSet)
                    }
                    UnloadKind::MapDirectory => kind == UnloadKind::MapSet,
                    UnloadKind::MapSet | UnloadKind::Bytecode => false,
                });
            if blocked {
                self.remaining.push(work);
                continue;
            }
            let id = work.id;
            return match work.instruction {
                UnloadInstruction::ProgramPin(receipt) => UnloadStep::ProgramPin {
                    receipt,
                    next: UnloadContinuation {
                        operation: self,
                        id,
                        kind,
                        wrap: UnloadInstruction::ProgramPin,
                    },
                },
                UnloadInstruction::ProgramRecord(receipt) => UnloadStep::ProgramRecord {
                    receipt,
                    next: UnloadContinuation {
                        operation: self,
                        id,
                        kind,
                        wrap: UnloadInstruction::ProgramRecord,
                    },
                },
                UnloadInstruction::MapPin(receipt) => UnloadStep::MapPin {
                    receipt,
                    next: UnloadContinuation {
                        operation: self,
                        id,
                        kind,
                        wrap: UnloadInstruction::MapPin,
                    },
                },
                UnloadInstruction::MapDirectory(receipt) => UnloadStep::MapDirectory {
                    receipt,
                    next: UnloadContinuation {
                        operation: self,
                        id,
                        kind,
                        wrap: UnloadInstruction::MapDirectory,
                    },
                },
                UnloadInstruction::MapSet(receipt) => UnloadStep::MapSet {
                    receipt,
                    next: UnloadContinuation {
                        operation: self,
                        id,
                        kind,
                        wrap: UnloadInstruction::MapSet,
                    },
                },
                UnloadInstruction::Bytecode(receipt) => UnloadStep::Bytecode {
                    receipt,
                    next: UnloadContinuation {
                        operation: self,
                        id,
                        kind,
                        wrap: UnloadInstruction::Bytecode,
                    },
                },
            };
        }
        UnloadStep::Complete(UnloadReport {
            remaining: self.remaining,
            attempts: self.attempts,
        })
    }
}
impl<T, P, R, M, D, S, B, E> UnloadContinuation<T, P, R, M, D, S, B, E> {
    /// Record success or retained failure, without retrying it during this pass.
    pub fn completed(
        mut self,
        outcome: Result<(), EffectFailure<T, E>>,
    ) -> UnloadProgram<P, R, M, D, S, B, E> {
        let outcome = outcome.map_err(|failure| {
            self.operation.remaining.push(PendingUnload {
                id: self.id,
                instruction: (self.wrap)(failure.remaining),
            });
            failure.cause
        });
        self.operation.attempts.push(UnloadAttempt {
            id: self.id,
            kind: self.kind,
            outcome,
        });
        self.operation
    }
}
impl<P, R, M, D, S, B, E> UnloadReport<P, R, M, D, S, B, E> {
    /// Go's operation status: post-record artifact failures are warnings.
    pub fn failed(&self) -> bool {
        self.remaining.iter().any(|work| {
            matches!(
                work.instruction.kind(),
                UnloadKind::ProgramPin | UnloadKind::ProgramRecord
            )
        })
    }
    /// Ordered history, including successful steps and previous passes.
    pub fn attempts(&self) -> &[UnloadAttempt<E>] {
        &self.attempts
    }
    /// Unresolved and blocked work; successful receipts are absent.
    pub fn remaining(&self) -> &[PendingUnload<P, R, M, D, S, B>] {
        &self.remaining
    }
    /// Explicitly begin one more pass over unresolved work only.
    pub fn retry(self) -> UnloadProgram<P, R, M, D, S, B, E> {
        UnloadProgram {
            pending: VecDeque::from(self.remaining),
            remaining: Vec::new(),
            attempts: self.attempts,
        }
    }
}
