//! Confined load-bound tracing adoption and standalone link acquisition.
use crate::artifacts::proc_path;
use crate::directory::{BENEATH, CONFINED, ensure_directory};
use crate::error::Failure;
use crate::link::{Location, verify_bpffs};
use crate::observe::{observe, optional_dir};
use crate::removal::open_owned;
use crate::{
    Error, LinkPinning, LiveTracepoint, PreparedTracingAttach, RuntimeWriter, TracingProgram,
};
use std::num::NonZeroU32;

impl RuntimeWriter<'_> {
    /// Adopt a managed tracing program's canonical pin without loading or
    /// mounting anything. Reject mismatched IDs, types, symlinks and ancestors.
    pub fn prepare_tracing_attach<K: crate::TracingKernel>(
        &self,
        kernel: &K,
        id: NonZeroU32,
        target: &bpfman_model::TracingLink,
    ) -> Result<PreparedTracingAttach<K::Tracing>, Error> {
        let bpffs = optional_dir(&self.runtime.root, "fs", BENEATH)?
            .ok_or(Failure::Unsafe("program bpffs is missing"))?;
        verify_bpffs(&bpffs)?;
        let program_pin = observe(self, &bpffs, "fs", &format!("prog_{id}"), false)?
            .ok_or(Failure::Unsafe("program pin is missing"))?;
        let (info, program) = kernel
            .tracing_at(
                crate::PinSource(&proc_path(&bpffs).join(&program_pin.name)),
                target,
            )
            .map_err(Failure::Kernel)?;

        if info.id != id.get() || info.kind != crate::PinProgramKind::Tracing {
            return Err(
                Failure::Unsafe("program pin has a different kernel identity or type").into(),
            );
        }

        open_owned(&program_pin)?;
        let links = ensure_directory(&bpffs, "links", CONFINED)?;

        Ok(PreparedTracingAttach {
            program,
            program_pin,
            links,
            target: target.clone(),
        })
    }
}

impl<P: TracingProgram> PreparedTracingAttach<P> {
    /// Attach exactly once, returning ownership of the unpinned kernel link.
    /// Failure releases any locally acquired descriptors before returning.
    pub fn attach(
        mut self,
        writer: &RuntimeWriter<'_>,
        target: &bpfman_model::TracingLink,
    ) -> Result<LiveTracepoint<P::Link>, Error> {
        self.program_pin.check_writer(writer)?;
        open_owned(&self.program_pin)?;
        if *target != self.target {
            return Err(
                Failure::Unsafe("tracing function or selection changed since admission").into(),
            );
        }
        let link = self.program.attach().map_err(Failure::Kernel)?;
        let id = NonZeroU32::new(link.id().map_err(Failure::Kernel)?)
            .ok_or(Failure::Unsafe("zero kernel link ID"))?;

        Ok(LiveTracepoint {
            root: self.program_pin.root,
            links: self.links,
            location: Location::Tracing(target.kind),
            link,
            id,
        })
    }
}
