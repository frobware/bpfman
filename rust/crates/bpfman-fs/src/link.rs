//! Standalone attachments use adopted program and bpffs descriptors. Persisted
//! paths never become syscall targets, and removal remains in `removal`.

use crate::{
    Error, LinkPin, LiveTracepoint, PreparedTracepointAttach, RuntimeWriter,
    artifacts::{BPF_SUPER_MAGIC, entry, identity, proc_path},
    directory::{BENEATH, CONFINED, ensure_directory},
    error::{Failure, io},
    observe::{observe, optional_dir},
    removal::open_owned,
};
use aya::programs::{
    ProgramInfo, ProgramType, TracePoint,
    links::{FdLink, LinkType, PinnedLink},
};
use bpfman_core::EffectFailure;
use bpfman_model::Tracepoint;
use std::num::{NonZeroU32, NonZeroU64};

impl RuntimeWriter<'_> {
    /// Adopt a managed tracepoint's canonical program pin without loading or
    /// mounting anything. Reject mismatched IDs, types, symlinks and ancestors.
    pub fn prepare_tracepoint_attach(
        &self,
        id: NonZeroU32,
    ) -> Result<PreparedTracepointAttach, Error> {
        let bpffs = optional_dir(&self.runtime.root, "fs", BENEATH)?
            .ok_or(Failure::Unsafe("program bpffs is missing"))?;
        verify_bpffs(&bpffs)?;
        let program_pin = observe(self, &bpffs, "fs", &format!("prog_{id}"), false)?
            .ok_or(Failure::Unsafe("program pin is missing"))?;
        let info = ProgramInfo::from_pin(proc_path(&bpffs).join(&program_pin.name))
            .map_err(Failure::Program)?;

        if info.id() != id.get() || info.program_type() != ProgramType::TracePoint.into() {
            return Err(
                Failure::Unsafe("program pin has a different kernel identity or type").into(),
            );
        }

        let program = TracePoint::from_program_info(info, "managed_tracepoint".into())
            .map_err(Failure::Program)?;
        open_owned(&program_pin)?;
        let links = ensure_directory(&bpffs, "links", CONFINED)?;

        Ok(PreparedTracepointAttach {
            program,
            program_pin,
            links,
        })
    }

    /// Observe the canonical link pin for pending or finalised stored intent.
    /// Missing pins are already absent; malformed or mismatched pins are errors.
    /// The caller must validate stored ownership under this same writer scope.
    pub fn observe_link_pin(
        &self,
        id: NonZeroU64,
        program: NonZeroU32,
        kernel: Option<NonZeroU32>,
    ) -> Result<Option<LinkPin>, Error> {
        let Some(bpffs) = optional_dir(&self.runtime.root, "fs", BENEATH)? else {
            return Ok(None);
        };
        verify_bpffs(&bpffs)?;
        let Some(links) = optional_dir(&bpffs, "links", CONFINED)? else {
            return Ok(None);
        };
        let Some(entry) = observe(self, &links, "fs/links", &id.to_string(), false)? else {
            return Ok(None);
        };
        let link: FdLink = PinnedLink::from_pin(proc_path(&links).join(&entry.name))
            .map_err(Failure::Link)?
            .into();
        let info = link.info().map_err(Failure::Link)?;

        if info.program_id() != program.get()
            || info.link_type().map_err(Failure::Link)? != LinkType::PerfEvent
            || kernel.is_some_and(|id| id.get() != info.id())
        {
            return Err(Failure::Unsafe(
                "link pin has a different kernel identity, program, or type",
            )
            .into());
        }

        let id = NonZeroU32::new(info.id()).ok_or(Failure::Unsafe("zero kernel link ID"))?;
        open_owned(&entry)?;

        // Closing this observation descriptor must happen before returning the
        // receipt. Unpin can then release our last managed attachment reference.
        Ok(Some(LinkPin {
            entry: Box::new(entry),
            id,
        }))
    }

    /// Release an unpinned attachment acquired in this runtime. Closing its owned
    /// descriptor is infallible at this boundary; there is no persistent pin.
    pub fn release_tracepoint(
        &self,
        live: LiveTracepoint,
    ) -> Result<(), EffectFailure<LiveTracepoint, Error>> {
        let result = identity(&self.runtime.root).and_then(|root| {
            if root != live.root {
                return Err(Failure::Unsafe("attachment belongs to another runtime").into());
            }
            Ok(())
        });

        result.map_err(|cause| EffectFailure {
            cause,
            remaining: live,
        })
    }
}

fn verify_bpffs(fd: &std::os::fd::OwnedFd) -> Result<(), Error> {
    if rustix::fs::fstatfs(fd)
        .map_err(|e| io("verify attachment bpffs", e))?
        .f_type
        != BPF_SUPER_MAGIC
    {
        return Err(Failure::Unsafe("existing fs is not bpffs").into());
    }

    Ok(())
}

impl PreparedTracepointAttach {
    /// Attach exactly once, returning ownership of the unpinned kernel link.
    /// Failure releases any locally acquired descriptors before returning.
    pub fn attach(
        mut self,
        writer: &RuntimeWriter<'_>,
        target: &Tracepoint,
    ) -> Result<LiveTracepoint, Error> {
        self.program_pin.check_writer(writer)?;
        open_owned(&self.program_pin)?;
        let id = self
            .program
            .attach(target.group(), target.name())
            .map_err(Failure::Program)?;
        let link: FdLink = self
            .program
            .take_link(id)
            .map_err(Failure::Program)?
            .try_into()
            .map_err(Failure::Link)?;
        let id = NonZeroU32::new(link.info().map_err(Failure::Link)?.id())
            .ok_or(Failure::Unsafe("zero kernel link ID"))?;

        Ok(LiveTracepoint {
            root: self.program_pin.root,
            links: self.links,
            link,
            id,
        })
    }
}

impl LiveTracepoint {
    /// Pin this attachment at a store-allocated managed identity. Before pinning,
    /// failure closes the live handle. After pinning, failure retains pin ownership.
    pub fn pin(
        self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU64,
    ) -> Result<LinkPin, EffectFailure<Option<LinkPin>, Error>> {
        let fail = |cause| EffectFailure {
            cause,
            remaining: None,
        };
        if identity(&writer.runtime.root).map_err(fail)? != self.root {
            return Err(fail(
                Failure::Unsafe("attachment belongs to another runtime").into(),
            ));
        }

        let entry = entry(
            writer,
            &self.links,
            "fs/links".into(),
            id.to_string(),
            false,
        )
        .map_err(fail)?;
        entry.check_writer(writer).map_err(fail)?;
        let pinned = self
            .link
            .pin(proc_path(&self.links).join(&entry.name))
            .map_err(|e| fail(Failure::Pin(e).into()))?;
        let mut receipt = LinkPin {
            entry: Box::new(entry),
            id: self.id,
        };
        drop(pinned);

        match receipt.entry.observe() {
            Ok(()) => Ok(receipt),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: Some(receipt),
            }),
        }
    }
}

impl LinkPin {
    /// Captured kernel identity, distinct from the managed link handle.
    pub fn kernel_id(&self) -> NonZeroU32 {
        self.id
    }
}

impl crate::RuntimeDirectory {
    /// Read a canonical standalone link pin without writer authority. Traverse
    /// beneath opened descriptors and inspect the opened inode, never a stored path.
    pub fn read_link_pin(&self, id: NonZeroU64) -> Result<Option<bpfman_model::KernelLink>, Error> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat2};
        let Some(bpffs) = optional_dir(&self.root, "fs", BENEATH)? else {
            return Ok(None);
        };
        verify_bpffs(&bpffs)?;
        let Some(links) = optional_dir(&bpffs, "links", CONFINED)? else {
            return Ok(None);
        };
        let pin = match openat2(
            &links,
            id.to_string(),
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

        let link: FdLink = PinnedLink::from_pin(proc_path(&pin))
            .map_err(Failure::Link)?
            .into();
        let info = link.info().map_err(Failure::Link)?;
        if info.link_type().map_err(Failure::Link)? != LinkType::PerfEvent {
            return Err(Failure::Unsafe("link pin is not a perf-event link").into());
        }
        let id = NonZeroU32::new(info.id()).ok_or(Failure::Unsafe("zero kernel link ID"))?;
        let program_id =
            NonZeroU32::new(info.program_id()).ok_or(Failure::Unsafe("zero program ID"))?;

        Ok(Some(bpfman_model::KernelLink { id, program_id }))
    }
}
