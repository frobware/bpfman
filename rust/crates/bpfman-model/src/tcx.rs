//! Native traffic-control attachment intent and relative ordering.
use crate::{InterfaceName, NetworkNamespace, TcKey};
use core::num::NonZeroU32;

/// Resolved native TCX attachment, without a dispatcher revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TcxLink {
    /// Namespace, interface and direction identity.
    pub key: TcKey,
    /// Interface spelling validated during admission.
    pub interface: InterfaceName,
    /// Namespace selector retained as operator intent.
    pub netns: NetworkNamespace,
    /// Lower values execute first; ties retain attachment order.
    pub priority: u32,
    /// Rank among managed links on this hook, derived from the store snapshot.
    pub position: usize,
}

/// Relative position in the native kernel chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TcxOrder {
    /// First managed attachment, at chain head.
    First,
    /// Immediately before the specified live program.
    Before(NonZeroU32),
    /// Immediately after the specified live program.
    After(NonZeroU32),
}

/// Choose a position from members ordered by priority and attachment order.
/// The caller supplies validated live anchors on the same hook.
pub fn tcx_order(members: &[(u32, NonZeroU32)], priority: u32) -> TcxOrder {
    for &(existing, program) in members {
        if existing > priority {
            return TcxOrder::Before(program);
        }
    }
    members
        .last()
        .map_or(TcxOrder::First, |&(_, program)| TcxOrder::After(program))
}

/// Derive managed TCX ranks from one complete store snapshot. This describes
/// operator ordering, not a live kernel inventory or foreign programs.
pub fn tcx_positions(links: &mut [crate::StoredLink]) {
    let mut ordered: alloc::vec::Vec<_> = links
        .iter()
        .filter_map(|r| match &r.details {
            crate::LinkDetails::Tcx(d) => Some((d.key, d.priority, r.id)),
            _ => None,
        })
        .collect();
    ordered.sort();
    for r in links {
        if let crate::LinkDetails::Tcx(d) = &mut r.details {
            d.position = ordered
                .iter()
                .filter(|(key, priority, id)| {
                    *key == d.key && (*priority, *id) < (d.priority, r.id)
                })
                .count();
        }
    }
}
