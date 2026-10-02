//! Startup effects live here; ordinary store reads remain read-only.

use std::time::Duration;

use bpfman_core::{StoreSetup, plan_store_setup};
use bpfman_fs::RuntimeLayout;
use bpfman_lock::{AcquireOptions, with_write_lock};
use bpfman_store_sqlite::{SCHEMA_VERSION, Store};

use crate::{
    Error, ErrorKind,
    error::{Failure, lock_error, store_error},
};

pub(super) fn store(layout: &RuntimeLayout, timeout: Duration) -> Result<Store, Error> {
    let database = layout.database_path();
    // Match Go's layout and serialise first-touch setup with both implementations.
    with_write_lock(
        &layout.lock_path(),
        AcquireOptions {
            timeout,
            cancelled: None,
        },
        |permit| {
            // Observation and execution share the lock. Never interpret a failed
            // observation as absence, or return a path to be reopened later.
            let existing = Store::inspect(&database).map_err(store_error)?;
            let observed = existing.as_ref().map(Store::schema_version);
            match plan_store_setup(observed, SCHEMA_VERSION) {
                StoreSetup::Initialise => {
                    bpfman_store_sqlite::initialise_if_missing(&database, &permit)
                        .map_err(store_error)?;
                    Store::open(&database).map_err(store_error)
                }
                StoreSetup::UseExisting => existing.ok_or(Error {
                    kind: ErrorKind::Unavailable,
                    source: Failure::MissingSetupObservation,
                }),
                StoreSetup::RejectIncompatible { found, expected } => Err(Error {
                    kind: ErrorKind::IncompatibleState,
                    source: Failure::IncompatibleSchema { found, expected },
                }),
            }
        },
    )
    .map_err(lock_error)?
}
