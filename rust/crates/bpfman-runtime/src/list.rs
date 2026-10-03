//! Application operations: gather observations, then evaluate pure policy.

use bpfman_store::ProgramReader;
use std::time::Duration;

use bpfman_core::ProgramFilter;
use bpfman_fs::RuntimeLayout;
use bpfman_model::StoredProgramSummary;

use crate::{Error, error::store_error, store::read_store};

/// Read managed summaries from the initialized store without the writer lock.
#[tracing::instrument(name = "program.list", level = "debug", skip_all, err)]
pub fn list_programs<S: bpfman_store::OpenStore>(
    backend: &S,
    layout: &RuntimeLayout,
    filter: &ProgramFilter,
    lock_timeout: Duration,
) -> Result<Vec<StoredProgramSummary>, Error> {
    let mut store = read_store(backend, layout, lock_timeout)?;
    let programs = store.read_programs().map_err(store_error)?;

    Ok(bpfman_core::list_programs(programs, filter))
}
