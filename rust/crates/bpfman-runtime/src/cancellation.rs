use std::sync::atomic::{AtomicBool, Ordering};

use crate::{Cancellation, Error, ErrorKind, error::Failure};

impl Cancellation {
    /// Create an independent request that has not been cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. This does not wait for the operation to stop.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }

    /// Whether this request has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    pub(super) fn flag(&self) -> &AtomicBool {
        &self.flag
    }

    pub(super) fn check(&self) -> Result<(), Error> {
        if self.is_cancelled() {
            tracing::debug!("cancellation observed at operation boundary");
            Err(Error {
                kind: ErrorKind::Cancelled,
                source: Failure::Cancelled,
            })
        } else {
            Ok(())
        }
    }
}
