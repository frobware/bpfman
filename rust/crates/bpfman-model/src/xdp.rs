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

/// Complete single-member XDP snapshot. Persistence publishes it atomically.
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
    /// Sole managed extension link.
    pub member: StoredLink,
}

/// One-slot dispatcher configuration matching Go's C ABI.
pub fn xdp_config(proceed_on: XdpProceedOn) -> [u8; 124] {
    *crate::XdpConfig::single(proceed_on).bytes()
}
