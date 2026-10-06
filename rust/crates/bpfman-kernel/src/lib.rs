//! Backend-independent kernel observation and lifecycle capabilities.
//!
//! Reads borrow one backend supplied at application construction. Implementations
//! own resource reuse and synchronization; observations confer no removal authority.

mod error;
mod lifecycle;
pub use lifecycle::{
    Acquisition, ObjectInfo, ObjectLoader, ProgramLoad, ProgramResources, Removal, TracepointLinks,
    UnloadArtifacts, XdpArtifacts, XdpLifecycle,
};

use bpfman_fs::{ObservedMapPin, RuntimeDirectory};
use bpfman_model::{KernelLink, KernelMap, KernelProgram, ProgramStats, XdpLink};
use std::num::{NonZeroU32, NonZeroU64};

/// Program and map observations, independent of the kernel implementation.
pub trait ProgramObservations {
    /// Read a live program; distinguish missing objects from failed inspection.
    fn program(&self, id: NonZeroU32) -> Result<(KernelProgram, Option<ProgramStats>), Error>;

    /// Read a live map without manufacturing inaccessible attributes.
    fn map(&self, id: u32) -> Result<KernelMap, Error>;

    /// Correlate managed map pins beneath this runtime's adopted root.
    /// Concrete adapters delegate confinement and traversal to bpfman-fs.
    fn map_pins(
        &self,
        runtime: &RuntimeDirectory,
        map_set: NonZeroU32,
    ) -> Result<Vec<ObservedMapPin>, Error>;
}

/// Standalone and dispatcher-member link observations.
pub trait LinkObservations {
    /// Inspect a perf-event link by kernel identity.
    fn tracepoint_link(&self, id: NonZeroU32) -> Result<KernelLink, Error>;

    /// Inspect an extension link, including its actual dispatcher target.
    fn extension_link(&self, id: NonZeroU32) -> Result<KernelLink, Error>;

    /// Read a canonical standalone pin without granting removal authority.
    fn tracepoint_pin(
        &self,
        runtime: &RuntimeDirectory,
        id: NonZeroU64,
    ) -> Result<Option<KernelLink>, Error>;

    /// Read a canonical dispatcher-member pin beneath the adopted runtime.
    fn extension_pin(
        &self,
        runtime: &RuntimeDirectory,
        link: &XdpLink,
    ) -> Result<Option<KernelLink>, Error>;
}

/// Portable classification of a kernel operation failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    /// Invalid input before kernel acquisition.
    InvalidInput,
    /// Capability outside the implemented slice.
    Unsupported,
    /// The requested kernel ID no longer exists.
    Missing,
    /// Observation was denied or failed at an OS boundary.
    Unavailable,
    /// Inconsistent data or unsafe pin traversal.
    InvalidData,
}

/// Classified failure retaining backend diagnostics only through its source.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct Error {
    cause: Box<error::Cause>,
}
