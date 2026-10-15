//! Confined user-space probe adoption and standalone link acquisition.
use crate::artifacts::proc_path;
use crate::directory::{BENEATH, CONFINED, ensure_directory};
use crate::error::Failure;
use crate::link::{Location, verify_bpffs};
use crate::observe::{observe, optional_dir};
use crate::removal::open_owned;
use crate::{
    Error, LinkPinning, LiveTracepoint, PreparedUprobeAttach, RuntimeWriter, UprobeProgram,
};
use std::num::NonZeroU32;

impl RuntimeWriter<'_> {
    /// Adopt a managed uprobe's canonical program pin without loading or
    /// mounting anything. Reject mismatched IDs, types, symlinks and ancestors.
    pub fn prepare_uprobe_attach<K: crate::UprobeKernel>(
        &self,
        kernel: &K,
        id: NonZeroU32,
        retprobe: bool,
    ) -> Result<PreparedUprobeAttach<K::Uprobe>, Error> {
        let bpffs = optional_dir(&self.runtime.root, "fs", BENEATH)?
            .ok_or(Failure::Unsafe("program bpffs is missing"))?;
        verify_bpffs(&bpffs)?;
        let program_pin = observe(self, &bpffs, "fs", &format!("prog_{id}"), false)?
            .ok_or(Failure::Unsafe("program pin is missing"))?;
        let (info, program) = kernel
            .uprobe_at(
                crate::PinSource(&proc_path(&bpffs).join(&program_pin.name)),
                retprobe,
            )
            .map_err(Failure::Kernel)?;

        if info.id != id.get() || info.kind != crate::PinProgramKind::Kprobe {
            return Err(
                Failure::Unsafe("program pin has a different kernel identity or type").into(),
            );
        }

        open_owned(&program_pin)?;
        let links = ensure_directory(&bpffs, "links", CONFINED)?;

        Ok(PreparedUprobeAttach {
            program,
            program_pin,
            links,
            retprobe,
        })
    }
}

impl<P: UprobeProgram> PreparedUprobeAttach<P> {
    /// Attach exactly once, returning ownership of the unpinned kernel link.
    /// Failure releases any locally acquired descriptors before returning.
    pub fn attach(
        mut self,
        writer: &RuntimeWriter<'_>,
        target: &bpfman_model::UprobeLink,
        file: &std::fs::File,
    ) -> Result<LiveTracepoint<P::Link>, Error> {
        self.program_pin.check_writer(writer)?;
        open_owned(&self.program_pin)?;
        if target.retprobe() != self.retprobe {
            return Err(
                Failure::Unsafe("probe entry/return selection changed since admission").into(),
            );
        }
        let link = self.program.attach(target, file).map_err(Failure::Kernel)?;
        let id = NonZeroU32::new(link.id().map_err(Failure::Kernel)?)
            .ok_or(Failure::Unsafe("zero kernel link ID"))?;

        Ok(LiveTracepoint {
            root: self.program_pin.root,
            links: self.links,
            location: Location::Tracepoint,
            link,
            id,
        })
    }
}

impl RuntimeWriter<'_> {
    /// Duplicate this writer's held lock for an explicitly launched helper.
    /// Closing this descriptor releases its reference, never explicitly unlocks.
    pub fn duplicate_lock_fd(&self) -> Result<std::os::fd::OwnedFd, Error> {
        self._permit
            .duplicate_fd()
            .map_err(|e| Failure::Lock(e).into())
    }
}
