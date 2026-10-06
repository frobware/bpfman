//! Kernel pin bridge. Only this crate can construct a confined syscall target.
use bpfman_model::{InterfaceName, KernelLink, Tracepoint, XdpKey};
use std::{num::NonZeroU32, path::Path};

/// Diagnostic cause from the kernel implementation, never filesystem authority.
pub type KernelResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// A descriptor-relative pin destination, borrowed for one authorized syscall.
/// It cannot be constructed from a caller-supplied path.
/// ```compile_fail
/// let target = bpfman_fs::PinTarget(std::path::Path::new("/tmp/pin"));
/// ```
pub struct PinTarget<'a>(pub(crate) &'a Path);

/// An opened or descriptor-relative pin source validated by this filesystem adapter.
pub struct PinSource<'a>(pub(crate) &'a Path);

/// Portable program types needed to validate managed pins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinProgramKind {
    /// Standalone tracepoint.
    Tracepoint,
    /// Dispatcher extension.
    Extension,
    /// XDP dispatcher.
    Xdp,
    /// A foreign or unsupported program type.
    Other,
}

/// Kernel identity of a pinned program; carries no deletion authority.
pub struct PinnedProgram {
    /// Kernel program ID.
    pub id: u32,
    /// Observed program type.
    pub kind: PinProgramKind,
    /// Referenced maps, or unavailable evidence.
    pub map_ids: Option<Vec<u32>>,
}

/// Kernel identity of an outer XDP link; implementations reject other link types.
pub struct OuterInfo {
    /// Kernel link ID.
    pub id: u32,
    /// Dispatcher ID.
    pub program: u32,
    /// Attached interface, or zero after synchronous detach.
    pub ifindex: u32,
}

/// Program handle operations at a filesystem-authorized pin target.
pub trait ProgramPinning {
    /// Stable identity of the loaded program.
    fn id(&self) -> KernelResult<u32>;
    /// Pin without replacing an existing entry. Err means no pin was created.
    fn pin(&mut self, target: PinTarget<'_>) -> KernelResult<()>;
}

/// Map handle operations at a filesystem-authorized pin target.
pub trait MapPinning {
    /// Pin without replacing an existing entry. Err means no pin was created.
    fn pin(&self, target: PinTarget<'_>) -> KernelResult<()>;
}

/// Owned live link; dropping an unpinned handle releases the attachment.
pub trait LinkPinning: std::fmt::Debug + Send + Sync + 'static {
    /// Stable link identity.
    fn id(&self) -> KernelResult<u32>;
    /// Consume the live handle. Success leaves a persistent pin; failure releases it.
    fn pin(self, target: PinTarget<'_>) -> KernelResult<()>;
}

/// Adopted tracepoint whose attachment lifetime remains in the kernel adapter.
pub trait TracepointProgram {
    /// Owned unpinned attachment.
    type Link: LinkPinning;
    /// Attach once without acquiring a persistent pin.
    fn attach(&mut self, target: &Tracepoint) -> KernelResult<Self::Link>;
}

/// Adopted extension that can attach to a loaded dispatcher.
pub trait ExtensionProgram {
    /// Concrete opaque dispatcher handle.
    type Dispatcher: ProgramPinning;
    /// Owned unpinned extension link.
    type Link: LinkPinning;
    /// Attach to a validated slot; local failures release temporary acquisitions.
    fn attach(
        &mut self,
        dispatcher: &Self::Dispatcher,
        slot: bpfman_model::XdpSlot,
    ) -> KernelResult<Self::Link>;
}

/// An owned outer-link descriptor, retained across failed pin acquisition.
pub trait OuterLink: Send + Sync + 'static {
    /// Observe identity and attachment state while holding the descriptor.
    fn info(&self) -> KernelResult<OuterInfo>;
    /// Synchronously detach even if other descriptors keep the object alive.
    fn detach(&self) -> KernelResult<()>;
    /// Pin without consuming this handle; failure leaves the live handle owned.
    fn pin(&self, target: PinTarget<'_>) -> KernelResult<()>;
}

/// Read program/map identities using only filesystem-provided pin sources.
pub trait ProgramInspection {
    /// Read a pinned program's identity and referenced maps.
    fn program_at(&self, source: PinSource<'_>) -> KernelResult<PinnedProgram>;
    /// Read a pinned map identity.
    fn map_at(&self, source: PinSource<'_>) -> KernelResult<u32>;
}

/// Inspect a perf-event or extension link through an opened pin.
pub trait LinkInspection {
    /// Read portable link identity and target details.
    fn link_at(&self, source: PinSource<'_>) -> KernelResult<KernelLink>;
}

/// Adopt a tracepoint while retaining its live program handle.
pub trait TracepointKernel: LinkInspection {
    /// Opaque owned program.
    type Tracepoint: TracepointProgram;
    /// Open identity and program ownership together.
    fn tracepoint_at(
        &self,
        source: PinSource<'_>,
    ) -> KernelResult<(PinnedProgram, Self::Tracepoint)>;
}

/// XDP kernel mechanisms, distinct from filesystem pin ownership.
pub trait XdpKernel: ProgramInspection + LinkInspection {
    /// Opaque owned extension.
    type Extension: ExtensionProgram;
    /// Opaque owned outer link.
    type Outer: OuterLink;
    /// Resolve the current network namespace and requested interface.
    fn interface(&self, interface: &InterfaceName) -> KernelResult<XdpKey>;
    /// Open extension identity and handle together.
    fn extension_at(&self, source: PinSource<'_>)
    -> KernelResult<(PinnedProgram, Self::Extension)>;
    /// Open an existing outer link; reject other link types.
    fn outer_at(&self, source: PinSource<'_>) -> KernelResult<Self::Outer>;
    /// Inspect absence without granting filesystem removal authority.
    fn outer_by_id(&self, id: NonZeroU32) -> KernelResult<Option<Self::Outer>>;
    /// Attach in driver mode without replacing an existing interface attachment.
    fn attach_outer(
        &self,
        dispatcher: &<Self::Extension as ExtensionProgram>::Dispatcher,
        key: XdpKey,
    ) -> KernelResult<Self::Outer>;
}

/// Conditional live-link updates using retained kernel program descriptors.
pub trait XdpSwitchKernel: XdpKernel {
    /// Opaque program handle retaining a target across failed restoration.
    type Target: Send + Sync + 'static;
    /// Adopt an XDP target through a filesystem-confined pin source.
    fn target_at(&self, source: PinSource<'_>) -> KernelResult<(PinnedProgram, Self::Target)>;
    /// Atomically replace only the expected target. Err guarantees no mutation.
    fn replace_outer(
        &self,
        outer: &Self::Outer,
        expected: &Self::Target,
        new: &Self::Target,
    ) -> KernelResult<()>;
}

impl PinTarget<'_> {
    /// Descriptor-relative pathname valid only for the borrowed syscall scope.
    pub fn path(&self) -> &Path {
        self.0
    }
}

impl PinSource<'_> {
    /// Descriptor-relative pathname valid only for this observation scope.
    pub fn path(&self) -> &Path {
        self.0
    }
}
