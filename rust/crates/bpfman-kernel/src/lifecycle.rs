//! Owned kernel capabilities. Success consumes cleanup receipts; failure retains them.
use crate::Error;
use bpfman_core::EffectFailure;
use bpfman_fs::{Bytecode, RuntimeWriter};
use bpfman_model::{
    InterfaceName, ProgramSpec, Symbol, Tracepoint, XdpKey, XdpProceedOn, XdpSnapshot,
};
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
};

/// A partial acquisition retains all ownership needed for explicit compensation.
pub type Acquisition<T> = Result<T, EffectFailure<Option<T>, Error>>;
/// A failed cleanup returns its original, non-cloneable receipt.
pub type Removal<T> = Result<(), EffectFailure<T, Error>>;

/// ELF metadata validated before runtime initialization.
pub struct ObjectInfo {
    /// Validated ELF license.
    pub license: String,
    /// Candidate private map names.
    pub maps: Vec<String>,
}

/// Local ELF inspection without kernel mutation or runtime initialization.
pub trait ObjectLoader {
    /// Validate a selection and private map names, retaining no live kernel objects.
    fn validate_object(&self, bytes: &[u8], spec: &ProgramSpec) -> Result<ObjectInfo, Error>;
    /// Validate global names and sizes against the captured ELF.
    fn validate_globals(
        &self,
        bytes: &[u8],
        globals: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), Error>;
}

/// Owned private program/map pins and their dependent container.
/// Implementations must revalidate backend, runtime and resource identity on removal.
pub trait ProgramResources {
    /// Non-cloneable program pin ownership.
    type ProgramPin: Send + Sync + 'static;
    /// Non-cloneable map pin ownership.
    type MapPin: Send + Sync + 'static;
    /// Non-cloneable private map container ownership.
    type MapDirectory: Send + Sync + 'static;

    /// Stable identity of an owned program pin.
    fn program_id(pin: &Self::ProgramPin) -> NonZeroU32;
    /// Remove only the owned pin, returning ownership on failure.
    fn remove_program(
        &self,
        writer: &RuntimeWriter<'_>,
        pin: Self::ProgramPin,
    ) -> Removal<Self::ProgramPin>;
    /// Remove only the owned map pin.
    fn remove_map(&self, writer: &RuntimeWriter<'_>, pin: Self::MapPin) -> Removal<Self::MapPin>;
    /// Remove an empty owned map container after its pins have been removed.
    fn remove_map_directory(
        &self,
        writer: &RuntimeWriter<'_>,
        directory: Self::MapDirectory,
    ) -> Removal<Self::MapDirectory>;
    /// Adopt canonical artifacts after stored ownership has been validated.
    fn observe_unload(
        &self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<UnloadArtifacts<Self>, Error>;
}

/// Validated artifacts for teardown. Missing objects need no receipt.
pub struct UnloadArtifacts<K: ProgramResources + ?Sized> {
    /// Program pin.
    pub program: Option<K::ProgramPin>,
    /// Individual private map pins.
    pub maps: Vec<K::MapPin>,
    /// Dependent empty map container.
    pub directory: Option<K::MapDirectory>,
    /// Bytecode remains a real filesystem artifact for every backend.
    pub bytecode: Option<Bytecode>,
}

/// Load and publish private program/map pins without committing store records.
pub trait ProgramLoad: ObjectLoader + ProgramResources {
    /// Root-bound readiness for pin acquisition.
    type Prepared;
    /// Owned live program and map handles, released when dropped.
    type Loaded;

    /// Prepare canonical managed pin collections under writer authority.
    fn prepare_load(&self, writer: &RuntimeWriter<'_>) -> Result<Self::Prepared, Error>;
    /// Load one selection from captured, validated bytes.
    fn load_program(
        &self,
        writer: &RuntimeWriter<'_>,
        bytes: &[u8],
        maps: &[String],
        globals: &BTreeMap<String, Vec<u8>>,
        spec: &ProgramSpec,
    ) -> Result<Self::Loaded, Error>;
    /// Maps actually referenced by this loaded program.
    fn map_names(loaded: &Self::Loaded) -> &[String];
    /// Pin one owned program, retaining partial ownership on failure.
    fn pin_program(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: &Self::Prepared,
        loaded: &mut Self::Loaded,
        name: &Symbol,
    ) -> Acquisition<Self::ProgramPin>;
    /// Exclusively create a private map container.
    fn create_map_directory(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: &Self::Prepared,
        id: NonZeroU32,
    ) -> Acquisition<Self::MapDirectory>;
    /// Pin one referenced map beneath its owned container.
    fn pin_map(
        &self,
        writer: &RuntimeWriter<'_>,
        loaded: &Self::Loaded,
        directory: &Self::MapDirectory,
        name: &str,
    ) -> Acquisition<Self::MapPin>;
}

/// Standalone attachment ownership, shared by detach and program unload.
pub trait TracepointLinks {
    /// Adopted program and canonical pin collection.
    type PreparedTracepoint;
    /// Owned unpinned link; releasing it ends the attachment.
    type LiveTracepoint: Send + Sync + 'static;
    /// Non-cloneable persistent link-pin receipt.
    type LinkPin: Send + Sync + 'static;

    /// Adopt and validate the canonical managed program pin.
    fn prepare_tracepoint(
        &self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<Self::PreparedTracepoint, Error>;
    /// Attach once, returning ownership of the unpinned link.
    fn attach_tracepoint(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: Self::PreparedTracepoint,
        target: &Tracepoint,
    ) -> Result<Self::LiveTracepoint, Error>;
    /// Pin under a store-allocated ID. An unpinned failure must release the live link.
    fn pin_tracepoint(
        &self,
        writer: &RuntimeWriter<'_>,
        live: Self::LiveTracepoint,
        id: NonZeroU64,
    ) -> Acquisition<Self::LinkPin>;
    /// Stable kernel identity of the acquired pin.
    fn link_id(pin: &Self::LinkPin) -> NonZeroU32;
    /// Adopt a canonical existing pin only after identity validation.
    fn observe_link_pin(
        &self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU64,
        program: NonZeroU32,
        kernel: Option<NonZeroU32>,
    ) -> Result<Option<Self::LinkPin>, Error>;
    /// Release an unpinned attachment under the original runtime authority.
    fn release_tracepoint(
        &self,
        writer: &RuntimeWriter<'_>,
        live: Self::LiveTracepoint,
    ) -> Removal<Self::LiveTracepoint>;
    /// Remove the owned link pin.
    fn remove_link(&self, writer: &RuntimeWriter<'_>, pin: Self::LinkPin)
    -> Removal<Self::LinkPin>;
}

/// First-member XDP dispatcher acquisitions and synchronous last detach.
pub trait XdpLifecycle {
    /// Adopted extension and interface.
    type PreparedXdp;
    /// Loaded dispatcher with local lifetime ownership.
    type Dispatcher;
    /// Owned outer link, including an unpinned partial acquisition.
    type Outer: Send + Sync + 'static;
    /// Owned freplace link pin.
    type Extension: Send + Sync + 'static;
    /// Owned dispatcher program pin.
    type DispatcherPin: Send + Sync + 'static;
    /// Owned revision container.
    type Revision: Send + Sync + 'static;

    /// Resolve the selected-namespace interface and adopt the managed extension.
    fn prepare_xdp(
        &self,
        writer: &RuntimeWriter<'_>,
        program: NonZeroU32,
        interface: &InterfaceName,
        netns: &bpfman_model::NetworkNamespace,
    ) -> Result<(XdpKey, Self::PreparedXdp), Error>;
    /// Load the configured one-member dispatcher.
    fn load_dispatcher(&self, proceed_on: XdpProceedOn) -> Result<Self::Dispatcher, Error>;
    /// Exclusively create revision one.
    fn create_revision(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: &Self::PreparedXdp,
    ) -> Acquisition<Self::Revision>;
    /// Pin the loaded dispatcher in its owned revision.
    fn pin_dispatcher(
        &self,
        writer: &RuntimeWriter<'_>,
        revision: &Self::Revision,
        dispatcher: &mut Self::Dispatcher,
    ) -> Acquisition<Self::DispatcherPin>;
    /// Attach and pin slot zero's extension.
    fn pin_extension(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: &mut Self::PreparedXdp,
        revision: &Self::Revision,
        dispatcher: &Self::Dispatcher,
    ) -> Acquisition<Self::Extension>;
    /// Attach without replacing an existing program; retain live ownership if pinning fails.
    fn pin_outer(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: &Self::PreparedXdp,
        dispatcher: &Self::Dispatcher,
    ) -> Acquisition<Self::Outer>;
    /// Dispatcher identity for atomic store publication.
    fn dispatcher_id(pin: &Self::DispatcherPin) -> NonZeroU32;
    /// Extension identity for atomic store publication.
    fn extension_id(pin: &Self::Extension) -> NonZeroU32;
    /// Only a successfully pinned outer link may be committed.
    fn outer_id(pin: &Self::Outer) -> Result<NonZeroU32, Error>;
    /// Validate and adopt the entire committed snapshot before teardown.
    fn observe_xdp(
        &self,
        writer: &RuntimeWriter<'_>,
        snapshot: &XdpSnapshot,
    ) -> Result<XdpArtifacts<Self>, Error>;
    /// Synchronously stop traffic before unpinning; failure retains ownership.
    fn remove_outer(&self, writer: &RuntimeWriter<'_>, outer: Self::Outer) -> Removal<Self::Outer>;
    /// Remove the owned extension after traffic has stopped.
    fn remove_extension(
        &self,
        writer: &RuntimeWriter<'_>,
        extension: Self::Extension,
    ) -> Removal<Self::Extension>;
    /// Remove the owned dispatcher pin after traffic has stopped.
    fn remove_dispatcher(
        &self,
        writer: &RuntimeWriter<'_>,
        pin: Self::DispatcherPin,
    ) -> Removal<Self::DispatcherPin>;
    /// Remove the empty owned revision after its pins are gone.
    fn remove_revision(
        &self,
        writer: &RuntimeWriter<'_>,
        revision: Self::Revision,
    ) -> Removal<Self::Revision>;
}

/// Complete preflight ownership for XDP teardown.
pub struct XdpArtifacts<K: XdpLifecycle + ?Sized> {
    /// Outer attachment.
    pub outer: Option<K::Outer>,
    /// Member extension link.
    pub extension: Option<K::Extension>,
    /// Dispatcher program pin.
    pub program: Option<K::DispatcherPin>,
    /// Dependent revision directory.
    pub directory: Option<K::Revision>,
}
