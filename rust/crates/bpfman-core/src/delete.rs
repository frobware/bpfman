//! Stable dependant-first ordering for explicit deletion targets.
use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};
use core::num::NonZeroU32;

/// Order selected shared-map dependants before their selected owners. Unnamed
/// dependants are not added. Preserve unrelated input order, matching Go, and
/// deduplicate a multi-target selection when relationships are present. The
/// iterative walk remains bounded even for malformed cyclic observations.
pub fn order_program_deletes(
    ids: &[NonZeroU32],
    dependants: &BTreeMap<NonZeroU32, Vec<NonZeroU32>>,
) -> Vec<NonZeroU32> {
    if ids.len() <= 1 || dependants.is_empty() {
        return ids.to_vec();
    }
    let selected: BTreeSet<_> = ids.iter().copied().collect();
    let mut seen = BTreeSet::new();
    let mut ordered = Vec::with_capacity(ids.len());
    for &id in ids {
        let mut stack = alloc::vec![(id, false)];
        while let Some((id, visited)) = stack.pop() {
            if visited {
                ordered.push(id);
                continue;
            }
            if !seen.insert(id) {
                continue;
            }
            stack.push((id, true));
            if let Some(children) = dependants.get(&id) {
                for &child in children.iter().rev() {
                    if selected.contains(&child) {
                        stack.push((child, false));
                    }
                }
            }
        }
    }
    ordered
}
