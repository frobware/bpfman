use crate::{Bpfman, Cancellation, Error, error::store_error};
use bpfman_lock::AcquireOptions;
use bpfman_model::DispatcherKey;
use bpfman_store::{DispatcherStore, OpenStore};

impl<S: DispatcherStore, K> Bpfman<S, K> {
    /// Forget a dispatcher and its member records, preserving running attachments
    /// and managed pins. Return false if the dispatcher does not exist.
    pub fn delete_dispatcher(&self, key: DispatcherKey) -> Result<bool, Error> {
        self.delete_dispatcher_with_cancellation(key, &Cancellation::new())
    }

    /// Delete stored intent under one cancellable writer scope. Cancellation
    /// before the store mutation leaves records unchanged; an admitted atomic
    /// mutation determines its own outcome, even if cancellation arrives in flight.
    #[tracing::instrument(name = "dispatcher.delete", level = "debug", skip_all, err)]
    pub fn delete_dispatcher_with_cancellation(
        &self,
        key: DispatcherKey,
        cancellation: &Cancellation,
    ) -> Result<bool, Error> {
        self.store
            .runtime()
            .with_writer(
                AcquireOptions {
                    timeout: self.lock_timeout,
                    cancelled: Some(cancellation.flag()),
                },
                |writer| {
                    cancellation.check()?;
                    self.store.open(&writer).map_err(store_error)?;
                    cancellation.check()?;
                    self.store
                        .delete_dispatcher(&writer, key)
                        .map_err(store_error)
                },
            )
            .map_err(crate::error::filesystem_error)?
    }
}
