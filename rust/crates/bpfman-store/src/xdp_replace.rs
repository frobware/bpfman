use crate::{Error, ErrorKind, XdpCommit, XdpStore};
use bpfman_core::EffectFailure;
use bpfman_fs::RuntimeWriter;
use bpfman_model::{XDP_MAX_MEMBERS, XdpDispatcherSnapshot, XdpKey};
use std::num::NonZeroU64;

/// Identity of a desired member: preserve an observed link or allocate a new one.
pub enum XdpMemberId {
    /// Surviving managed link from the receipt's snapshot.
    Existing(NonZeroU64),
    /// Allocate a fresh managed link ID in the publication transaction.
    New,
}

/// Fully staged member, with its new kernel link and dispatcher details.
pub struct XdpMemberCommit<'a> {
    /// Stable managed identity or a request to allocate one.
    pub identity: XdpMemberId,
    /// Acquired attachment; revision is the next revision, not necessarily one.
    pub attachment: XdpCommit<'a>,
}

/// Complete desired membership in execution order. Slice indices are slots.
pub struct XdpReplace<'a> {
    /// RFC3339 publication timestamp for the dispatcher header.
    pub updated_at: &'a str,
    /// Between one and ten members, including at most one newly attached member.
    pub members: &'a [XdpMemberCommit<'a>],
}

fn invalid() -> Error {
    Error::new(
        ErrorKind::InvalidData,
        std::io::Error::other("invalid XDP replacement membership or revision"),
    )
}

impl XdpReplace<'_> {
    /// Check revision continuity, stable outer identity, distinct links, and
    /// preservation of every surviving member's program and operator fields.
    pub fn validate(&self, current: &XdpDispatcherSnapshot) -> Result<(), Error> {
        let old = current.members().first().ok_or_else(invalid)?;
        let first = self.members.first().ok_or_else(invalid)?;
        let next = old
            .details
            .revision
            .get()
            .checked_add(1)
            .ok_or_else(invalid)?;
        if self.members.len() > XDP_MAX_MEMBERS {
            return Err(invalid());
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut kernels = std::collections::BTreeSet::new();
        let mut new = false;
        for (position, member) in self.members.iter().enumerate() {
            let a = &member.attachment;
            if a.details.slot.index() != position
                || a.details.key != old.details.key
                || a.details.interface != old.details.interface
                || a.details.netns != old.details.netns
                || a.outer_link_id != old.outer_link_id
                || a.details.revision.get() != next
                || a.details.dispatcher_id == old.details.dispatcher_id
                || a.details.dispatcher_id != first.attachment.details.dispatcher_id
                || a.details.priority > i32::MAX as u32
                || a.extension_link_id == a.outer_link_id
                || !kernels.insert(a.extension_link_id)
            {
                return Err(invalid());
            }
            match member.identity {
                XdpMemberId::New if !new => new = true,
                XdpMemberId::New => return Err(invalid()),
                XdpMemberId::Existing(id) => {
                    let prior = current
                        .members()
                        .iter()
                        .find(|s| s.member.id == id)
                        .ok_or_else(invalid)?;
                    if !ids.insert(id)
                        || a.program_id != prior.member.program_id
                        || a.details.priority != prior.details.priority
                        || a.details.proceed_on != prior.details.proceed_on
                        || a.metadata != &prior.member.metadata
                        || a.created_at != prior.member.created_at
                    {
                        return Err(invalid());
                    }
                }
            }
        }
        Ok(())
    }
}

/// Complete dispatcher observations, including every member in slot order.
pub trait XdpDispatcherReader {
    /// Read every complete supported dispatcher from one snapshot, ordered by key.
    /// Unsupported or malformed state fails rather than producing a partial listing.
    fn read_xdp_dispatchers(&mut self) -> Result<Vec<XdpDispatcherSnapshot>, Error>;

    /// Read atomically; malformed membership is an error, never partial success.
    fn read_xdp_dispatcher(&mut self, key: XdpKey) -> Result<Option<XdpDispatcherSnapshot>, Error>;
}

/// Conditional complete-snapshot publication, independent of kernel switching.
pub trait XdpReplacementStore: XdpStore {
    /// Observe all members and bind conditional evidence to this runtime/store.
    fn observe_xdp_dispatcher(
        &self,
        writer: &RuntimeWriter<'_>,
        key: XdpKey,
    ) -> Result<Option<(XdpDispatcherSnapshot, Self::XdpReceipt)>, Error>;

    /// Atomically replace an unchanged complete snapshot. Success consumes old
    /// evidence; failure commits nothing and returns it for explicit retry.
    /// No fallible observation may occur after publication succeeds.
    fn replace_xdp(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::XdpReceipt,
        request: XdpReplace<'_>,
    ) -> Result<XdpDispatcherSnapshot, EffectFailure<Self::XdpReceipt, Error>>;
}
