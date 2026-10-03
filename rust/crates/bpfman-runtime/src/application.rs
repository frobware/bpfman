use crate::{ActiveStore, Bpfman};
use std::time::Duration;

impl<S> Bpfman<S> {
    /// Bind operations to this active store and its already-adopted runtime.
    /// Construction performs no I/O. The timeout applies only to writer-lock
    /// acquisition; reads have no lock budget or global coordination.
    pub fn new(store: ActiveStore<S>, lock_timeout: Duration) -> Self {
        Self {
            store,
            lock_timeout,
        }
    }
}
