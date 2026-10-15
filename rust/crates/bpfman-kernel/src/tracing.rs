//! Function tracing acquisition; pinning and teardown share standalone ownership.
use crate::{Error, TracepointLinks};
use bpfman_fs::RuntimeWriter;
use bpfman_model::TracingLink;
use std::num::NonZeroU32;

/// Load-bound function tracing for already-loaded native tracing programs.
pub trait TracingLinks: TracepointLinks {
    /// Confined adopted tracing program.
    type PreparedTracing;
    /// Adopt the native program before publishing pending intent.
    fn prepare_tracing(
        &self,
        writer: &RuntimeWriter<'_>,
        program: NonZeroU32,
        target: &TracingLink,
    ) -> Result<Self::PreparedTracing, Error>;
    /// Attach once without acquiring a persistent pin.
    fn attach_tracing(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: Self::PreparedTracing,
        target: &TracingLink,
    ) -> Result<Self::LiveTracepoint, Error>;
}
