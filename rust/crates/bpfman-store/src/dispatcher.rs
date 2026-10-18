use crate::{Error, OpenStore};
use bpfman_fs::RuntimeWriter;
use bpfman_model::DispatcherKey;

/// Administrative removal of stored intent, independent of kernel teardown.
/// An unlocked runtime cannot authorize removal:
/// ```compile_fail
/// use bpfman_store::DispatcherStore;
/// fn unlocked<S: DispatcherStore>(store: &S, runtime: &bpfman_fs::RuntimeDirectory, key: bpfman_model::DispatcherKey) {
///     store.delete_dispatcher(runtime, key);
/// }
/// ```
pub trait DispatcherStore: OpenStore {
    /// Atomically delete the dispatcher and all extension-link records at this
    /// attachment point, preserving programs and map relationships. Return true
    /// when committed, false when absent. False and Err must leave all records
    /// unchanged, including orphan member records when no dispatcher exists.
    /// Never acquire kernel or filesystem cleanup authority from this operation.
    fn delete_dispatcher(
        &self,
        writer: &RuntimeWriter<'_>,
        key: DispatcherKey,
    ) -> Result<bool, Error>;
}
