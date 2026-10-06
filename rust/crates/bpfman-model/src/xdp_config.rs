use crate::XdpProceedOn;
use core::fmt;

/// Number of extension slots in the XDP v2 dispatcher ABI.
pub const XDP_MAX_MEMBERS: usize = 10;

/// A valid extension slot in an XDP dispatcher.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct XdpSlot(u8);

/// Invalid dispatcher capacity, slot, or priority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidXdpConfig {
    /// A dispatcher must contain between one and ten extensions.
    Capacity,
    /// Fragment declarations must match the active member count.
    FragmentCount,
    /// A slot must be between zero and nine.
    Slot,
    /// Go stores priorities as nonnegative signed 32-bit integers.
    Priority,
}

impl fmt::Display for InvalidXdpConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Capacity => "XDP dispatcher requires between one and ten members",
            Self::FragmentCount => "XDP fragment declarations must match member count",
            Self::Slot => "XDP dispatcher slot must be between zero and nine",
            Self::Priority => "XDP priority exceeds i32::MAX",
        })
    }
}

impl core::error::Error for InvalidXdpConfig {}

impl TryFrom<usize> for XdpSlot {
    type Error = InvalidXdpConfig;

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        if value < XDP_MAX_MEMBERS {
            Ok(Self(value as u8))
        } else {
            Err(InvalidXdpConfig::Slot)
        }
    }
}

impl XdpSlot {
    /// First slot, used for initial attachment.
    pub const FIRST: Self = Self(0);

    /// Zero-based chain position.
    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// A validated priority; lower values execute first, including zero.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct XdpPriority(u32);

impl TryFrom<u32> for XdpPriority {
    type Error = InvalidXdpConfig;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        if value <= i32::MAX as u32 {
            Ok(Self(value))
        } else {
            Err(InvalidXdpConfig::Priority)
        }
    }
}

impl XdpPriority {
    /// User-specified priority, independent of the dispatcher's ABI run priority.
    pub fn get(self) -> u32 {
        self.0
    }
}

/// Complete native-endian XDP v2 configuration, matching Go's C ABI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XdpConfig([u8; 124]);

impl XdpConfig {
    /// Construct the ABI configuration in execution order. Unused actions and
    /// all flags remain zero; all ten ABI run priorities are always 50.
    pub fn new(actions: &[XdpProceedOn]) -> Result<Self, InvalidXdpConfig> {
        if actions.is_empty() || actions.len() > XDP_MAX_MEMBERS {
            return Err(InvalidXdpConfig::Capacity);
        }
        Ok(Self::encode(actions))
    }

    /// Configuration for a first attachment or an unpinned verification dispatcher.
    pub fn single(action: XdpProceedOn) -> Self {
        Self::encode(&[action])
    }

    fn encode(actions: &[XdpProceedOn]) -> Self {
        let mut bytes = [0; 124];
        bytes[..4].copy_from_slice(&[236, 2, actions.len() as u8, 0]);
        for (word, action) in bytes[4..44].chunks_exact_mut(4).zip(actions) {
            word.copy_from_slice(&action.mask().to_ne_bytes());
        }
        for priority in bytes[44..84].chunks_exact_mut(4) {
            priority.copy_from_slice(&50u32.to_ne_bytes());
        }
        Self(bytes)
    }

    /// Apply each member's declared multi-buffer support in slot order.
    /// The dispatcher enables fragments only when every member opts in.
    pub fn with_frags(mut self, members: &[bool]) -> Result<Self, InvalidXdpConfig> {
        if members.len() != usize::from(self.0[2]) {
            return Err(InvalidXdpConfig::FragmentCount);
        }
        self.0[3] = u8::from(members.iter().all(|&supported| supported));
        for (word, &supported) in self.0[84..124].chunks_exact_mut(4).zip(members) {
            // Linux BPF_F_XDP_HAS_FRAGS, independent of interface attach flags.
            word.copy_from_slice(&(if supported { 1u32 << 5 } else { 0 }).to_ne_bytes());
        }
        Ok(self)
    }

    /// Whether every member declares support for multi-buffer XDP packets.
    pub fn supports_frags(&self) -> bool {
        self.0[3] != 0
    }

    /// Bytes to supply to the dispatcher's `conf` global.
    pub fn bytes(&self) -> &[u8; 124] {
        &self.0
    }
}
