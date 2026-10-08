use crate::{Error, OpenStore};
use bpfman_core::EffectFailure;
use bpfman_fs::RuntimeWriter;
use bpfman_model::{StoredLink, TcLink, TcSnapshot, XdpKey};
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
};

/// Complete acquisitions ready for atomic first-member publication.
pub struct TcCommit<'a> {
    /// Managed extension ID.
    pub program_id: NonZeroU32,
    /// Exact filter and dispatcher identity.
    pub details: &'a TcLink,
    /// Pinned freplace link ID.
    pub extension_link_id: NonZeroU32,
    /// Operator labels.
    pub metadata: &'a BTreeMap<String, String>,
    /// RFC3339 timestamp.
    pub created_at: &'a str,
}

/// Atomic TC ingress publication and conditional complete-snapshot mutation.
pub trait TcStore: OpenStore {
    /// Non-cloneable evidence for the original runtime, store, and snapshot.
    type TcReceipt: Send + Sync + 'static;
    /// Refuse occupied TC ingress attach points and non-TC programs.
    fn preflight_tc(
        &self,
        w: &RuntimeWriter<'_>,
        key: XdpKey,
        program: NonZeroU32,
    ) -> Result<(), Error>;
    /// Publish the whole acquired snapshot. Err guarantees no commit.
    fn commit_tc(&self, w: &RuntimeWriter<'_>, request: TcCommit<'_>) -> Result<StoredLink, Error>;
    /// Observe the exact snapshot and conditional deletion evidence.
    fn observe_tc(
        &self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU64,
    ) -> Result<Option<(TcSnapshot, Self::TcReceipt)>, Error>;
    /// Observe the complete dispatcher containing a managed member.
    fn observe_tc_member_dispatcher(
        &self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU64,
    ) -> Result<Option<(bpfman_model::TcDispatcherSnapshot, Self::TcReceipt)>, Error>;
    /// Observe the complete dispatcher and conditional publication evidence.
    fn observe_tc_dispatcher(
        &self,
        w: &RuntimeWriter<'_>,
        key: XdpKey,
    ) -> Result<Option<(bpfman_model::TcDispatcherSnapshot, Self::TcReceipt)>, Error>;
    /// Replace only the unchanged complete snapshot. Failure guarantees no publication.
    fn replace_tc(
        &self,
        w: &RuntimeWriter<'_>,
        receipt: Self::TcReceipt,
        request: TcReplace<'_>,
    ) -> Result<bpfman_model::TcDispatcherSnapshot, EffectFailure<Self::TcReceipt, Error>>;
    /// Delete only unchanged intent after all kernel/filesystem prerequisites.
    fn delete_tc(
        &self,
        w: &RuntimeWriter<'_>,
        receipt: Self::TcReceipt,
    ) -> Result<(), EffectFailure<Self::TcReceipt, Error>>;
}

/// Fully staged desired member; surviving links retain their managed IDs.
pub struct TcMemberCommit<'a> {
    /// Stable identity or one newly allocated link.
    pub identity: crate::XdpMemberId,
    /// Acquired TC attachment for the next revision.
    pub attachment: TcCommit<'a>,
}

/// Complete nonempty desired TC membership in execution order.
pub struct TcReplace<'a> {
    /// RFC3339 dispatcher publication timestamp.
    pub updated_at: &'a str,
    /// Between one and ten members with at most one new link.
    pub members: &'a [TcMemberCommit<'a>],
}

impl TcReplace<'_> {
    /// Validate revision continuity, exact filter identity and unchanged survivors.
    pub fn validate(&self, current: &bpfman_model::TcDispatcherSnapshot) -> Result<(), Error> {
        let invalid = || {
            Error::new(
                crate::ErrorKind::InvalidData,
                std::io::Error::other("invalid TC replacement membership or revision"),
            )
        };
        let old = current.members().first().ok_or_else(invalid)?;
        let first = self.members.first().ok_or_else(invalid)?;
        let next = old.details.revision.checked_add(1).ok_or_else(invalid)?;
        if self.members.len() > 10 {
            return Err(invalid());
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut kernels = std::collections::BTreeSet::new();
        let mut new = false;
        for (slot, member) in self.members.iter().enumerate() {
            let a = &member.attachment;
            let d = a.details;
            let o = &old.details;
            if d.slot.index() != slot
                || d.key != o.key
                || d.interface != o.interface
                || d.netns != o.netns
                || d.filter_handle != o.filter_handle
                || d.filter_priority != o.filter_priority
                || d.revision != next
                || d.dispatcher_id == o.dispatcher_id
                || d.dispatcher_id != first.attachment.details.dispatcher_id
                || d.priority > i32::MAX as u32
                || !kernels.insert(a.extension_link_id)
            {
                return Err(invalid());
            }
            match member.identity {
                crate::XdpMemberId::New if !new => new = true,
                crate::XdpMemberId::New => return Err(invalid()),
                crate::XdpMemberId::Existing(id) => {
                    let prior = current
                        .members()
                        .iter()
                        .find(|p| p.member.id == id)
                        .ok_or_else(invalid)?;
                    if !ids.insert(id)
                        || a.program_id != prior.member.program_id
                        || d.priority != prior.details.priority
                        || d.proceed_on != prior.details.proceed_on
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
