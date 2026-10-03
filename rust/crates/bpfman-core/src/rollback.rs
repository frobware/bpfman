use alloc::{collections::VecDeque, vec::Vec};
use bpfman_model::ProgramSpec;

use crate::{
    CompensationAttempt, CompensationContinuation, CompensationKind, EffectFailure,
    LoadCompensation, LoadFailure, LoadRollback, PendingCompensation, RollbackStep,
};

impl<P, M, B> LoadCompensation<P, M, B> {
    /// Diagnostic label derived from the instruction variant.
    pub fn kind(&self) -> CompensationKind {
        match self {
            Self::RemoveBytecode(_) => CompensationKind::Bytecode,
            Self::RemoveProgramPin(_) => CompensationKind::ProgramPin,
            Self::RemoveMapPin(_) => CompensationKind::MapPin,
        }
    }
}

impl<P, M, B, E> LoadRollback<P, M, B, E> {
    pub(super) fn new(
        spec: ProgramSpec,
        primary: E,
        instructions: Vec<LoadCompensation<P, M, B>>,
    ) -> Self {
        Self {
            spec,
            primary,
            pending: instructions
                .into_iter()
                .enumerate()
                .map(|(id, instruction)| PendingCompensation { id, instruction })
                .collect(),
            unresolved: Vec::new(),
            attempts: Vec::new(),
        }
    }

    /// Inspect scheduled instructions without consuming or duplicating receipts.
    pub fn pending(&self) -> impl ExactSizeIterator<Item = &PendingCompensation<P, M, B>> {
        self.pending.iter()
    }

    /// Dispatch one instruction. The next pass state is only available after
    /// supplying this instruction's observation to its consuming continuation.
    pub fn next(mut self) -> RollbackStep<P, M, B, E> {
        let Some(PendingCompensation { id, instruction }) = self.pending.pop_front() else {
            return RollbackStep::Complete(LoadFailure {
                spec: self.spec,
                primary: self.primary,
                remaining: self.unresolved,
                attempts: self.attempts,
            });
        };
        let kind = instruction.kind();

        match instruction {
            LoadCompensation::RemoveBytecode(receipt) => RollbackStep::RemoveBytecode {
                receipt,
                next: CompensationContinuation {
                    rollback: self,
                    id,
                    kind,
                    wrap: LoadCompensation::RemoveBytecode,
                },
            },
            LoadCompensation::RemoveProgramPin(receipt) => RollbackStep::RemoveProgramPin {
                receipt,
                next: CompensationContinuation {
                    rollback: self,
                    id,
                    kind,
                    wrap: LoadCompensation::RemoveProgramPin,
                },
            },
            LoadCompensation::RemoveMapPin(receipt) => RollbackStep::RemoveMapPin {
                receipt,
                next: CompensationContinuation {
                    rollback: self,
                    id,
                    kind,
                    wrap: LoadCompensation::RemoveMapPin,
                },
            },
        }
    }
}

impl<R, P, M, B, E> CompensationContinuation<R, P, M, B, E> {
    /// Resume on success OR failure. Failure retains its receipt for a later
    /// pass, never appending it to the current queue. Policy drops no receipts
    /// or causes; the successful adapter has already consumed its resource.
    pub fn completed(
        mut self,
        result: Result<(), EffectFailure<R, E>>,
    ) -> LoadRollback<P, M, B, E> {
        let outcome = match result {
            Ok(()) => Ok(()),
            Err(EffectFailure { remaining, cause }) => {
                self.rollback.unresolved.push(PendingCompensation {
                    id: self.id,
                    instruction: (self.wrap)(remaining),
                });

                Err(cause)
            }
        };
        self.rollback.attempts.push(CompensationAttempt {
            id: self.id,
            kind: self.kind,
            outcome,
        });
        self.rollback
    }
}

impl<P, M, B> PendingCompensation<P, M, B> {
    /// Stable identity within this load, including subsequent retry passes.
    pub fn id(&self) -> usize {
        self.id
    }

    /// Inspect the instruction and its receipt without taking ownership.
    pub fn instruction(&self) -> &LoadCompensation<P, M, B> {
        &self.instruction
    }
}

impl<P, M, B, E> LoadFailure<P, M, B, E> {
    /// Requested program; a clean rollback still means the load failed.
    pub fn spec(&self) -> &ProgramSpec {
        &self.spec
    }

    /// Original forward-operation failure, retained unchanged across retries.
    pub fn primary(&self) -> &E {
        &self.primary
    }

    /// Every cleanup observation in execution order, including earlier passes.
    pub fn attempts(&self) -> &[CompensationAttempt<E>] {
        &self.attempts
    }

    /// Only unresolved instructions. Successful cleanup has no retained receipt.
    pub fn remaining(&self) -> &[PendingCompensation<P, M, B>] {
        &self.remaining
    }

    /// Explicitly begin another pass over unresolved work only, retaining the
    /// primary failure, diagnostic history, instruction identities and order.
    /// Caller owns retry budgeting and must revalidate any newly acquired scope.
    pub fn retry(self) -> LoadRollback<P, M, B, E> {
        LoadRollback {
            spec: self.spec,
            primary: self.primary,
            pending: VecDeque::from(self.remaining),
            unresolved: Vec::new(),
            attempts: self.attempts,
        }
    }
}
