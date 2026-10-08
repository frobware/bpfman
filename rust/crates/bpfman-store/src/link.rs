use crate::Error;
use bpfman_core::EffectFailure;
use bpfman_fs::RuntimeWriter;
use bpfman_model::{StoredLink, Tracepoint};
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
};

/// Inputs for recording standalone tracepoint attachment intent before any
/// kernel acquisition. The store allocates the managed ID and canonical path.
pub struct PendingTracepoint<'a> {
    /// Already committed tracepoint program.
    pub program_id: NonZeroU32,
    /// Validated tracepoint target.
    pub target: &'a Tracepoint,
    /// Operator labels.
    pub metadata: &'a BTreeMap<String, String>,
    /// RFC3339 time captured by the caller.
    pub created_at: &'a str,
}

/// Consistent observations of standalone links without the runtime writer lock.
pub trait LinkReader {
    /// Read complete TC ingress dispatchers from one validated snapshot.
    fn read_tc_dispatchers(&mut self) -> Result<Vec<bpfman_model::TcDispatcherSnapshot>, Error>;
    /// Read links from one fresh, validated snapshot, sorted by managed ID.
    /// Unsupported attachment types fail explicitly instead of being omitted.
    fn read_links(&mut self) -> Result<Vec<StoredLink>, Error>;
}

/// Conditional mutations for the pending-link protocol. Receipts retain the
/// exact observed state and runtime identity, and cannot be cloned or forged.
/// Store operations do not acquire or release kernel attachments.
///
/// ```compile_fail
/// use bpfman_store::LinkStore;
/// fn duplicate<S: LinkStore>(receipt: S::LinkReceipt) {
///     let _copy = receipt.clone();
/// }
/// ```
///
/// ```compile_fail
/// use bpfman_store::LinkStore;
/// fn unlocked<S: LinkStore>(store: &S, runtime: &bpfman_fs::RuntimeDirectory, receipt: S::LinkReceipt) {
///     store.delete_link(runtime, receipt);
/// }
/// ```
pub trait LinkStore {
    /// Owned evidence for one unchanged link record.
    type LinkReceipt: Send + Sync + 'static;

    /// Allocate and publish intent, including its canonical pin path, atomically.
    /// Err means nothing was committed. Validate that the program is a tracepoint.
    fn create_pending_tracepoint(
        &self,
        writer: &RuntimeWriter<'_>,
        request: PendingTracepoint<'_>,
    ) -> Result<(StoredLink, Self::LinkReceipt), Error>;

    /// Record the kernel ID only if the receipt still identifies pending intent.
    /// Err means no finalisation committed and retains the pending receipt for
    /// compensation. Nothing fallible may follow a successful commit.
    fn finalise_link(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::LinkReceipt,
        kernel_id: NonZeroU32,
    ) -> Result<StoredLink, EffectFailure<Self::LinkReceipt, Error>>;

    /// Observe an existing record for conditional deletion. Only absence is None.
    fn observe_link(
        &self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU64,
    ) -> Result<LinkObservation<Self>, Error>;

    /// Delete unchanged intent or a finalised record. The interpreter must first
    /// release any live attachment: a failed unpin blocks this dependent step.
    /// Err retains ownership and means no deletion committed. Never retry here.
    fn delete_link(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::LinkReceipt,
    ) -> Result<(), EffectFailure<Self::LinkReceipt, Error>>;
}

/// Absence or a stored link paired with backend-owned deletion evidence.
pub type LinkObservation<S> = Option<(StoredLink, <S as LinkStore>::LinkReceipt)>;
