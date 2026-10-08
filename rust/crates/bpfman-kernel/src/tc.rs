use crate::{Acquisition, Error, Removal};
use bpfman_fs::RuntimeWriter;
use bpfman_model::{InterfaceName, NetworkNamespace, TcProceedOn, TcSnapshot, XdpKey};
use std::num::NonZeroU32;

/// First-member legacy TC ingress ownership; deliberately separate from TCX.
pub trait TcLifecycle {
    /// Adopted extension and retained namespace/interface evidence.
    type Prepared;
    /// Owned dispatcher, extension pins, revision and durable clsact evidence.
    type Stage: Send + Sync + 'static;
    /// Owned exact filter and clsact cleanup, including partial acquisitions.
    type Filter: Send + Sync + 'static;
    /// Resolve the namespace/interface and adopt the managed EXT pin.
    fn prepare_tc(
        &self,
        w: &RuntimeWriter<'_>,
        program: NonZeroU32,
        interface: &InterfaceName,
        netns: &NetworkNamespace,
    ) -> Result<(XdpKey, Self::Prepared), Error>;
    /// Stage the one-member dispatcher and freplace link without traffic effects.
    fn stage_tc(
        &self,
        w: &RuntimeWriter<'_>,
        prepared: Self::Prepared,
        actions: TcProceedOn,
    ) -> Acquisition<Self::Stage>;
    /// Create or reuse clsact and install a legacy netlink filter; never use TCX.
    fn attach_tc_filter(
        &self,
        w: &RuntimeWriter<'_>,
        stage: &Self::Stage,
    ) -> Acquisition<Self::Filter>;
    /// Dispatcher and extension kernel identities ready for publication.
    fn tc_ids(stage: &Self::Stage) -> Result<(NonZeroU32, NonZeroU32), Error>;
    /// Actual filter priority and nonzero kernel-assigned handle.
    fn tc_filter(filter: &Self::Filter) -> Result<(u16, NonZeroU32), Error>;
    /// Validate the complete committed snapshot before teardown.
    fn observe_tc(
        &self,
        w: &RuntimeWriter<'_>,
        snapshot: &TcSnapshot,
    ) -> Result<(Self::Stage, Self::Filter), Error>;
    /// Remove only the exact owned filter, then reclaim only an owned empty clsact.
    fn remove_tc_filter(
        &self,
        w: &RuntimeWriter<'_>,
        filter: Self::Filter,
    ) -> Removal<Self::Filter>;
    /// Remove staged pins/evidence and their empty container after traffic stops.
    fn remove_tc_stage(&self, w: &RuntimeWriter<'_>, stage: Self::Stage) -> Removal<Self::Stage>;
}
