//! Persistence contracts expressed as domain operations, independent of file format.
//! Backends own compatibility checks, atomic publication, and opaque teardown evidence.
//! No connections, transaction callbacks, schema versions, or serialized data escape.

mod xdp;
pub use xdp::{XdpCommit, XdpReader, XdpStore};
mod xdp_replace;
pub use xdp_replace::{
    XdpDispatcherReader, XdpMemberCommit, XdpMemberId, XdpReplace, XdpReplacementStore,
};

mod error;
mod link;

pub use link::{LinkObservation, LinkReader, LinkStore, PendingTracepoint};

use bpfman_core::EffectFailure;
use bpfman_fs::{RuntimeDirectory, RuntimeWriter};
use bpfman_model::{ProgramSpec, StoredProgram, StoredProgramSummary};
pub use error::{Error, ErrorKind};
use std::{collections::BTreeMap, num::NonZeroU32};

/// Open compatible state for independent readers or under writer authority.
/// Corrupt, inaccessible, or incompatible state must fail without replacement.
pub trait OpenStore {
    /// Retained, shareable store handle. Clones share backend resources, not a
    /// transaction or frozen snapshot. Each read observes current committed state.
    type Reader: ProgramReader + Clone + Send + Sync;

    /// Open existing state without acquiring the runtime writer lock or creating
    /// application state. Only absence returns None; invalid or inaccessible state fails.
    /// Independent readers may run concurrently with the single writer.
    fn open_reader(&self, runtime: &RuntimeDirectory) -> Result<Option<Self::Reader>, Error>;

    /// Observe and open in the same writer scope. Never migrate implicitly.
    fn open(&self, writer: &RuntimeWriter<'_>) -> Result<Self::Reader, Error>;
}

/// Consistent read-only observations from an opened backend.
/// Each call observes one committed snapshot. Separate calls may see newer state.
/// Clone the opened handle for independent callers. Backends own connection
/// reuse and snapshot acquisition; cloning must not perform I/O.
pub trait ProgramReader {
    /// Validate current store compatibility without retaining a snapshot.
    /// Mutation preflight uses this when reusing an already-opened handle.
    fn validate(&mut self) -> Result<(), Error>;

    /// Read stored summaries without kernel observations.
    fn read_programs(&mut self) -> Result<Vec<StoredProgramSummary>, Error>;

    /// Read complete records and relationships in one consistent snapshot.
    /// Validate current format compatibility on every read.
    fn read_records(&mut self) -> Result<Vec<StoredProgram>, Error>;
}

/// Inputs for committing one private, local program.
/// Managed paths are derived from writer authority, never supplied as targets.
pub struct LoadRecord<'a> {
    /// Kernel-assigned identity.
    pub id: NonZeroU32,
    /// Validated ELF selection.
    pub spec: &'a ProgramSpec,
    /// Original local file operand.
    pub source: &'a str,
    /// ELF license.
    pub license: &'a str,
    /// RFC3339 time captured before load.
    pub created_at: &'a str,
    /// Operator labels, including the application label.
    pub metadata: &'a BTreeMap<String, String>,
    /// Validated ELF global overrides as raw bytes.
    pub globals: &'a BTreeMap<String, Vec<u8>>,
}

/// Atomic publication of loaded programs and their private map-set memberships.
///
/// An unlocked runtime cannot authorize a commit, regardless of backend:
/// ```compile_fail
/// use bpfman_store::{CommitLoad, LoadRecord};
/// fn unlocked<S: CommitLoad>(store: &S, runtime: &bpfman_fs::RuntimeDirectory, record: LoadRecord<'_>) {
///     store.commit_program(runtime, record);
/// }
/// ```
pub trait CommitLoad {
    /// Never overwrite existing state. Err means the load was not committed and
    /// the caller may compensate its acquisitions. Nothing fallible may turn a
    /// successful commit into Err; later observation/delivery failures are separate.
    fn commit_program(
        &self,
        writer: &RuntimeWriter<'_>,
        record: LoadRecord<'_>,
    ) -> Result<StoredProgramSummary, Error> {
        let summary = StoredProgramSummary::new(
            record.id,
            record.spec.name().as_str().into(),
            record.spec.kind(),
            record.metadata.clone(),
            Vec::new(),
        );
        self.commit_programs(writer, &[record])?;

        Ok(summary)
    }

    /// Publish every member in one atomic operation, in input order. A failure
    /// publishes none of the batch, including map sets. Never overwrite records.
    /// An empty batch is a no-op. Success ends compensation authority for all members.
    fn commit_programs(
        &self,
        writer: &RuntimeWriter<'_>,
        records: &[LoadRecord<'_>],
    ) -> Result<(), Error>;
}

/// Conditional teardown of a program with exclusively owned maps.
/// Observation may precede link teardown; deletion must refuse remaining links.
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
