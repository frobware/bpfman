use alloc::{collections::BTreeSet, vec::Vec};
use bpfman_model::{LinkKind, StoredLink, StoredProgramSummary};
use core::num::NonZeroU32;

/// Selection over stored link intent and, optionally, its owning programs.
#[derive(Clone, Debug, Default)]
pub struct LinkFilter {
    /// Owning program identity, if supplied.
    pub program_id: Option<NonZeroU32>,
    /// Union of managed attachment kinds; empty means every kind.
    pub kinds: Vec<LinkKind>,
    /// Owning-program selection. None preserves links without a stored owner.
    pub program: Option<crate::ProgramFilter>,
}

/// Filter stored links and sort by managed link ID. Program-scoped filtering
/// uses load-time metadata, never the link's own attach-time metadata. Separate
/// program and link observations do not imply a shared atomic snapshot.
pub fn list_links(
    links: Vec<StoredLink>,
    programs: Vec<StoredProgramSummary>,
    filter: &LinkFilter,
) -> Vec<StoredLink> {
    let allowed: Option<BTreeSet<_>> = filter.program.as_ref().map(|scope| {
        crate::list_programs(programs, scope)
            .into_iter()
            .map(|program| program.id())
            .collect()
    });
    let mut selected: Vec<_> = links
        .into_iter()
        .filter(|link| {
            filter.program_id.is_none_or(|id| link.program_id == id)
                && (filter.kinds.is_empty() || filter.kinds.contains(&link.details.kind()))
                && allowed
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&link.program_id))
        })
        .collect();
    selected.sort_by_key(|link| link.id);
    selected
}
