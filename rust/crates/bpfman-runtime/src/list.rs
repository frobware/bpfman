//! Application operations: gather observations, then evaluate pure policy.

use std::time::Duration;

use bpfman_core::ProgramFilter;
use bpfman_fs::RuntimeLayout;
use bpfman_model::StoredProgramSummary;

use crate::{Error, error::store_error, setup};

/// Initialise missing state, read managed summaries, and apply pure selection.
pub fn list_programs(
    layout: &RuntimeLayout,
    filter: &ProgramFilter,
    lock_timeout: Duration,
) -> Result<Vec<StoredProgramSummary>, Error> {
    let mut store = setup::store(layout, lock_timeout)?;
    let programs = store.read_programs().map_err(store_error)?;
    Ok(bpfman_core::list_programs(programs, filter))
}
