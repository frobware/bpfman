use alloc::{string::String, vec::Vec};

use bpfman_model::{ProgramType, StoredProgramSummary};

/// Selection over stored program metadata. Empty type selection means all types.
#[derive(Clone, Debug, Default)]
pub struct ProgramFilter {
    /// Union of requested program types.
    pub types: Vec<ProgramType>,
    /// Exact application label match, if supplied.
    pub application: Option<String>,
}

/// Filter complete store observations and order results by program ID.
pub fn list_programs(
    programs: Vec<StoredProgramSummary>,
    filter: &ProgramFilter,
) -> Vec<StoredProgramSummary> {
    let mut selected: Vec<_> = programs
        .into_iter()
        .filter(|p| {
            (filter.types.is_empty() || filter.types.contains(&p.kind()))
                && filter
                    .application
                    .as_ref()
                    .is_none_or(|app| app.is_empty() || p.application() == app)
        })
        .collect();
    selected.sort_by_key(StoredProgramSummary::id);
    selected
}
