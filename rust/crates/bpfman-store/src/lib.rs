//! Persistence contracts expressed as domain operations, independent of file format.
//! Backends own compatibility checks, atomic publication, and opaque teardown evidence.
//! No connections, transaction callbacks, schema versions, or serialized data escape.

mod error;

use bpfman_core::EffectFailure;
use bpfman_fs::RuntimeWriter;
use bpfman_model::{StoredProgram, StoredProgramSummary, Symbol};
pub use error::{Error, ErrorKind};
use std::{collections::BTreeMap, num::NonZeroU32};

/// Open compatible state under writer authority, creating only if absent.
/// Corrupt, inaccessible, or incompatible state must fail without replacement.
pub trait OpenStore {
    /// Opened read handle; owns backend evidence rather than a path to reopen.
    type Reader: ProgramReader;

    /// Observe and open in the same writer scope. Never migrate implicitly.
    fn open(&self, writer: &RuntimeWriter<'_>) -> Result<Self::Reader, Error>;
}

/// Consistent read-only observations from an opened backend.
pub trait ProgramReader {
    /// Read stored summaries without kernel observations.
    fn read_programs(&mut self) -> Result<Vec<StoredProgramSummary>, Error>;

    /// Read complete records and relationships in one consistent snapshot.
    /// Validate current format compatibility on every read.
    fn read_records(&mut self) -> Result<Vec<StoredProgram>, Error>;
}

/// Inputs for committing one private, local tracepoint.
/// Managed paths are derived from writer authority, never supplied as targets.
pub struct TracepointRecord<'a> {
    /// Kernel-assigned identity.
    pub id: NonZeroU32,
    /// Validated ELF selection.
    pub name: &'a Symbol,
    /// Original local file operand.
    pub source: &'a str,
    /// ELF license.
    pub license: &'a str,
    /// RFC3339 time captured before load.
    pub created_at: &'a str,
    /// Operator labels, including the application label.
    pub metadata: &'a BTreeMap<String, String>,
}

/// Atomic publication of a loaded program and its private map-set membership.
///
/// An unlocked runtime cannot authorize a commit, regardless of backend:
/// ```compile_fail
/// use bpfman_store::{CommitLoad, TracepointRecord};
/// fn unlocked<S: CommitLoad>(store: &S, runtime: &bpfman_fs::RuntimeDirectory, record: TracepointRecord<'_>) {
///     store.commit_tracepoint(runtime, record);
/// }
/// ```
pub trait CommitLoad {
    /// Never overwrite existing state. Err means the load was not committed and
    /// the caller may compensate its acquisitions. Nothing fallible may turn a
    /// successful commit into Err; later observation/delivery failures are separate.
    fn commit_tracepoint(
        &self,
        writer: &RuntimeWriter<'_>,
        record: TracepointRecord<'_>,
    ) -> Result<StoredProgramSummary, Error>;
}

/// Conditional teardown of an unattached tracepoint with exclusively owned maps.
/// Receipts must retain backend and runtime identity, remain non-cloneable, and
/// be revalidated atomically on deletion. They are evidence, not bare IDs.
///
/// Callers cannot assume they can duplicate backend evidence:
/// ```compile_fail
/// use bpfman_store::UnloadStore;
/// fn duplicate<S: UnloadStore>(receipt: S::ProgramReceipt) {
///     let _copy = receipt.clone();
/// }
/// ```
/// Nor can they substitute an integer identity for an owned receipt:
/// ```compile_fail
/// use bpfman_store::UnloadStore;
/// fn by_id<S: UnloadStore>(store: &S, writer: &bpfman_fs::RuntimeWriter<'_>) {
///     store.delete_program(writer, 42u32);
/// }
/// ```
pub trait UnloadStore {
    /// Owned evidence authorizing conditional program deletion.
    type ProgramReceipt: Send + Sync + 'static;

    /// Owned evidence authorizing deletion of the unused private map set.
    type MapSetReceipt: Send + Sync + 'static;

    /// Validate scope and ownership without creation or mutation. Only absence
    /// returns None; failed observation must remain an error.
    fn observe_unload(
        &self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<UnloadObservation<Self>, Error>;
    /// Delete only if the original evidence still holds. Err retains ownership
    /// and means no deletion committed. Require authority for the original root.
    fn delete_program(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::ProgramReceipt,
    ) -> Result<(), EffectFailure<Self::ProgramReceipt, Error>>;
    /// Delete only after program deletion and map-pin cleanup, and only if still
    /// unused. Err retains ownership; never swallow errors or retry automatically.
    fn delete_map_set(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::MapSetReceipt,
    ) -> Result<(), EffectFailure<Self::MapSetReceipt, Error>>;
}

/// Absence or the backend's paired program and map-set ownership evidence.
pub type UnloadObservation<S> = Option<(
    <S as UnloadStore>::ProgramReceipt,
    <S as UnloadStore>::MapSetReceipt,
)>;
