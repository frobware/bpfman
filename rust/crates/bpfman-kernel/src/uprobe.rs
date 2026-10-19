//! Host user-space probe acquisition; pinning and teardown share standalone ownership.
use crate::{Error, TracepointLinks};
use bpfman_fs::RuntimeWriter;
use bpfman_model::UprobeLink;
use std::num::NonZeroU32;

/// Entry and return probes for already-loaded native probe programs.
pub trait UprobeLinks: TracepointLinks {
    /// Confined adopted probe program.
    type PreparedUprobe;
    /// Adopt the native program before publishing pending intent.
    fn prepare_uprobe(
        &self,
        writer: &RuntimeWriter<'_>,
        program: NonZeroU32,
        target: &UprobeLink,
    ) -> Result<Self::PreparedUprobe, Error>;
    /// Adopt with caller-owned cancellation for a potentially blocking target lookup.
    fn prepare_uprobe_with_cancellation(
        &self,
        writer: &RuntimeWriter<'_>,
        program: NonZeroU32,
        target: &UprobeLink,
        _cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<Self::PreparedUprobe, Error> {
        self.prepare_uprobe(writer, program, target)
    }
    /// Attach once without acquiring a persistent pin.
    fn attach_uprobe(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: Self::PreparedUprobe,
        target: &UprobeLink,
    ) -> Result<Self::LiveTracepoint, Error>;
}
