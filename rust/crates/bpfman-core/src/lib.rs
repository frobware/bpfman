//! Pure observation policy and SANS-I/O lifecycle decisions.
//!
//! Operation-specific machines will consume observations and return effects as
//! data. Adapters execute those effects outside this crate. Listing is an
//! ordinary pure function over complete store observations. Single-program
//! loading uses consuming continuations to order effects and compensation.

#![no_std]

extern crate alloc;

mod link;
mod list;
mod load;
mod rollback;
mod store;
mod unload;

pub use link::{
    LinkAttempt, LinkCleanup, LinkCleanupContinuation, LinkCleanupKind, LinkCleanupReport,
    LinkCleanupStep, LinkResource,
};
pub use list::{ProgramFilter, list_programs, select_records};
pub use store::plan_store_open;

use alloc::{collections::VecDeque, vec::Vec};
use bpfman_model::ProgramSpec;

/// First effect of a single-program load: load and pin the selected program.
///
/// This is policy, not filesystem authority or proof of kernel state. Adapters
/// must still require their runtime writer. The interpreter validates the ELF
/// and options before beginning, keeps the source snapshot and other context,
/// and executes each effect before reporting its result to the continuation.
/// Adapters return receipts for unresolved partial work on failure. Rollback
/// attempts each owned resource separately. A failed store transaction must
/// leave no committed record. Receipts must be non-cloneable, root-bound
/// capabilities minted by adapters, not arbitrary paths or shared resources.
///
/// Continuations are deliberately neither `Clone` nor `Copy`. They cannot be
/// forged or advanced out of order. Rust cannot prove an interpreter performed
/// an effect truthfully, or prevent it dropping/forgetting a continuation;
/// `must_use` and interpreter tests complement the type-level ordering.
///
/// ```compile_fail
/// use bpfman_core::LoadProgram;
/// use bpfman_model::{ProgramSpec, Symbol};
/// let load = LoadProgram::new(ProgramSpec::Tracepoint(Symbol::try_from("trace").unwrap()));
/// load.published(()); // bytecode cannot be published before loading
/// ```
///
/// A continuation cannot be used twice:
///
/// ```compile_fail
/// use bpfman_core::LoadProgram;
/// use bpfman_model::{ProgramSpec, Symbol};
/// let load = LoadProgram::new(ProgramSpec::Tracepoint(Symbol::try_from("trace").unwrap()));
/// let publish = load.loaded((), Vec::<()>::new());
/// let duplicate = load.loaded((), Vec::<()>::new());
/// ```
#[must_use = "execute the load effect and report its result"]
pub struct LoadProgram {
    spec: ProgramSpec,
}

/// Publish the bytecode snapshot for an already loaded and pinned program.
///
/// `P` and `M` are opaque owned program/map-pin receipts, never cloned, inspected, or
/// dropped by policy, including on failure.
///
/// ```compile_fail
/// use bpfman_core::PublishBytecode;
/// let publish = PublishBytecode::<(), ()> { program_pin: (), map_pins: vec![] };
/// ```
#[must_use = "publish bytecode or begin compensation"]
pub struct PublishBytecode<P, M> {
    load: LoadProgram,
    program_pin: P,
    map_pins: Vec<M>,
}

/// Atomically persist a program and its map-set membership after publication.
///
/// `B` is evidence returned by bytecode publication. No independent optional
/// handle can disagree with the lifecycle phase.
///
/// ```compile_fail
/// use bpfman_core::LoadProgram;
/// use bpfman_model::{ProgramSpec, Symbol};
/// let publish = LoadProgram::new(ProgramSpec::Tracepoint(Symbol::try_from("trace").unwrap()))
///     .loaded((), Vec::<()>::new());
/// publish.committed(()); // persistence requires published bytecode
/// ```
#[must_use = "persist the record or begin compensation"]
pub struct PersistProgram<P, M, B> {
    published: PublishBytecode<P, M>,
    bytecode: B,
}

/// Independent load cleanup instructions. Never arbitrary paths or syscalls.
///
/// Each receipt authorises removal of precisely this load's acquisition. Map
/// pins are individual instructions, so one failure cannot hide later pins.
/// Removing a pin does not assert that the underlying kernel object is gone.
pub enum LoadCompensation<P, M, B> {
    /// Remove an owned staged or published bytecode artifact.
    RemoveBytecode(B),
    /// Remove our program pin and release its owned local handles.
    RemoveProgramPin(P),
    /// Remove one owned map pin, never another owner's/shared pin.
    RemoveMapPin(M),
}

/// Data-free instruction label for diagnostic history, not mutation authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompensationKind {
    /// Owned bytecode cleanup.
    Bytecode,
    /// Owned program-pin cleanup.
    ProgramPin,
    /// One owned map-pin cleanup.
    MapPin,
}

/// Failed effect observation retaining its cause and unresolved work.
///
/// This is data, not a backend error interface. `E` is the interpreter's error
/// type. For cleanup, `R` is the unconsumed receipt; on a forward failure it is
/// the partial acquisitions. On success the adapter consumes its receipt,
/// rather than asking the pure core to run its destructor.
#[must_use = "retain both the cause and unresolved resources"]
pub struct EffectFailure<R, E> {
    /// Resources still owned after the failed effect.
    pub remaining: R,
    /// Failure which prevented completion.
    pub cause: E,
}

/// Unresolved acquisitions after a partial kernel load.
///
/// Every combination is meaningful: loading may fail before creating a program
/// pin, and maps may have been pinned independently. List map pins in acquisition
/// order. Borrowed/shared pins must never appear here.
pub struct KernelAcquisitions<P, M> {
    /// Our program pin, if still owned.
    pub program_pin: Option<P>,
    /// Our individual map pins, if any remain.
    pub map_pins: Vec<M>,
}

/// One full pass over independent load compensations; failures never stop it.
///
/// Retrying is only possible from the terminal report. No failed instruction is
/// reinserted into this pass's pending queue. Dependent/destructive operations
/// must not be added to this instruction set without modelling prerequisites.
/// This does not run I/O, handle cancellation, or perform automatic retries.
/// The runtime interpreter must retain writer authority during a pass.
///
/// ```compile_fail
/// use bpfman_core::LoadProgram;
/// use bpfman_model::{ProgramSpec, Symbol};
/// let rollback = LoadProgram::new(ProgramSpec::Tracepoint(Symbol::try_from("trace").unwrap()))
///     .loaded((), Vec::<()>::new()).published(()).failed("store failed");
/// rollback.committed(());
/// ```
#[must_use = "drain the compensation pass, including after failures"]
pub struct LoadRollback<P, M, B, E> {
    spec: ProgramSpec,
    primary: E,
    pending: VecDeque<PendingCompensation<P, M, B>>,
    unresolved: Vec<PendingCompensation<P, M, B>>,
    attempts: Vec<CompensationAttempt<E>>,
}

/// Next instruction with a continuation accepting only its receipt type.
///
/// A terminal report is produced only when the pass has no unattempted work.
#[must_use = "execute and resume, or retain the terminal report"]
pub enum RollbackStep<P, M, B, E> {
    /// Remove the owned bytecode artifact.
    RemoveBytecode {
        /// Receipt consumed by the adapter, returned only on failure.
        receipt: B,
        /// Continuation for this instruction's observation.
        next: CompensationContinuation<B, P, M, B, E>,
    },
    /// Remove the owned program pin.
    RemoveProgramPin {
        /// Receipt consumed by the adapter, returned only on failure.
        receipt: P,
        /// Continuation for this instruction's observation.
        next: CompensationContinuation<P, P, M, B, E>,
    },
    /// Remove one owned map pin.
    RemoveMapPin {
        /// Receipt consumed by the adapter, returned only on failure.
        receipt: M,
        /// Continuation for this instruction's observation.
        next: CompensationContinuation<M, P, M, B, E>,
    },
    /// Every instruction was attempted; errors and unresolved work are retained.
    Complete(LoadFailure<P, M, B, E>),
}

/// Consuming continuation for exactly one cleanup observation.
///
/// Both outcomes resume the remaining pass, never short-circuiting it.
/// `R` ties a failed observation to the dispatched resource's type.
///
/// A failed map-pin operation cannot return a program-pin receipt:
///
/// ```compile_fail
/// use bpfman_core::{CompensationContinuation, EffectFailure};
/// struct ProgramPin;
/// struct MapPin;
/// struct Bytecode;
/// fn wrong_receipt(next: CompensationContinuation<MapPin, ProgramPin, MapPin, Bytecode, ()>) {
///     next.completed(Err(EffectFailure { remaining: ProgramPin, cause: () }));
/// }
/// ```
///
/// Reporting success does not leave a reusable continuation:
///
/// ```compile_fail
/// use bpfman_core::CompensationContinuation;
/// fn twice(next: CompensationContinuation<(), (), (), (), ()>) {
///     let first = next.completed(Ok(()));
///     let second = next.completed(Ok(()));
/// }
/// ```
#[must_use = "report the outcome and continue the pass"]
pub struct CompensationContinuation<R, P, M, B, E> {
    rollback: LoadRollback<P, M, B, E>,
    id: usize,
    kind: CompensationKind,
    wrap: fn(R) -> LoadCompensation<P, M, B>,
}

/// Unresolved instruction retaining its identity across retry passes.
pub struct PendingCompensation<P, M, B> {
    id: usize,
    instruction: LoadCompensation<P, M, B>,
}

/// One attempted instruction, without ownership authority.
///
/// Identity is local to this load and stable across retries. Receipt-free
/// history retains earlier failures even if a later attempt succeeds.
pub struct CompensationAttempt<E> {
    /// Instruction identity, also found on unresolved instructions.
    pub id: usize,
    /// Instruction kind.
    pub kind: CompensationKind,
    /// Adapter observation; not the original forward-operation failure.
    pub outcome: Result<(), E>,
}

/// Terminal report: original cause, attempt history, unresolved receipts.
///
/// Empty remaining work means compensation completed, not that loading succeeded.
#[must_use = "report the original and cleanup failures and release retained evidence"]
pub struct LoadFailure<P, M, B, E> {
    spec: ProgramSpec,
    primary: E,
    remaining: Vec<PendingCompensation<P, M, B>>,
    attempts: Vec<CompensationAttempt<E>>,
}

/// Terminal successful load; resources are returned to their interpreter.
#[must_use = "retain the committed resources and construct the application result"]
pub struct LoadComplete<P, M, B, S> {
    /// Requested program.
    pub spec: ProgramSpec,
    /// Program-pin receipt, transferred to committed ownership.
    pub program_pin: P,
    /// Individual map-pin receipts, transferred to committed ownership.
    pub map_pins: Vec<M>,
    /// Published bytecode evidence.
    pub bytecode: B,
    /// Evidence of the committed store operation.
    pub stored: S,
}

/// A complete store observation and the evidence that produced it.
///
/// `T` is opaque to policy. The interpreter can supply an opened store; policy
/// only moves it into the decision, never inspects, duplicates, or drops it.
/// Failed observations must be handled before constructing this value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreObservation<T> {
    /// No store was found.
    Missing,
    /// An existing store and its observed schema version.
    Existing {
        /// Observed schema version.
        version: i64,
        /// Evidence retained by the interpreter, such as the opened store.
        evidence: T,
    },
}

/// A store-opening decision retaining all evidence needed to execute it.
///
/// Selecting an existing store without retaining that store is not representable:
///
/// ```compile_fail
/// use bpfman_core::StoreOpenPlan;
/// let decision: StoreOpenPlan<()> = StoreOpenPlan::UseExisting;
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreOpenPlan<T> {
    /// No store exists; create it at the supported schema version.
    Create,
    /// Use the observed store without changing its schema.
    UseExisting(T),
    /// Neither upgrade nor downgrade an existing incompatible store.
    RejectIncompatible {
        /// Observed schema version.
        found: i64,
        /// Schema version supported by the adapter.
        expected: i64,
        /// Rejected evidence, returned so cleanup remains the interpreter's job.
        evidence: T,
    },
}

/// One unload effect, in Go's forward teardown order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnloadKind {
    /// Unpin before any destructive store or artifact cleanup.
    ProgramPin,
    /// Delete the managed record; failure still permits bytecode cleanup.
    ProgramRecord,
    /// Remove one private map pin after record deletion.
    MapPin,
    /// Remove the container after all map pins succeed.
    MapDirectory,
    /// Delete the unused map-set row after its pins and container are gone.
    MapSet,
    /// Independent bytecode cleanup after the program is unpinned.
    Bytecode,
}

/// Owned teardown work, distinct from compensation of an uncommitted load.
pub enum UnloadInstruction<P, R, M, D, S, B> {
    /// Existing program pin.
    ProgramPin(P),
    /// Validated committed program record.
    ProgramRecord(R),
    /// One private map pin.
    MapPin(M),
    /// Private map container.
    MapDirectory(D),
    /// Validated private map-set row.
    MapSet(S),
    /// Published bytecode artifacts.
    Bytecode(B),
}

/// Stable identity and owned receipt for unresolved teardown work.
pub struct PendingUnload<P, R, M, D, S, B> {
    id: usize,
    instruction: UnloadInstruction<P, R, M, D, S, B>,
}

/// Receipt-free history, including successful effects; blocked work is not an attempt.
pub struct UnloadAttempt<E> {
    /// Identity stable across explicit retry passes.
    pub id: usize,
    /// Effect label, never filesystem authority.
    pub kind: UnloadKind,
    /// Observed effect outcome.
    pub outcome: Result<(), E>,
}

/// One bounded forward teardown pass. No rollback or automatic retry.
#[must_use = "execute each permitted effect and retain the report"]
pub struct UnloadProgram<P, R, M, D, S, B, E> {
    pending: VecDeque<PendingUnload<P, R, M, D, S, B>>,
    remaining: Vec<PendingUnload<P, R, M, D, S, B>>,
    attempts: Vec<UnloadAttempt<E>>,
}

/// Terminal unload outcome, including owned residue and all prior outcomes.
#[must_use = "report failures and retain unresolved work for explicit retry"]
pub struct UnloadReport<P, R, M, D, S, B, E> {
    remaining: Vec<PendingUnload<P, R, M, D, S, B>>,
    attempts: Vec<UnloadAttempt<E>>,
}

/// Consuming continuation tied to the dispatched receipt type.
///
/// A failed map removal cannot return program-record evidence:
///
/// ```compile_fail
/// use bpfman_core::{EffectFailure, UnloadContinuation};
/// struct Map;
/// struct Record;
/// fn wrong(next: UnloadContinuation<Map, (), Record, Map, (), (), (), ()>) {
///     next.completed(Err(EffectFailure { remaining: Record, cause: () }));
/// }
/// ```
///
/// Successful completion consumes the continuation:
///
/// ```compile_fail
/// use bpfman_core::UnloadContinuation;
/// fn twice(next: UnloadContinuation<(), (), (), (), (), (), (), ()>) {
///     let first = next.completed(Ok(()));
///     let second = next.completed(Ok(()));
/// }
/// ```
#[must_use = "report the effect and continue teardown"]
pub struct UnloadContinuation<T, P, R, M, D, S, B, E> {
    operation: UnloadProgram<P, R, M, D, S, B, E>,
    id: usize,
    kind: UnloadKind,
    wrap: UnloadWrap<T, P, R, M, D, S, B>,
}

/// Next permitted effect, or a terminal report when the pass is exhausted.
#[must_use = "execute the effect or retain the terminal report"]
pub enum UnloadStep<P, R, M, D, S, B, E> {
    /// Execute ProgramPin teardown.
    ProgramPin {
        /// Owned receipt consumed by the adapter on success.
        receipt: P,
        /// Continuation accepting only this receipt type on failure.
        next: UnloadContinuation<P, P, R, M, D, S, B, E>,
    },
    /// Execute ProgramRecord teardown.
    ProgramRecord {
        /// Owned receipt consumed by the adapter on success.
        receipt: R,
        /// Continuation accepting only this receipt type on failure.
        next: UnloadContinuation<R, P, R, M, D, S, B, E>,
    },
    /// Execute MapPin teardown.
    MapPin {
        /// Owned receipt consumed by the adapter on success.
        receipt: M,
        /// Continuation accepting only this receipt type on failure.
        next: UnloadContinuation<M, P, R, M, D, S, B, E>,
    },
    /// Execute MapDirectory teardown.
    MapDirectory {
        /// Owned receipt consumed by the adapter on success.
        receipt: D,
        /// Continuation accepting only this receipt type on failure.
        next: UnloadContinuation<D, P, R, M, D, S, B, E>,
    },
    /// Execute MapSet teardown.
    MapSet {
        /// Owned receipt consumed by the adapter on success.
        receipt: S,
        /// Continuation accepting only this receipt type on failure.
        next: UnloadContinuation<S, P, R, M, D, S, B, E>,
    },
    /// Execute Bytecode teardown.
    Bytecode {
        /// Owned receipt consumed by the adapter on success.
        receipt: B,
        /// Continuation accepting only this receipt type on failure.
        next: UnloadContinuation<B, P, R, M, D, S, B, E>,
    },
    /// All independent work was attempted once; blocked work remains owned.
    Complete(UnloadReport<P, R, M, D, S, B, E>),
}

type UnloadWrap<T, P, R, M, D, S, B> = fn(T) -> UnloadInstruction<P, R, M, D, S, B>;
