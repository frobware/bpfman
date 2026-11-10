use super::*;
use crate::{LinkDetails, LinkState, StoredLink, TcDirection};
use alloc::{collections::BTreeMap, string::String, vec};
use core::num::NonZeroU64;

#[test]
fn ranks_use_priority_then_identity_with_independent_hooks_and_preserve_input_order() {
    let mut links = vec![];
    for (id, priority, direction, ifindex, nsid) in [
        (5, 50, TcDirection::Ingress, 1, 1),
        (9, 0, TcDirection::Egress, 1, 1),
        (2, 50, TcDirection::Ingress, 1, 1),
        (3, 10, TcDirection::Ingress, 1, 1),
        (7, 0, TcDirection::Ingress, 2, 1),
        (8, 0, TcDirection::Ingress, 1, 2),
    ] {
        links.push(StoredLink {
            id: NonZeroU64::new(id).expect("id"),
            program_id: NonZeroU32::MIN,
            details: LinkDetails::Tcx(TcxLink {
                key: TcKey {
                    nsid: NonZeroU64::new(nsid).expect("namespace"),
                    ifindex: NonZeroU32::new(ifindex).expect("interface"),
                    direction,
                },
                interface: "fake0".parse().expect("name"),
                netns: Default::default(),
                priority,
                position: usize::MAX,
            }),
            state: LinkState::Pending,
            pin_path: String::new(),
            metadata: BTreeMap::new(),
            created_at: String::new(),
        });
    }
    tcx_positions(&mut links);
    assert_eq!(
        links
            .iter()
            .map(|link| link.id.get())
            .collect::<alloc::vec::Vec<_>>(),
        [5, 9, 2, 3, 7, 8]
    );
    assert_eq!(
        links
            .iter()
            .filter_map(|link| match &link.details {
                LinkDetails::Tcx(details) => Some(details.position),
                _ => None,
            })
            .collect::<alloc::vec::Vec<_>>(),
        [2, 0, 1, 0, 0, 0]
    );
    let first = links.clone();
    tcx_positions(&mut links);
    assert_eq!(links, first);
    tcx_positions(&mut []);
}
