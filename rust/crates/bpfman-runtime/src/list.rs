//! Application operations: gather observations, then evaluate pure policy.

use bpfman_core::ProgramFilter;
use bpfman_model::StoredProgramSummary;
use bpfman_store::{OpenStore, ProgramReader};

use crate::{Bpfman, Error, error::store_error};

impl<S: OpenStore> Bpfman<S> {
    /// Read managed summaries without kernel observations or the writer lock.
    #[tracing::instrument(name = "program.list", level = "debug", skip_all, err)]
    pub fn list(&self, filter: &ProgramFilter) -> Result<Vec<StoredProgramSummary>, Error> {
        let mut reader = self.store.reader();
        let programs = reader.read_programs().map_err(store_error)?;

        Ok(bpfman_core::list_programs(programs, filter))
    }
}
