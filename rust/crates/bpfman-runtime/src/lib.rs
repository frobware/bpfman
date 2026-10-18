//! A bpfman instance, bound to its runtime and persistence backend.
//!
//! Open the selected store once, then inject it and a kernel backend into
//! [`Bpfman`]. Read methods
//! borrow the instance without acquiring the writer lock. Mutations and explicit
//! cleanup passes acquire scoped writer authority internally. The library never
//! installs signal handlers, a telemetry subscriber, or formats command output.
//! Use the `*_with_cancellation` methods with a caller-owned [`Cancellation`] to
//! stop at safe operation boundaries. Ordinary methods use an independent token.
//! Once commit or teardown starts, its outcome is preserved; compensation runs
//! with the writer lock held regardless of the forward request's cancellation.
//!
//! ```no_run
//! use bpfman_runtime::{ActiveStore, Bpfman, Error};
//! use bpfman_fs::RuntimeLayout;
//! use bpfman_store::OpenStore;
//! use std::time::Duration;
//!
//! fn list<S: OpenStore, K>(backend: S, kernel: K, layout: &RuntimeLayout) -> Result<(), Error> {
//!     let timeout = Duration::from_secs(30);
//!     let store = ActiveStore::open(backend, layout, timeout)?;
//!     let bpfman = Bpfman::new(store, kernel, timeout);
//!     let programs = bpfman.list(&Default::default())?;
//!     // The caller chooses how to use or present these domain values.
//!     Ok(())
//! }
//! ```

mod xdp;
pub use xdp::{XdpAttach, XdpError, XdpReport};

mod application;
mod cancellation;
mod compensation;
mod delete;
mod delete_error;
mod detach;
mod dispatcher;
mod error;
mod kernel;
mod link;
mod link_error;
mod link_observation;
mod list;
mod load;
mod load_error;
mod observation;
mod store;
mod unload;
mod unload_error;
mod unload_tc;
pub use unload_tc::UnloadTcAttempt;
mod unload_xdp;
pub use unload_xdp::UnloadXdpAttempt;

/// An instance bound to an initialized store, runtime, and kernel backend.
///
/// Reads, loading, attachment, teardown, and explicit retries use the same
/// injected backend. Each operation requires only its kernel capabilities;
/// associated handles and receipts preserve backend-owned resource lifetimes.
///
/// Construct the active store at startup, then move it into this application.
/// Operations share the adopted runtime, not an operation-wide mutex. Reads
/// need no writer lock; mutations acquire scoped authority for this runtime.
/// The caller owns telemetry collection and presentation.
///
/// Application dependencies cannot be replaced through the public API:
/// ```compile_fail,E0616
/// use bpfman_runtime::Bpfman;
/// fn replace<S: bpfman_store::OpenStore, K>(app: &mut Bpfman<S, K>) {
///     let _store = &mut app.store;
/// }
/// ```
///
/// Construction requires an initialized store, not a backend selector:
/// ```compile_fail,E0308
/// use bpfman_runtime::Bpfman;
/// fn uninitialized<S>(backend: S) {
///     let _app = Bpfman::new(backend, (), std::time::Duration::from_secs(1));
/// }
/// ```
pub struct Bpfman<S: bpfman_store::OpenStore, K> {
    kernel: K,
    store: ActiveStore<S>,
    lock_timeout: std::time::Duration,
}

/// A caller-owned, one-way cancellation request for an operation.
/// Clones share the request; independent operations can use independent tokens.
/// Cancellation is cooperative and never interrupts compensation or a commit.
#[derive(Clone, Debug, Default)]
pub struct Cancellation {
    flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Validated local program input, prepared before opening runtime state.
/// Owns the exact ELF bytes that will be loaded and published.
/// Request contents are private and cannot be changed after validation:
/// ```compile_fail,E0616
/// use bpfman_runtime::PreparedProgram;
/// fn change(request: &mut PreparedProgram) {
///     request.source = "another.o".into();
/// }
/// ```
pub struct PreparedProgram {
    map_owner: Option<std::num::NonZeroU32>,
    object: kernel::LocalObject,
    source: bpfman_model::ProgramSource,
    spec: bpfman_model::ProgramSpec,
    metadata: std::collections::BTreeMap<String, String>,
}

/// Validated, nonempty local program batch sharing one captured ELF snapshot.
/// Each member creates or joins a map set; persistence publishes the entire batch atomically.
pub struct PreparedPrograms {
    first: PreparedProgram,
    remaining: Vec<bpfman_model::ProgramSpec>,
}

/// Store opened at startup and bound to an adopted runtime directory.
/// Only absent state is initialized, under the writer lock. Move this handle
/// into `Bpfman` to use the application API without supplying runtime paths.
pub struct ActiveStore<S: bpfman_store::OpenStore> {
    backend: S,
    reader: S::Reader,
    runtime: bpfman_fs::RuntimeDirectory,
}

/// Kernel function attachment intent; entry/return selection comes from the loaded program.
pub struct KprobeAttach {
    /// Managed program identity.
    pub program_id: std::num::NonZeroU32,
    /// Validated kernel function name.
    pub function: bpfman_model::KernelFunction,
    /// Byte offset from the function entry.
    pub offset: u64,
    /// Operator labels for this attachment.
    pub metadata: std::collections::BTreeMap<String, String>,
}

/// Host user-space attachment intent. Entry/return selection comes from the program.
pub struct UprobeAttach {
    /// Managed program identity.
    pub program_id: std::num::NonZeroU32,
    /// Validated host target, symbol/offset and PID filter.
    pub target: bpfman_model::UprobeTarget,
    /// Operator labels.
    pub metadata: std::collections::BTreeMap<String, String>,
}

/// Validated tracepoint attachment intent for an already loaded program.
pub struct TracepointAttach {
    /// Managed program identity.
    pub program_id: std::num::NonZeroU32,
    /// Validated event group and name.
    pub target: bpfman_model::Tracepoint,
    /// Operator labels for this attachment.
    pub metadata: std::collections::BTreeMap<String, String>,
}

/// Backend-independent attachment and detachment failure categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkErrorKind {
    /// Cancelled at an admission or forward-effect boundary.
    Cancelled,
    /// Managed program or link does not exist.
    NotFound,
    /// The stored program or attachment type is outside this slice.
    Unsupported,
    /// Stored evidence or filesystem identity is invalid.
    InvalidState,
    /// The writer lock wait budget expired.
    TimedOut,
    /// A store, filesystem, or kernel operation failed.
    Unavailable,
}

/// Opaque cause of a link operation or cleanup attempt.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct LinkCause {
    cause: link_error::Cause,
}

/// Link cleanup history and retained ownership. Failed attachment retains its
/// original cause even after a successful explicit cleanup pass.
#[must_use = "inspect progress and retain unresolved link cleanup"]
pub struct LinkReport<S: bpfman_store::LinkStore, K: bpfman_kernel::TracepointLinks> {
    id: std::num::NonZeroU64,
    primary: Option<LinkCause>,
    cleanup: link::StoreReport<S, K>,
}

/// Failed link operation, retaining all progress and unresolved cleanup receipts.
pub struct LinkError<S: bpfman_store::LinkStore, K: bpfman_kernel::TracepointLinks> {
    failure: link_error::Failure<S, K>,
}

/// One ordered detach result, retaining the attachment family's cleanup ownership.
#[must_use = "inspect each result and retain unresolved cleanup"]
pub struct DetachOutcome<
    S: bpfman_store::LinkStore + bpfman_store::XdpStore + bpfman_store::TcStore,
    K: bpfman_kernel::TracepointLinks + bpfman_kernel::XdpReplacement + bpfman_kernel::TcLifecycle,
> {
    /// Requested managed link ID; duplicates have separate outcomes.
    pub id: std::num::NonZeroU64,
    /// Completed progress or a failure retaining unresolved cleanup.
    pub outcome: Result<DetachReport<S, K>, DetachError<S, K>>,
}

/// Completed detach progress, preserving the original family's report.
#[must_use = "inspect progress and retain unresolved cleanup"]
pub enum DetachReport<
    S: bpfman_store::LinkStore + bpfman_store::XdpStore + bpfman_store::TcStore,
    K: bpfman_kernel::TracepointLinks + bpfman_kernel::XdpReplacement + bpfman_kernel::TcLifecycle,
> {
    /// Standalone attachment cleanup.
    Standalone(LinkReport<S, K>),
    /// XDP dispatcher member cleanup.
    Xdp(XdpReport<S, K>),
    /// Legacy TC dispatcher member cleanup.
    Tc(TcReport<S, K>),
}

/// Detach failure with the original family's consuming retry ownership.
pub enum DetachError<
    S: bpfman_store::LinkStore + bpfman_store::XdpStore + bpfman_store::TcStore,
    K: bpfman_kernel::TracepointLinks + bpfman_kernel::XdpReplacement + bpfman_kernel::TcLifecycle,
> {
    /// Failure while selecting stored intent, before teardown starts.
    Observation(LinkCause),
    /// Standalone attachment failure.
    Standalone(LinkError<S, K>),
    /// XDP dispatcher member failure.
    Xdp(XdpError<S, K>),
    /// Legacy TC dispatcher member failure.
    Tc(TcError<S, K>),
}

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
trait LoadCleanup {
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
    /// The operation was cancelled at a safe boundary.
    Cancelled,
    /// Stored state requires another schema version or initialisation.
    IncompatibleState,
    /// Stored observations violate domain invariants.
    InvalidState,
}

/// Application failure; concrete adapter errors remain private diagnostics.
#[derive(Debug, thiserror::Error)]
#[error("access bpfman runtime")]
pub struct Error {
    kind: ErrorKind,
    #[source]
    source: error::Failure,
}

/// A failed load, retaining original diagnostics and unresolved cleanup receipts.
pub struct LoadError<K: bpfman_kernel::ProgramResources> {
    failure: Box<load_error::KernelFailure<K>>,
    retry_lock_error: Option<Error>,
}

/// Backend-independent classification for the supported load operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoadErrorKind {
    /// The operation was cancelled before its completion boundary.
    Cancelled,
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
    /// The operation was cancelled before its completion boundary.
    Cancelled,
    /// No managed record exists for this ID; no kernel-only identity is adopted.
    NotFound,
    /// Other program types or shared maps need a later slice.
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
pub struct UnloadReport<
    S: bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpStore
        + bpfman_store::TcStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement
        + bpfman_kernel::TcLifecycle,
> {
    report: unload::StoreReport<S, K>,
    xdp: unload_xdp::Progress<S, K>,
    tc: unload_tc::Progress<S, K>,
}

/// One ordered batch result. Inspect warnings and retain unresolved cleanup
/// exactly as for a single-program unload.
#[must_use = "inspect each result and retain unresolved cleanup"]
pub struct UnloadOutcome<
    S: bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpStore
        + bpfman_store::TcStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement
        + bpfman_kernel::TcLifecycle,
> {
    /// The requested managed program ID. Duplicates retain separate outcomes.
    pub id: std::num::NonZeroU32,
    /// Completed progress or a failure retaining any unresolved cleanup.
    pub outcome: Result<UnloadReport<S, K>, UnloadError<S, K>>,
}

/// Unload failure, retaining progress and receipts if teardown began.
pub struct UnloadError<
    S: bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpStore
        + bpfman_store::TcStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement
        + bpfman_kernel::TcLifecycle,
> {
    failure: unload_error::Failure<S, K>,
}

/// Classification of a full program-observation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationErrorKind {
    /// The operation was cancelled before its completion boundary.
    Cancelled,
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

mod tc;
pub use tc::{TcAttach, TcError, TcReport};

/// Native TCX attachment request. Lower priority values run first; ties retain
/// attachment order. Namespace and hook identity are resolved during admission.
pub struct TcxAttach {
    /// Loaded native classifier program.
    pub program_id: std::num::NonZeroU32,
    /// Selected interface.
    pub interface: bpfman_model::InterfaceName,
    /// Network namespace selector.
    pub netns: bpfman_model::NetworkNamespace,
    /// Ingress or egress hook.
    pub direction: bpfman_model::TcDirection,
    /// Nonnegative operator priority, at most i32::MAX.
    pub priority: u32,
    /// Operator labels.
    pub metadata: std::collections::BTreeMap<String, String>,
}

/// Attach a function tracing or LSM program to its load-time target.
pub struct TracingAttach {
    /// Loaded program ID.
    pub program_id: std::num::NonZeroU32,
    /// Required tracing or LSM program family.
    pub kind: bpfman_model::TracingKind,
    /// Operator labels.
    pub metadata: std::collections::BTreeMap<String, String>,
}

/// Program selection for cascading deletion.
#[derive(Clone, Copy, Debug)]
pub enum ProgramDelete<'a> {
    /// Delete explicit programs, optionally including unnamed shared-map dependants.
    Ids {
        /// Requested identities; selected dependants are processed before owners.
        ids: &'a [std::num::NonZeroU32],
        /// Include unnamed shared-map dependants.
        recursive: bool,
    },
    /// Select every managed program under the writer lock.
    All,
}

/// One requested deletion and all progress made on its behalf.
#[must_use = "inspect each result and retain unresolved cleanup"]
pub struct DeleteOutcome<
    I,
    S: bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpStore
        + bpfman_store::TcStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement
        + bpfman_kernel::TcLifecycle,
> {
    /// Requested program or link identity.
    pub id: I,
    /// Completed underlying operations, including recursively deleted dependants.
    pub progress: Vec<DeleteProgress<S, K>>,
    /// Failure retains the original underlying cleanup ownership.
    pub outcome: Result<(), DeleteError<S, K>>,
}

/// A completed step in cascading deletion, retaining any cleanup warnings.
#[must_use = "inspect progress and retain unresolved cleanup"]
pub enum DeleteProgress<
    S: bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpStore
        + bpfman_store::TcStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement
        + bpfman_kernel::TcLifecycle,
> {
    /// A detached link; the report retains its attachment family.
    Link {
        /// Managed link identity.
        id: std::num::NonZeroU64,
        /// Original cleanup report.
        report: DetachReport<S, K>,
    },
    /// An unloaded program, possibly retaining retryable cleanup warnings.
    Program {
        /// Managed program identity.
        id: std::num::NonZeroU32,
        /// Original cleanup report.
        report: UnloadReport<S, K>,
    },
}

/// Cascading deletion failure, retaining original consuming retry ownership.
pub enum DeleteError<
    S: bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpStore
        + bpfman_store::TcStore,
    K: bpfman_kernel::ProgramResources
        + bpfman_kernel::TracepointLinks
        + bpfman_kernel::XdpReplacement
        + bpfman_kernel::TcLifecycle,
> {
    /// Admission or stored relationship observation failed.
    Observation(Error),
    /// A prior attempt in this batch failed; retry its original retained receipt.
    BlockedProgram(std::num::NonZeroU32),
    /// A prior link attempt in this batch failed; retry its retained receipt.
    BlockedLink(std::num::NonZeroU64),
    /// Link teardown failed.
    Link {
        /// Failed managed link identity, possibly belonging to a dependant.
        id: std::num::NonZeroU64,
        /// Original family-specific failure and cleanup ownership.
        error: Box<DetachError<S, K>>,
    },
    /// Program teardown failed.
    Program {
        /// Failed managed program identity, possibly a dependant.
        id: std::num::NonZeroU32,
        /// Original failure and cleanup ownership.
        error: UnloadError<S, K>,
    },
}

/// Stable classification for cascading deletion failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeleteErrorKind {
    /// Cancelled before the next teardown started.
    Cancelled,
    /// A requested managed identity is absent.
    NotFound,
    /// Ownership changed or an earlier failed attempt blocks this target.
    InvalidState,
    /// This backend does not support the operation.
    Unsupported,
    /// Store, filesystem or kernel observation failed.
    Unavailable,
}
