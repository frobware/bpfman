//! Native TCX acquisition; pin cleanup shares standalone ownership.
use crate::{Error, TracepointLinks};
use bpfman_fs::RuntimeWriter;
use bpfman_model::{InterfaceName, NetworkNamespace, TcDirection, TcxLink, TcxOrder};
use std::num::NonZeroU32;

/// Native classifier attachment with explicit kernel multi-program ordering.
pub trait TcxLinks: TracepointLinks {
    /// Confined program and retained namespace/interface evidence.
    type PreparedTcx;
    /// Resolve and adopt before publishing pending intent.
    fn prepare_tcx(
        &self,
        writer: &RuntimeWriter<'_>,
        program: NonZeroU32,
        interface: &InterfaceName,
        netns: &NetworkNamespace,
        direction: TcDirection,
        priority: u32,
    ) -> Result<(TcxLink, Self::PreparedTcx), Error>;
    /// Attach relative to a validated live anchor. Failure acquires no link.
    fn attach_tcx(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: Self::PreparedTcx,
        order: TcxOrder,
    ) -> Result<Self::LiveTracepoint, Error>;
}
