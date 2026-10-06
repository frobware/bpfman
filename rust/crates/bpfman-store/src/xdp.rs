use crate::{Error, OpenStore};
use bpfman_core::EffectFailure;
use bpfman_fs::RuntimeWriter;
use bpfman_model::{StoredLink, XdpKey, XdpLink, XdpSnapshot};
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
};

/// Fully acquired first attachment, ready for atomic publication.
pub struct XdpCommit<'a> {
    /// Extension program ID.
    pub program_id: NonZeroU32,
    /// Interface and dispatcher identity.
    pub details: &'a XdpLink,
    /// Extension link kernel ID.
    pub extension_link_id: NonZeroU32,
    /// Outer XDP link kernel ID.
    pub outer_link_id: NonZeroU32,
    /// Operator labels.
    pub metadata: &'a BTreeMap<String, String>,
    /// RFC3339 timestamp.
    pub created_at: &'a str,
}

/// Single-member observations for the currently supported kernel lifecycle.
/// Complete membership is available through `crate::XdpDispatcherReader`.
pub trait XdpReader {
    /// Read a singleton dispatcher; multiple members are an error, never truncated.
    fn read_xdp(&mut self, key: XdpKey) -> Result<Option<XdpSnapshot>, Error>;
}

/// Atomic first-attach publication and conditional last-detach deletion.
pub trait XdpStore: OpenStore {
    /// Owned evidence for an unchanged complete snapshot and its runtime/store.
    type XdpReceipt: Send + Sync + 'static;

    /// Validate supported format, managed XDP program, and vacant attach point.
    fn preflight_xdp(
        &self,
        writer: &RuntimeWriter<'_>,
        key: XdpKey,
        program: NonZeroU32,
    ) -> Result<(), Error>;

    /// Allocate a managed link and publish the entire snapshot. Err means no commit.
    fn commit_xdp(
        &self,
        writer: &RuntimeWriter<'_>,
        request: XdpCommit<'_>,
    ) -> Result<StoredLink, Error>;

    /// Observe a singleton with conditional evidence; reject multi-member dispatchers.
    fn observe_xdp(
        &self,
        writer: &RuntimeWriter<'_>,
        link: NonZeroU64,
    ) -> Result<Option<(XdpSnapshot, Self::XdpReceipt)>, Error>;

    /// Delete only the unchanged snapshot after its kernel/filesystem prerequisites.
    fn delete_xdp(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::XdpReceipt,
    ) -> Result<(), EffectFailure<Self::XdpReceipt, Error>>;
}

impl XdpCommit<'_> {
    /// Validate invariants required of a first-attach snapshot before mutation.
    pub fn validate(&self) -> Result<(), Error> {
        if self.details.slot != bpfman_model::XdpSlot::FIRST
            || self.details.priority > i32::MAX as u32
            || self.details.revision != NonZeroU32::MIN
            || self.extension_link_id == self.outer_link_id
        {
            return Err(Error::new(
                crate::ErrorKind::InvalidData,
                std::io::Error::other(
                    "first XDP snapshot requires slot zero, revision one, a valid priority, and distinct kernel links",
                ),
            ));
        }
        Ok(())
    }
}
