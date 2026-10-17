use alloc::{boxed::Box, collections::BTreeMap, string::String, vec::Vec};

use bpfman_model::{ProgramType, StoredProgramSummary};

/// Selection over stored program metadata. Empty type selection means all types.
#[derive(Clone, Debug, Default)]
pub struct ProgramFilter {
    /// Union of requested program types.
    pub types: Vec<ProgramType>,
    /// Exact application label match, if supplied.
    pub application: Option<String>,
    /// Exact metadata pairs. A nonempty application overrides its pair here.
    pub metadata: BTreeMap<String, String>,
    /// Validated label requirements, combined with exact pairs using AND.
    pub selector: crate::MetadataSelector,
}

impl ProgramFilter {
    fn matches_metadata(&self, metadata: &BTreeMap<String, String>) -> bool {
        let application = self.application.as_ref().filter(|value| !value.is_empty());
        application.is_none_or(|app| metadata.get("bpfman.io/application") == Some(app))
            && self.metadata.iter().all(|(key, value)| {
                (key == "bpfman.io/application" && application.is_some())
                    || metadata.get(key) == Some(value)
            })
            && self.selector.matches(metadata)
    }

    fn selects_unmanaged(&self) -> bool {
        self.application.as_ref().is_none_or(String::is_empty)
            && self.metadata.is_empty()
            && self.selector.is_empty()
    }
}

/// Selection requiring live link observations rather than stored intent alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentFilter {
    /// Programs with at least one observed kernel link.
    Attached,
    /// Programs with no observed kernel links, including stale or pending intent.
    Unattached,
}

/// Select managed rows by observed kernel link presence. Kernel-only rows are
/// excluded for either selection, matching Go's lack of managed link state.
pub fn select_program_attachments(
    entries: Vec<bpfman_model::ProgramListEntry>,
    links: &[bpfman_model::ObservedLink],
    filter: AttachmentFilter,
) -> Vec<bpfman_model::ProgramListEntry> {
    let active: alloc::collections::BTreeSet<_> = links
        .iter()
        .filter(|link| link.kernel.is_some())
        .map(|link| link.record.program_id)
        .collect();

    entries
        .into_iter()
        .filter(|entry| match entry {
            bpfman_model::ProgramListEntry::Managed(program) => {
                active.contains(&program.record.id) == (filter == AttachmentFilter::Attached)
            }
            bpfman_model::ProgramListEntry::KernelOnly(_) => false,
        })
        .collect()
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
                && filter.matches_metadata(p.metadata())
        })
        .collect();
    selected.sort_by_key(StoredProgramSummary::id);
    selected
}

/// Select full records with the same policy used by stored summary listing.
pub fn select_records(
    records: Vec<bpfman_model::StoredProgram>,
    filter: &ProgramFilter,
) -> Vec<bpfman_model::StoredProgram> {
    let mut selected: Vec<_> = records
        .into_iter()
        .filter(|p| {
            (filter.types.is_empty() || filter.types.contains(&p.spec.kind()))
                && filter.matches_metadata(&p.metadata)
        })
        .collect();
    selected.sort_by_key(|p| p.id);
    selected
}

/// Join one store snapshot with a kernel inventory, preserving managed identity
/// before applying exact managed and coarse kernel-only type filters.
pub fn list_program_entries(
    records: Vec<bpfman_model::StoredProgram>,
    kernel: Vec<bpfman_model::KernelProgram>,
    filter: &ProgramFilter,
) -> Vec<bpfman_model::ProgramListEntry> {
    use bpfman_model::{ProgramEntry, ProgramListEntry};

    let mut kernel: BTreeMap<_, _> = kernel.into_iter().map(|p| (p.id, p)).collect();
    let mut managed_kernel = BTreeMap::new();

    // Remove every stored identity before filtering; a filtered managed program
    // must never reappear as an unmanaged row under the coarser kernel taxonomy.
    for record in &records {
        if let Some(observed) = kernel.remove(&record.id) {
            managed_kernel.insert(record.id, observed);
        }
    }

    let mut entries: Vec<_> = select_records(records, filter)
        .into_iter()
        .map(|record| {
            ProgramListEntry::Managed(Box::new(ProgramEntry {
                kernel: managed_kernel.remove(&record.id),
                record,
            }))
        })
        .collect();

    if filter.selects_unmanaged() {
        entries.extend(
            kernel
                .into_values()
                .filter(|p| {
                    filter.types.is_empty()
                        || filter.types.iter().any(|kind| {
                            let projected = match kind {
                                ProgramType::Xdp => "xdp",
                                ProgramType::Tc | ProgramType::Tcx => "schedcls",
                                ProgramType::Tracepoint => "tracepoint",
                                ProgramType::Kprobe
                                | ProgramType::Kretprobe
                                | ProgramType::Uprobe
                                | ProgramType::Uretprobe => "kprobe",
                                ProgramType::Fentry | ProgramType::Fexit => "tracing",
                                ProgramType::Lsm => "lsm",
                            };
                            p.kind == projected
                        })
                })
                .map(ProgramListEntry::KernelOnly),
        );
    }

    entries.sort_by_key(ProgramListEntry::id);
    entries
}
