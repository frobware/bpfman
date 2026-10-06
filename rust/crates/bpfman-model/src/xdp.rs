use crate::StoredLink;
use alloc::string::String;
use core::{
    fmt,
    num::{NonZeroU32, NonZeroU64},
    str::FromStr,
};

/// Network-namespace inode and interface index identifying an XDP attach point.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct XdpKey {
    /// Network namespace inode.
    pub nsid: NonZeroU64,
    /// Interface index within that namespace.
    pub ifindex: NonZeroU32,
}

/// A validated Linux interface name, not a path or a namespace selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InterfaceName(String);

/// Invalid XDP input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidXdp;

impl fmt::Display for InvalidXdp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid XDP interface or proceed-on action")
    }
}

impl core::error::Error for InvalidXdp {}

impl FromStr for InterfaceName {
    type Err = InvalidXdp;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if raw.is_empty()
            || raw.len() >= 16
            || matches!(raw, "." | "..")
            || raw
                .chars()
                .any(|c| c == '/' || c == ':' || c == '\0' || c.is_whitespace())
        {
            return Err(InvalidXdp);
        }
        Ok(Self(raw.into()))
    }
}

impl InterfaceName {
    /// Kernel interface name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Valid dispatcher return-code mask. Codes 0–4 and dispatcher-return 31 exist.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XdpProceedOn(u32);

impl Default for XdpProceedOn {
    fn default() -> Self {
        Self((1 << 2) | (1 << 31))
    }
}

impl TryFrom<u32> for XdpProceedOn {
    type Error = InvalidXdp;

    fn try_from(mask: u32) -> Result<Self, Self::Error> {
        if mask & !0x8000001f != 0 {
            return Err(InvalidXdp);
        }
        Ok(Self(mask))
    }
}

impl XdpProceedOn {
    /// Dispatcher ABI mask.
    pub fn mask(self) -> u32 {
        self.0
    }
}

/// One extension's attachment to a dispatcher revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XdpLink {
    /// Validated position in the dispatcher chain.
    pub slot: crate::XdpSlot,
    /// Owning attach point.
    pub key: XdpKey,
    /// Observed interface name.
    pub interface: InterfaceName,
    /// Requested priority; lower values run first.
    pub priority: u32,
    /// Return codes that continue the chain.
    pub proceed_on: XdpProceedOn,
    /// Dispatcher kernel program ID.
    pub dispatcher_id: NonZeroU32,
    /// Dispatcher revision.
    pub revision: NonZeroU32,
}

/// One member and its dispatcher details from a consistent store observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XdpSnapshot {
    /// Extension name from the same stored snapshot.
    pub program_name: crate::Symbol,
    /// Extension program pin path from the same stored snapshot.
    pub program_pin_path: String,
    /// Dispatcher attachment details shared with its member.
    pub details: XdpLink,
    /// Outer interface link kernel ID.
    pub outer_link_id: NonZeroU32,
    /// Managed extension link.
    pub member: StoredLink,
}

/// Complete nonempty dispatcher snapshot, ordered by contiguous chain slots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XdpDispatcherSnapshot(alloc::vec::Vec<XdpSnapshot>);

/// Inconsistent membership, slots, or shared dispatcher identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidXdpSnapshot;

impl fmt::Display for InvalidXdpSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid XDP dispatcher snapshot")
    }
}

impl core::error::Error for InvalidXdpSnapshot {}

impl XdpDispatcherSnapshot {
    /// Validate capacity, shared dispatcher identity, and distinct member links.
    pub fn new(members: alloc::vec::Vec<XdpSnapshot>) -> Result<Self, InvalidXdpSnapshot> {
        let first = members.first().ok_or(InvalidXdpSnapshot)?;
        if members.len() > crate::XDP_MAX_MEMBERS {
            return Err(InvalidXdpSnapshot);
        }
        for (index, member) in members.iter().enumerate() {
            let crate::LinkState::Attached { kernel_id } = member.member.state else {
                return Err(InvalidXdpSnapshot);
            };
            if member.details.slot.index() != index
                || member.details.key != first.details.key
                || member.details.interface != first.details.interface
                || member.details.dispatcher_id != first.details.dispatcher_id
                || member.details.revision != first.details.revision
                || member.outer_link_id != first.outer_link_id
                || member.member.details != crate::LinkDetails::Xdp(member.details.clone())
                || member.details.priority > i32::MAX as u32
                || kernel_id == member.outer_link_id
                || members[..index].iter().any(|prior| {
                    prior.member.id == member.member.id || prior.member.state == member.member.state
                })
            {
                return Err(InvalidXdpSnapshot);
            }
        }
        Ok(Self(members))
    }

    /// All members in execution order; the slice is never empty.
    pub fn members(&self) -> &[XdpSnapshot] {
        &self.0
    }
}

/// One-slot dispatcher configuration matching Go's C ABI.
pub fn xdp_config(proceed_on: XdpProceedOn) -> [u8; 124] {
    *crate::XdpConfig::single(proceed_on).bytes()
}
