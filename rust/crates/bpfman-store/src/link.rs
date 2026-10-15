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
    /// Read complete TC dispatchers from one validated snapshot.
    fn read_tc_dispatchers(&mut self) -> Result<Vec<bpfman_model::TcDispatcherSnapshot>, Error>;
    /// Read links from one fresh, validated snapshot, sorted by managed ID.
    /// Unsupported attachment types fail explicitly instead of being omitted.
    fn read_links(&mut self) -> Result<Vec<StoredLink>, Error>;
}

/// Conditional mutations for standalone tracepoint, probe and TCX pending intent.
/// Receipts retain exact observed state and runtime identity, and cannot be cloned or forged.
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
    /// Publish function tracing intent matching the program's load-time target.
    fn create_pending_tracing(
        &self,
        writer: &RuntimeWriter<'_>,
        request: PendingTracing<'_>,
    ) -> Result<(StoredLink, Self::LinkReceipt), Error>;

    /// Publish host probe intent, validating loaded entry/return selection.
    fn create_pending_uprobe(
        &self,
        writer: &RuntimeWriter<'_>,
        request: PendingUprobe<'_>,
    ) -> Result<(StoredLink, Self::LinkReceipt), Error>;

    /// Publish probe intent, validating the loaded program and entry/return selection.
    fn create_pending_kprobe(
        &self,
        writer: &RuntimeWriter<'_>,
        request: PendingKprobe<'_>,
    ) -> Result<(StoredLink, Self::LinkReceipt), Error>;

    /// Allocate native TCX intent, validating the loaded program and hook uniqueness.
    fn create_pending_tcx(
        &self,
        writer: &RuntimeWriter<'_>,
        request: PendingTcx<'_>,
    ) -> Result<(StoredLink, Self::LinkReceipt), Error>;

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

/// Native TCX intent admitted before kernel attachment.
pub struct PendingTcx<'a> {
    /// Managed native classifier.
    pub program_id: NonZeroU32,
    /// Resolved namespace and hook.
    pub target: &'a bpfman_model::TcxLink,
    /// Operator labels.
    pub metadata: &'a BTreeMap<String, String>,
    /// Caller-captured RFC3339 timestamp.
    pub created_at: &'a str,
}

/// Standalone kernel probe intent admitted before kernel attachment.
pub struct PendingKprobe<'a> {
    /// Managed native probe program.
    pub program_id: NonZeroU32,
    /// Kernel function, offset and derived entry/return selection.
    pub target: &'a bpfman_model::KprobeLink,
    /// Operator labels.
    pub metadata: &'a BTreeMap<String, String>,
    /// Caller-captured RFC3339 timestamp.
    pub created_at: &'a str,
}

/// Standalone user-space probe intent admitted before kernel attachment.
pub struct PendingUprobe<'a> {
    /// Managed native probe program.
    pub program_id: NonZeroU32,
    /// Host file, symbol/offset, PID and derived entry/return selection.
    pub target: &'a bpfman_model::UprobeLink,
    /// Operator labels.
    pub metadata: &'a BTreeMap<String, String>,
    /// Caller-captured RFC3339 timestamp.
    pub created_at: &'a str,
}

/// Function tracing intent admitted before kernel attachment.
pub struct PendingTracing<'a> {
    /// Managed native tracing program.
    pub program_id: NonZeroU32,
    /// Load-bound function and entry/exit selection.
    pub target: &'a bpfman_model::TracingLink,
    /// Operator labels.
    pub metadata: &'a BTreeMap<String, String>,
    /// Caller-captured RFC3339 timestamp.
    pub created_at: &'a str,
}
