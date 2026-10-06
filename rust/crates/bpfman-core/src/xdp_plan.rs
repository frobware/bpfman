//! Membership planning follows manager/executor_dispatcher.go's stable ordering.

use alloc::{collections::BTreeSet, vec::Vec};
use bpfman_model::{Symbol, XDP_MAX_MEMBERS, XdpConfig, XdpPriority, XdpProceedOn, XdpSlot};
use core::{fmt, num::NonZeroU32, num::NonZeroU64};

/// Identity used when publishing a desired member. New members receive their
/// durable link ID at atomic publication; survivors preserve their existing ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XdpMemberIdentity {
    /// A member being attached by this operation.
    New,
    /// An already attached member retained by this operation.
    Existing(NonZeroU64),
}

/// Pure ordering input. The interpreter retains metadata, program pins, and
/// kernel evidence alongside this value, indexed by the plan's source index.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XdpMemberOrder {
    /// New or existing managed link identity.
    pub identity: XdpMemberIdentity,
    /// Validated ELF program name, used as Go's final sorting key.
    pub name: Symbol,
    /// Validated requested priority.
    pub priority: XdpPriority,
    /// Return codes allowing execution of the next member.
    pub proceed_on: XdpProceedOn,
}

/// One desired member's position. All other member data stays with its input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct XdpPlacement {
    source: usize,
    slot: XdpSlot,
}

impl XdpPlacement {
    /// Index in the caller's complete desired-member input.
    pub fn source(self) -> usize {
        self.source
    }

    /// Contiguous execution slot in the new revision.
    pub fn slot(self) -> XdpSlot {
        self.slot
    }
}

/// Validated nonempty revision plan, containing no mutation authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XdpRevisionPlan {
    revision: NonZeroU32,
    placements: Vec<XdpPlacement>,
    config: XdpConfig,
}

impl XdpRevisionPlan {
    /// Revision one for first attach, otherwise the checked successor.
    pub fn revision(&self) -> NonZeroU32 {
        self.revision
    }

    /// Complete membership in execution order; unused slots are reclaimed.
    pub fn placements(&self) -> &[XdpPlacement] {
        &self.placements
    }

    /// Complete dispatcher configuration paired with this exact ordering.
    pub fn config(&self) -> &XdpConfig {
        &self.config
    }
}

/// Desired nonempty revision or explicit last-member removal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XdpMembershipPlan {
    /// Stage a complete revision before attaching or replacing the outer link.
    Revision(XdpRevisionPlan),
    /// No dispatcher is loaded when the final member is detached.
    Remove,
}

/// Invalid membership or exhausted revision space, detected before effects.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum XdpPlanError {
    /// More members than the embedded dispatcher's ten slots.
    Capacity,
    /// An existing managed link occurred more than once.
    DuplicateLink,
    /// First attach must have exactly one new member; replacement admits at
    /// most one new member per operation.
    Membership,
    /// Revision increment would wrap and reuse an old artifact identity.
    RevisionExhausted,
}

impl fmt::Display for XdpPlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Capacity => "no free XDP dispatcher slots (all ten occupied)",
            Self::DuplicateLink => "duplicate managed link in XDP membership",
            Self::Membership => "invalid desired XDP membership",
            Self::RevisionExhausted => "XDP dispatcher revision exhausted",
        })
    }
}

impl core::error::Error for XdpPlanError {}

/// Plan a complete desired membership from an already observed revision.
///
/// Ordering matches Go: priority, unattached before attached, then program name.
/// Exact ties retain input order, so observations must supply members in their
/// stored order. Kernel program IDs are deliberately not an extra tie-breaker.
/// This function does not adopt pins or infer that an attach point is managed.
pub fn plan_xdp_membership(
    current_revision: Option<NonZeroU32>,
    desired: &[XdpMemberOrder],
) -> Result<XdpMembershipPlan, XdpPlanError> {
    if desired.len() > XDP_MAX_MEMBERS {
        return Err(XdpPlanError::Capacity);
    }
    let mut identities = BTreeSet::new();
    let mut new_members = 0;
    for member in desired {
        match member.identity {
            XdpMemberIdentity::New => new_members += 1,
            XdpMemberIdentity::Existing(id) => {
                if !identities.insert(id) {
                    return Err(XdpPlanError::DuplicateLink);
                }
            }
        }
    }
    if new_members > 1 || (current_revision.is_none() && (desired.len() != 1 || new_members != 1)) {
        return Err(XdpPlanError::Membership);
    }
    if desired.is_empty() {
        return Ok(XdpMembershipPlan::Remove);
    }
    let revision = match current_revision {
        None => NonZeroU32::MIN,
        Some(revision) => revision
            .checked_add(1)
            .ok_or(XdpPlanError::RevisionExhausted)?,
    };
    let mut sources: Vec<_> = (0..desired.len()).collect();
    sources.sort_by(|&a, &b| {
        let a = &desired[a];
        let b = &desired[b];
        a.priority
            .cmp(&b.priority)
            .then_with(|| {
                matches!(a.identity, XdpMemberIdentity::Existing(_))
                    .cmp(&matches!(b.identity, XdpMemberIdentity::Existing(_)))
            })
            .then_with(|| a.name.cmp(&b.name))
    });
    let actions: Vec<_> = sources.iter().map(|&i| desired[i].proceed_on).collect();
    let placements = sources
        .into_iter()
        .enumerate()
        .map(|(slot, source)| {
            Ok(XdpPlacement {
                source,
                slot: slot.try_into().map_err(|_| XdpPlanError::Capacity)?,
            })
        })
        .collect::<Result<_, XdpPlanError>>()?;
    Ok(XdpMembershipPlan::Revision(XdpRevisionPlan {
        revision,
        placements,
        config: XdpConfig::new(&actions).map_err(|_| XdpPlanError::Capacity)?,
    }))
}
