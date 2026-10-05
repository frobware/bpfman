use alloc::{collections::BTreeMap, string::String};
use core::{
    fmt,
    num::{NonZeroU32, NonZeroU64},
    str::FromStr,
};

/// A tracepoint in `group/name` form. Components cannot traverse tracefs.
/// This validates syntax only; existence is an attachment-time observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tracepoint {
    group: String,
    name: String,
}

/// An invalid tracepoint name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidTracepoint;

impl fmt::Display for InvalidTracepoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected tracepoint group/name with nonempty, non-traversing components")
    }
}

impl core::error::Error for InvalidTracepoint {}

impl FromStr for Tracepoint {
    type Err = InvalidTracepoint;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        let (group, name) = raw.split_once('/').ok_or(InvalidTracepoint)?;
        let component = |s: &str| {
            !s.is_empty()
                && !matches!(s, "." | "..")
                && !s
                    .chars()
                    .any(|c| c == '/' || c == '\0' || c.is_whitespace())
        };

        if !component(group) || !component(name) {
            return Err(InvalidTracepoint);
        }

        Ok(Self {
            group: group.into(),
            name: name.into(),
        })
    }
}

impl Tracepoint {
    /// Tracefs event group.
    pub fn group(&self) -> &str {
        &self.group
    }

    /// Tracefs event name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for Tracepoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.group, self.name)
    }
}

/// Attachment-specific intent, separate from its persistence and kernel identities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LinkDetails {
    /// A standalone tracepoint attachment.
    Tracepoint(Tracepoint),
    /// XDP extension attached through a dispatcher.
    Xdp(crate::XdpLink),
}

/// Stored progress of the pending-link protocol. Neither variant asserts that
/// the kernel object is still present at observation time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkState {
    /// Intent committed before creating a kernel attachment.
    Pending,
    /// Kernel identity captured and committed after attachment and pinning.
    Attached {
        /// Kernel-assigned BPF link ID, distinct from the managed link handle.
        kernel_id: NonZeroU32,
    },
}

/// Stored standalone link. Paths are observations, never deletion authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredLink {
    /// Store-assigned managed handle.
    pub id: NonZeroU64,
    /// Managed program being attached.
    pub program_id: NonZeroU32,
    /// Attachment intent.
    pub details: LinkDetails,
    /// Pending intent or finalised kernel identity.
    pub state: LinkState,
    /// Canonical link pin location.
    pub pin_path: String,
    /// Operator labels.
    pub metadata: BTreeMap<String, String>,
    /// Validated RFC3339 creation time.
    pub created_at: String,
}

/// Identity observed from a standalone perf-event kernel link.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KernelLink {
    /// Type-specific kernel observations.
    pub details: KernelLinkDetails,
    /// Kernel link ID, distinct from the store handle.
    pub id: NonZeroU32,
    /// Kernel program attached by this link.
    pub program_id: NonZeroU32,
}

/// Stored intent and independently gathered kernel/filesystem observations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedLink {
    /// Committed intent, including pending records.
    pub record: StoredLink,
    /// Matching kernel link when its recorded ID is still present.
    pub kernel: Option<KernelLink>,
    /// A matching link pin was observed below the adopted runtime.
    pub pin_present: bool,
}

/// Type-specific facts observed from a kernel BPF link.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum KernelLinkDetails {
    /// Standalone tracepoint perf-event link.
    PerfEvent,
    /// Dispatcher extension link.
    Tracing {
        /// Kernel attach-type numeric value.
        attach_type: u32,
        /// Target dispatcher program ID.
        target_obj_id: u32,
        /// Target function BTF ID.
        target_btf_id: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracepoint_components_cannot_escape_tracefs() {
        for raw in [
            "", "sched", "/event", "sched/", "a/b/c", "../event", "a/..", "./b", "a/.", "a/ b",
            "a/b\0",
        ] {
            assert!(raw.parse::<Tracepoint>().is_err(), "{raw:?}");
        }

        let target = "sched/sched_switch".parse::<Tracepoint>();

        assert_eq!(target.as_ref().map(Tracepoint::group), Ok("sched"));
        assert_eq!(target.as_ref().map(Tracepoint::name), Ok("sched_switch"));
    }
}
