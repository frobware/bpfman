//! Open a compatible store, creating it only when absent.

use std::time::Duration;

use bpfman_core::{StoreObservation, StoreOpenPlan, plan_store_open};
use bpfman_fs::RuntimeLayout;
use bpfman_lock::{AcquireOptions, with_write_lock};
use bpfman_store_sqlite::{SCHEMA_VERSION, Store};

use crate::{
    Error, ErrorKind,
    error::{Failure, lock_error, store_error},
};

pub(super) fn open_or_create_store(
    layout: &RuntimeLayout,
    timeout: Duration,
) -> Result<Store, Error> {
    let database = layout.database_path();
    // Match Go's layout and serialise first-touch creation with both implementations.
    with_write_lock(
        &layout.lock_path(),
        AcquireOptions {
            timeout,
            cancelled: None,
        },
        |permit| {
            // Observation and execution share the lock. Never interpret a failed
            // observation as absence, or return a path to be reopened later.
            let observed = match Store::inspect(&database).map_err(store_error)? {
                None => StoreObservation::Missing,
                Some(store) => StoreObservation::Existing {
                    version: store.schema_version(),
                    evidence: store,
                },
            };
            match plan_store_open(observed, SCHEMA_VERSION) {
                StoreOpenPlan::Create => {
                    bpfman_store_sqlite::create_if_missing(&database, &permit)
                        .map_err(store_error)?;
                    Store::open(&database).map_err(store_error)
                }
                StoreOpenPlan::UseExisting(store) => Ok(store),
                StoreOpenPlan::RejectIncompatible {
                    found,
                    expected,
                    evidence: _,
                } => Err(Error {
                    kind: ErrorKind::IncompatibleState,
                    source: Failure::IncompatibleSchema { found, expected },
                }),
            }
        },
    )
    .map_err(lock_error)?
}
