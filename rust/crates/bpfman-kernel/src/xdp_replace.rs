use crate::{Acquisition, Error, Removal, XdpLifecycle};
use bpfman_fs::RuntimeWriter;
use bpfman_model::{XdpConfig, XdpDispatcherSnapshot, XdpSlot};
use std::num::NonZeroU32;

/// Complete-revision staging and conditional switching of the durable XDP link.
pub trait XdpReplacement: XdpLifecycle {
    /// Retained restoration evidence binding the runtime, outer link, and targets.
    type Switch: Send + Sync + 'static;

    /// Validate retained managed-program evidence before continuing an admitted
    /// multi-link operation. Must reject another runtime or kernel instance.
    fn validate_prepared_xdp(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: &Self::PreparedXdp,
    ) -> Result<(), Error>;

    /// Load the complete validated dispatcher configuration.
    fn load_revision(&self, config: &XdpConfig) -> Result<Self::Dispatcher, Error>;
    /// Exclusively create a selected revision without adopting prior residue.
    fn create_revision_at(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: &Self::PreparedXdp,
        revision: NonZeroU32,
    ) -> Acquisition<Self::Revision>;
    /// Attach and pin one member at its validated position.
    fn pin_extension_at(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: &mut Self::PreparedXdp,
        revision: &Self::Revision,
        dispatcher: &Self::Dispatcher,
        slot: XdpSlot,
    ) -> Acquisition<Self::Extension>;
    /// Adopt all known artifacts and refuse unknown children or mismatched identities.
    fn observe_dispatcher(
        &self,
        writer: &RuntimeWriter<'_>,
        snapshot: &XdpDispatcherSnapshot,
    ) -> Result<XdpDispatcherArtifacts<Self>, Error>;
    /// Atomically change the expected old target. Failure without a receipt is
    /// rejection before mutation; failure with a receipt requires restoration.
    fn switch_dispatcher(
        &self,
        writer: &RuntimeWriter<'_>,
        outer: &Self::Outer,
        old: &Self::DispatcherPin,
        new: &Self::DispatcherPin,
    ) -> Acquisition<Self::Switch>;
    /// Restore once. Every failure retains both target handles for explicit retry.
    fn restore_dispatcher(
        &self,
        writer: &RuntimeWriter<'_>,
        receipt: Self::Switch,
    ) -> Removal<Self::Switch>;
}

/// Complete preflight ownership, with missing artifacts represented for teardown retries.
pub struct XdpDispatcherArtifacts<K: XdpLifecycle + ?Sized> {
    /// Durable outer attachment.
    pub outer: Option<K::Outer>,
    /// All observed member extension pins in execution order.
    pub extensions: Vec<K::Extension>,
    /// Dispatcher program pin.
    pub program: Option<K::DispatcherPin>,
    /// Dependent revision container.
    pub directory: Option<K::Revision>,
}
