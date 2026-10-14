//! Native TCX pins use confined namespace/interface/program identities.
use crate::artifacts::{identity, proc_path};
use crate::directory::{BENEATH, CONFINED, ensure_directory};
use crate::error::{Failure, io};
use crate::link::verify_bpffs;
use crate::observe::{observe, optional_dir};
use crate::removal::open_owned;
use crate::{
    Error, LinkPin, LiveTracepoint, PreparedTcxAttach, RuntimeWriter, TcxKernel, TcxProgram,
};
use std::num::NonZeroU32;

impl RuntimeWriter<'_> {
    /// Adopt the native program and retained namespace through a confined pin.
    pub fn prepare_tcx_attach<K: TcxKernel>(
        &self,
        kernel: &K,
        id: NonZeroU32,
        target: &bpfman_model::TcxLink,
    ) -> Result<PreparedTcxAttach<K::Program>, Error> {
        let bpffs = optional_dir(&self.runtime.root, "fs", BENEATH)?
            .ok_or(Failure::Unsafe("program bpffs missing"))?;
        verify_bpffs(&bpffs)?;
        let program_pin = observe(self, &bpffs, "fs", &format!("prog_{id}"), false)?
            .ok_or(Failure::Unsafe("program pin missing"))?;
        let (info, program) = kernel
            .tcx_at(
                crate::PinSource(&proc_path(&bpffs).join(&program_pin.name)),
                target,
            )
            .map_err(Failure::Kernel)?;
        if info.id != id.get() || info.kind != crate::PinProgramKind::Tc {
            return Err(Failure::Unsafe("TCX program identity or type changed").into());
        }
        open_owned(&program_pin)?;
        let links = ensure_directory(&bpffs, format!("tcx-{}", target.key.direction), CONFINED)?;
        Ok(PreparedTcxAttach {
            program,
            program_pin,
            links,
            target: target.clone(),
            program_id: id,
        })
    }

    /// Adopt only the exact native hook and stored kernel identity.
    pub fn observe_tcx_link_pin(
        &self,
        backend: &impl crate::LinkInspection,
        target: &bpfman_model::TcxLink,
        program: NonZeroU32,
        kernel: Option<NonZeroU32>,
    ) -> Result<Option<LinkPin>, Error> {
        let Some(bpffs) = optional_dir(&self.runtime.root, "fs", BENEATH)? else {
            return Ok(None);
        };
        verify_bpffs(&bpffs)?;
        let Some(links) = optional_dir(&bpffs, &format!("tcx-{}", target.key.direction), CONFINED)?
        else {
            return Ok(None);
        };
        let Some(entry) = observe(
            self,
            &links,
            &format!("fs/tcx-{}", target.key.direction),
            &format!(
                "link_{}_{}_{}",
                target.key.nsid, target.key.ifindex, program
            ),
            false,
        )?
        else {
            return Ok(None);
        };
        let info = backend
            .link_at(crate::PinSource(&proc_path(&links).join(&entry.name)))
            .map_err(Failure::Kernel)?;

        if info.program_id.get() != program.get()
            || !matches!(info.details, bpfman_model::KernelLinkDetails::Tcx { ifindex, direction } if direction == target.key.direction && (ifindex == target.key.ifindex.get() || ifindex == 0))
            || kernel.is_some_and(|id| id.get() != info.id.get())
        {
            return Err(Failure::Unsafe(
                "link pin has a different kernel identity, program, or type",
            )
            .into());
        }

        let id = NonZeroU32::new(info.id.get()).ok_or(Failure::Unsafe("zero kernel link ID"))?;
        open_owned(&entry)?;

        // Observations retain no live descriptor. Teardown adopts one again
        // under writer authority and holds it across removal of the owned pin.
        Ok(Some(LinkPin {
            entry: Box::new(entry),
            id,
            kind: crate::removal::LinkKind::Tcx(target.key),
        }))
    }
}

impl<P: TcxProgram> PreparedTcxAttach<P> {
    /// Attach with explicit ordering, returning the sole unpinned link owner.
    pub fn attach(
        mut self,
        writer: &RuntimeWriter<'_>,
        order: bpfman_model::TcxOrder,
    ) -> Result<LiveTracepoint<P::Link>, Error> {
        self.program_pin.check_writer(writer)?;
        open_owned(&self.program_pin)?;
        let link = self
            .program
            .attach(&self.target, order)
            .map_err(Failure::Kernel)?;
        let id = NonZeroU32::new(crate::LinkPinning::id(&link).map_err(Failure::Kernel)?)
            .ok_or(Failure::Unsafe("zero TCX link ID"))?;
        Ok(LiveTracepoint {
            root: identity(&writer.runtime.root)?,
            links: self.links,
            location: crate::link::Location::Tcx {
                key: self.target.key,
                program: self.program_id,
            },
            link,
            id,
        })
    }
}

impl crate::RuntimeDirectory {
    /// Read a canonical TCX pin without acquiring mutation authority.
    pub fn read_tcx_link_pin(
        &self,
        backend: &impl crate::LinkInspection,
        target: &bpfman_model::TcxLink,
        program: NonZeroU32,
    ) -> Result<Option<bpfman_model::KernelLink>, Error> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat2};
        let Some(bpffs) = optional_dir(&self.root, "fs", BENEATH)? else {
            return Ok(None);
        };
        verify_bpffs(&bpffs)?;
        let Some(links) = optional_dir(&bpffs, &format!("tcx-{}", target.key.direction), CONFINED)?
        else {
            return Ok(None);
        };
        let pin = match openat2(
            &links,
            format!(
                "link_{}_{}_{}",
                target.key.nsid, target.key.ifindex, program
            ),
            OFlags::PATH | OFlags::CLOEXEC,
            Mode::empty(),
            CONFINED,
        ) {
            Ok(pin) => pin,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(error) => return Err(io("open observed link pin", error)),
        };
        let stat = fstat(&pin).map_err(|e| io("inspect observed link pin", e))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink > 1 {
            return Err(Failure::Unsafe("link pin has unexpected type or hard links").into());
        }

        let info = backend
            .link_at(crate::PinSource(&proc_path(&pin)))
            .map_err(Failure::Kernel)?;
        if !matches!(info.details, bpfman_model::KernelLinkDetails::Tcx { ifindex, direction } if direction == target.key.direction && (ifindex == target.key.ifindex.get() || ifindex == 0))
        {
            return Err(Failure::Unsafe("link pin is not a perf-event link").into());
        }
        let id = NonZeroU32::new(info.id.get()).ok_or(Failure::Unsafe("zero kernel link ID"))?;
        let program_id =
            NonZeroU32::new(info.program_id.get()).ok_or(Failure::Unsafe("zero program ID"))?;

        Ok(Some(bpfman_model::KernelLink {
            details: info.details,
            id,
            program_id,
        }))
    }
}
