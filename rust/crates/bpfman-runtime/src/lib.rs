//! Application operations: gather observations, then evaluate pure policy.

mod compensation;
mod error;
mod kernel;
mod list;
mod observation;

pub use observation::{get_program, list_program_entries};

mod load;
mod load_error;

pub use load::load_tracepoint;

/// Validated local tracepoint input, prepared before opening runtime state.
/// Owns the exact ELF bytes that will be loaded and published.
pub struct PreparedTracepoint {
    layout: bpfman_fs::RuntimeLayout,
    object: kernel::LocalObject,
    source: String,
    name: bpfman_model::Symbol,
    metadata: std::collections::BTreeMap<String, String>,
}

mod store;
pub use store::ActiveStore;
mod unload;
mod unload_error;

pub use unload::unload_tracepoint;

pub use compensation::compensate_load;
pub use list::list_programs;

/// Narrow filesystem-effects boundary for failed-load cleanup.
///
/// The real implementation must delegate managed-object removal to `bpfman-fs`;
/// it must not implement path traversal/unlinking here. Tests may inject a fake.
/// Associated receipt types identify owned resources, not arbitrary paths. The
/// implementation must check receipts belong to the supplied writer's root and
/// retain unresolved receipts on failure, including partially completed work.
/// Methods must be safe to attempt independently and must not remove shared or
/// pre-existing resources. Container cleanup needing prerequisites is outside
/// this interface. No guarantees about destroying kernel objects are implied
/// by successfully removing pins.
pub trait LoadCleanup {
    /// Non-cloneable program-pin ownership receipt.
    type ProgramPin;

    /// Non-cloneable ownership receipt for a single map pin.
    type MapPin;

    /// Non-cloneable ownership receipt for staged or published bytecode.
    type Bytecode;

    /// Application-classified error; concrete backend causes stay private.
    type Error: std::error::Error;

    /// Remove this owned bytecode artifact; success consumes the receipt.
    fn remove_bytecode(
        &mut self,
        writer: &bpfman_fs::RuntimeWriter<'_>,
        receipt: Self::Bytecode,
    ) -> Result<(), bpfman_core::EffectFailure<Self::Bytecode, Self::Error>>;

    /// Remove this owned program pin; success consumes the receipt.
    fn remove_program_pin(
        &mut self,
        writer: &bpfman_fs::RuntimeWriter<'_>,
        receipt: Self::ProgramPin,
    ) -> Result<(), bpfman_core::EffectFailure<Self::ProgramPin, Self::Error>>;

    /// Remove exactly this owned map pin, not a whole map directory.
    fn remove_map_pin(
        &mut self,
        writer: &bpfman_fs::RuntimeWriter<'_>,
        receipt: Self::MapPin,
    ) -> Result<(), bpfman_core::EffectFailure<Self::MapPin, Self::Error>>;
}

/// Backend-independent application failure classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// Runtime state could not be created, opened, or read.
    Unavailable,
    /// The writer lock could not be acquired within its wait budget.
    TimedOut,
    /// The writer-lock acquisition was cancelled.
    Cancelled,
    /// Stored state requires another schema version or initialisation.
    IncompatibleState,
    /// Stored observations violate domain invariants.
    InvalidState,
}

/// Application failure; concrete adapter errors remain private diagnostics.
#[derive(Debug, thiserror::Error)]
#[error("list managed programs")]
pub struct Error {
    kind: ErrorKind,
    #[source]
    source: error::Failure,
}

/// A failed load, retaining original diagnostics and unresolved cleanup receipts.
pub struct LoadError {
    failure: Box<load_error::Failure>,
}

/// Backend-independent classification for the supported load operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadErrorKind {
    /// The local ELF or supplied request is invalid.
    InvalidInput,
    /// The requested capability is outside the implemented slice.
    Unsupported,
    /// An operating-system, kernel, filesystem, lock, or store operation failed.
    Unavailable,
}

/// Backend-independent classification of an unload failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnloadErrorKind {
    /// No managed record exists for this ID; no kernel-only identity is adopted.
    NotFound,
    /// Linked programs, other program types, or shared maps need a later slice.
    Unsupported,
    /// Stored paths or observed objects violate the ownership contract.
    InvalidState,
    /// An OS, lock, kernel-observation, or store operation failed.
    Unavailable,
}

/// Opaque cause of an unload effect, with backend sources kept private.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct UnloadCause {
    cause: unload_error::Cause,
}

/// Unload progress and residue, including successful steps and earlier failures.
/// A successful operation may retain cleanup warnings, matching Go's contract.
#[must_use = "inspect cleanup warnings and retain any unresolved work"]
pub struct UnloadReport<S: bpfman_store::UnloadStore> {
    report: unload::StoreReport<S>,
}

/// Unload failure, retaining progress and receipts if teardown began.
pub struct UnloadError<S: bpfman_store::UnloadStore> {
    failure: unload_error::Failure<S>,
}

/// Classification of a full program-observation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationErrorKind {
    /// No managed record exists.
    NotFound,
    /// Recorded in the store snapshot but absent in the later kernel observation.
    /// This can occur during concurrent unload; it does not prove inconsistency.
    KernelMissing,
    /// Full link observation needs a later slice.
    Unsupported,
    /// An observation failed; it must not be presented as absence.
    Unavailable,
}

/// Opaque failure while observing a managed program.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct ObservationError {
    cause: observation::Failure,
}

#[cfg(test)]
#[path = "../../../tests/observation.rs"]
mod sample;
