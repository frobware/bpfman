//! Application operations: gather observations, then evaluate pure policy.

use bpfman_core::ProgramFilter;
use bpfman_model::StoredProgramSummary;
use bpfman_store::{OpenStore, ProgramReader};

use crate::{Bpfman, Error, error::store_error};

impl<S: OpenStore> Bpfman<S> {
    /// Read managed summaries without kernel observations or the writer lock.
    pub fn list(&self, filter: &ProgramFilter) -> Result<Vec<StoredProgramSummary>, Error> {
        self.list_with_cancellation(filter, &crate::Cancellation::new())
    }

    /// Read summaries with cancellation checks around snapshot acquisition.
    #[tracing::instrument(name = "program.list", level = "debug", skip_all, err)]
    pub fn list_with_cancellation(
        &self,
        filter: &ProgramFilter,
        cancellation: &crate::Cancellation,
    ) -> Result<Vec<StoredProgramSummary>, Error> {
        cancellation.check()?;
        let mut reader = self.store.reader();
        let programs = reader.read_programs().map_err(store_error)?;

        cancellation.check()?;

        Ok(bpfman_core::list_programs(programs, filter))
    }
}
