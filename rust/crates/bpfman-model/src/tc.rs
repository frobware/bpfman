//! Legacy TC ingress, separate from TCX and XDP identities.
use crate::{InterfaceName, NetworkNamespace, StoredLink, Symbol, XdpKey, XdpSlot};
use alloc::{string::String, vec::Vec};
use core::{fmt, num::NonZeroU32};

/// TC return-code mask; bit zero represents -1, bit 31 dispatcher-return 30.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TcProceedOn(u32);

/// Unsupported TC proceed-on code or mask.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidTc;

impl fmt::Display for InvalidTc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected TC actions -1 through 8 or dispatcher-return 30")
    }
}

impl core::error::Error for InvalidTc {}

impl Default for TcProceedOn {
    fn default() -> Self {
        Self((1 << 4) | (1 << 31))
    }
}

impl TryFrom<u32> for TcProceedOn {
    type Error = InvalidTc;

    fn try_from(mask: u32) -> Result<Self, Self::Error> {
        if mask & !0x800003ff != 0 {
            return Err(InvalidTc);
        }
        Ok(Self(mask))
    }
}

impl TcProceedOn {
    /// Native dispatcher ABI bitmask, shifted by one from the signed action.
    pub fn mask(self) -> u32 {
        self.0
    }
}

/// One extension attached to a legacy TC ingress dispatcher revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TcLink {
    /// Selected namespace path.
    pub netns: NetworkNamespace,
    /// Namespace inode and interface index; TC ingress uses its own collection.
    pub key: XdpKey,
    /// Validated interface name.
    pub interface: InterfaceName,
    /// Complete dispatcher revision.
    pub revision: NonZeroU32,
    /// Contiguous execution position.
    pub slot: XdpSlot,
    /// Requested member priority.
    pub priority: u32,
    /// Actions that continue to the dispatcher's final TC_ACT_OK.
    pub proceed_on: TcProceedOn,
    /// Native dispatcher kernel ID.
    pub dispatcher_id: NonZeroU32,
    /// Exact kernel-assigned filter handle, never a wildcard.
    pub filter_handle: NonZeroU32,
    /// Actual legacy filter priority, distinct from member priority.
    pub filter_priority: u16,
}

/// One member of an atomically stored TC ingress dispatcher.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TcSnapshot {
    /// Selected extension name.
    pub program_name: Symbol,
    /// Canonical managed program pin.
    pub program_pin_path: String,
    /// Dispatcher and filter facts.
    pub details: TcLink,
    /// Managed extension link and operator labels.
    pub member: StoredLink,
}

/// Malformed or incomplete TC dispatcher membership.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidTcSnapshot;

impl fmt::Display for InvalidTcSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid TC dispatcher membership or header")
    }
}

impl core::error::Error for InvalidTcSnapshot {}

/// Complete legacy TC ingress membership in execution order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TcDispatcherSnapshot(Vec<TcSnapshot>);

impl TcDispatcherSnapshot {
    /// Validate a complete, nonempty snapshot with contiguous slots and one header.
    pub fn new(mut members: Vec<TcSnapshot>) -> Result<Self, InvalidTcSnapshot> {
        members.sort_by_key(|m| m.details.slot.index());
        let first = members.first().ok_or(InvalidTcSnapshot)?;
        if members.len() > 10 {
            return Err(InvalidTcSnapshot);
        }
        for (slot, m) in members.iter().enumerate() {
            let d = &m.details;
            let f = &first.details;
            if d.slot.index() != slot
                || d.key != f.key
                || d.revision != f.revision
                || d.dispatcher_id != f.dispatcher_id
                || d.filter_handle != f.filter_handle
                || d.filter_priority != 50
                || d.netns != f.netns
                || d.interface != f.interface
                || d.priority > i32::MAX as u32
                || m.member.details != crate::LinkDetails::Tc(d.clone())
                || !matches!(m.member.state, crate::LinkState::Attached { .. })
                || members[..slot].iter().any(|prior| {
                    prior.member.id == m.member.id || prior.member.state == m.member.state
                })
            {
                return Err(InvalidTcSnapshot);
            }
        }
        Ok(Self(members))
    }

    /// Validated members, sorted by execution slot.
    pub fn members(&self) -> &[TcSnapshot] {
        &self.0
    }
}

/// Go's 84-byte TC dispatcher CONFIG ABI, with padding after the member count.
pub fn tc_config(proceed_on: TcProceedOn) -> [u8; 84] {
    // The singleton is always within the dispatcher's capacity.
    let mut bytes = [0; 84];
    bytes[0] = 1;
    bytes[4..8].copy_from_slice(&proceed_on.mask().to_ne_bytes());
    for priority in bytes[44..84].chunks_exact_mut(4) {
        priority.copy_from_slice(&50u32.to_ne_bytes());
    }
    bytes
}

/// Encode all signed TC continuation masks without using the XDP ABI.
pub fn tc_revision_config(actions: &[TcProceedOn]) -> Result<[u8; 84], InvalidTc> {
    if actions.is_empty() || actions.len() > 10 {
        return Err(InvalidTc);
    }
    let mut bytes = tc_config(TcProceedOn::try_from(0)?);
    bytes[0] = actions.len() as u8;
    for (i, action) in actions.iter().enumerate() {
        bytes[4 + i * 4..8 + i * 4].copy_from_slice(&action.mask().to_ne_bytes());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_tc_config_keeps_signed_masks_and_padding() {
        let actions: Vec<_> = (-1..=8)
            .map(|code| TcProceedOn::try_from(1u32 << (code + 1)).expect("action"))
            .collect();
        let bytes = tc_revision_config(&actions).expect("ten slots");
        assert_eq!(&bytes[..4], &[10, 0, 0, 0]);
        for (slot, action) in actions.iter().enumerate() {
            assert_eq!(
                &bytes[4 + slot * 4..8 + slot * 4],
                &action.mask().to_ne_bytes()
            );
        }
        for priority in bytes[44..84].chunks_exact(4) {
            assert_eq!(priority, &50u32.to_ne_bytes());
        }
        assert!(tc_revision_config(&[]).is_err());
        assert!(tc_revision_config(&[TcProceedOn::default(); 11]).is_err());
        assert_eq!(
            tc_revision_config(&[TcProceedOn::default()]).expect("one"),
            tc_config(TcProceedOn::default())
        );
    }

    #[test]
    fn signed_actions_and_config_match_tc_abi() {
        assert_eq!(TcProceedOn::default().mask(), 0x80000010);
        assert!(TcProceedOn::try_from(1).is_ok());
        assert!(TcProceedOn::try_from(1 << 10).is_err());
        let bytes = tc_config(TcProceedOn::default());
        assert_eq!(&bytes[..4], &[1, 0, 0, 0]);
        assert_eq!(&bytes[4..8], &0x80000010u32.to_ne_bytes());
        assert!(bytes[8..44].iter().all(|b| *b == 0));
        assert_eq!(&bytes[44..48], &50u32.to_ne_bytes());
    }
}
