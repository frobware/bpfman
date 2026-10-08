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

/// Atomic singleton TC ingress publication and conditional deletion.
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
    /// Delete only unchanged intent after all kernel/filesystem prerequisites.
    fn delete_tc(
        &self,
        w: &RuntimeWriter<'_>,
        receipt: Self::TcReceipt,
    ) -> Result<(), EffectFailure<Self::TcReceipt, Error>>;
}
