use crate::{Acquisition, Error, Removal};
use bpfman_fs::RuntimeWriter;
use bpfman_model::{InterfaceName, NetworkNamespace, TcProceedOn, TcSnapshot, XdpKey};
use std::num::NonZeroU32;

/// Legacy TC ingress ownership; deliberately separate from TCX.
pub trait TcLifecycle {
    /// Adopted extension and retained namespace/interface evidence.
    type Prepared;
    /// Owned dispatcher, extension pins, revision and durable clsact evidence.
    type Stage: Send + Sync + 'static;
    /// Owned exact filter and clsact cleanup, including partial acquisitions.
    type Filter: Send + Sync + 'static;
    /// Retained exact filter and both native targets for restoration.
    type Switch: Send + Sync + 'static;
    /// Stage a complete fresh revision using adopted programs in execution order.
    fn stage_tc_revision(
        &self,
        w: &RuntimeWriter<'_>,
        prepared: Vec<Self::Prepared>,
        revision: NonZeroU32,
        actions: &[TcProceedOn],
        clsact_owned: bool,
    ) -> Acquisition<Self::Stage>;
    /// Complete staged native and extension identities in slot order.
    fn tc_revision_ids(stage: &Self::Stage) -> Result<(NonZeroU32, Vec<NonZeroU32>), Error>;
    /// Validate complete active ownership before replacement or member removal.
    fn observe_tc_dispatcher(
        &self,
        w: &RuntimeWriter<'_>,
        snapshot: &bpfman_model::TcDispatcherSnapshot,
    ) -> Result<(Self::Stage, Self::Filter), Error>;
    /// Read retained durable clsact ownership for the next revision.
    fn tc_clsact_owned(&self, w: &RuntimeWriter<'_>, stage: &Self::Stage) -> Result<bool, Error>;
    /// Replace the exact old filter, retaining restoration on any possible mutation.
    fn switch_tc(
        &self,
        w: &RuntimeWriter<'_>,
        filter: &Self::Filter,
        old: &Self::Stage,
        new: &Self::Stage,
    ) -> Acquisition<Self::Switch>;
    /// Restore the original target once; retain both handles on failure.
    fn restore_tc(&self, w: &RuntimeWriter<'_>, receipt: Self::Switch) -> Removal<Self::Switch>;
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
