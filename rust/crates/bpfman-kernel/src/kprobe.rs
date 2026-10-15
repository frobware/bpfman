//! Kernel probe acquisition; pinning and teardown share standalone ownership.
use crate::{Error, TracepointLinks};
use bpfman_fs::RuntimeWriter;
use bpfman_model::KprobeLink;
use std::num::NonZeroU32;

/// Entry and return probes for already-loaded native probe programs.
pub trait KprobeLinks: TracepointLinks {
    /// Confined adopted probe program.
    type PreparedKprobe;
    /// Adopt the native program before publishing pending intent.
    fn prepare_kprobe(
        &self,
        writer: &RuntimeWriter<'_>,
        program: NonZeroU32,
        target: &KprobeLink,
    ) -> Result<Self::PreparedKprobe, Error>;
    /// Attach once without acquiring a persistent pin.
    fn attach_kprobe(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: Self::PreparedKprobe,
        target: &KprobeLink,
    ) -> Result<Self::LiveTracepoint, Error>;
}
