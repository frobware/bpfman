//! First-member legacy TC ingress, separate from TCX and XDP identities.
use crate::{InterfaceName, NetworkNamespace, StoredLink, Symbol, XdpKey};
use alloc::string::String;
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

/// One extension attached to slot zero of a legacy TC ingress dispatcher.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TcLink {
    /// Selected namespace path.
    pub netns: NetworkNamespace,
    /// Namespace inode and interface index; TC ingress uses its own collection.
    pub key: XdpKey,
    /// Validated interface name.
    pub interface: InterfaceName,
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

/// Atomically stored singleton TC ingress attachment.
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

/// Go's 84-byte TC dispatcher CONFIG ABI, with padding after the member count.
pub fn tc_config(proceed_on: TcProceedOn) -> [u8; 84] {
    let mut bytes = [0; 84];
    bytes[0] = 1;
    bytes[4..8].copy_from_slice(&proceed_on.mask().to_ne_bytes());
    for priority in bytes[44..84].chunks_exact_mut(4) {
        priority.copy_from_slice(&50u32.to_ne_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

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
