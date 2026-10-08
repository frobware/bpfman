//! Confined TC pins and durable clsact ownership; serialized paths are never authority.
use crate::{
    Error, ExtensionProgram, LinkPinning, ProgramPinning, RuntimeDirectory, RuntimeWriter,
    artifacts::{Entry, entry, proc_path},
    directory::{BENEATH, CONFINED, ensure_directory},
    error::{Failure, io},
    observe::{observe, optional_dir},
    removal::open_owned,
};
use bpfman_core::EffectFailure;
use bpfman_model::{TcDispatcherSnapshot, TcSnapshot, XdpKey};
use std::{
    fs::File,
    io::{Read, Write},
    num::NonZeroU32,
};

/// Adopted TC extension with the original confined program-pin identity.
pub struct TcProgram<E: ExtensionProgram> {
    entry: Entry,
    extension: E,
}

/// Owned staged pins and one-byte durable qdisc ownership evidence.
/// Each successful removal consumes its entry; retries retain only unresolved work.
pub struct TcPins {
    directory: Option<Box<Entry>>,
    program: Option<Box<Entry>>,
    extensions: Vec<Option<Box<Entry>>>,
    ownership: Option<Box<Entry>>,
    dispatcher_id: Option<NonZeroU32>,
    extension_ids: Vec<NonZeroU32>,
}

fn name(key: XdpKey, revision: NonZeroU32) -> String {
    format!("dispatcher_{}_{}_{}", key.nsid, key.ifindex, revision)
}

fn fail<T>(cause: Error) -> EffectFailure<Option<T>, Error> {
    EffectFailure {
        cause,
        remaining: None,
    }
}

fn nz(id: u32) -> Result<NonZeroU32, Error> {
    NonZeroU32::new(id).ok_or_else(|| Failure::Unsafe("zero TC kernel identity").into())
}

impl RuntimeWriter<'_> {
    /// Adopt a canonical managed EXT program beneath the retained bpffs root.
    pub fn prepare_tc_program<K: crate::TcKernel>(
        &self,
        kernel: &K,
        id: NonZeroU32,
    ) -> Result<TcProgram<K::Extension>, Error> {
        let bpffs = optional_dir(&self.runtime.root, "fs", BENEATH)?
            .ok_or(Failure::Unsafe("program bpffs is missing"))?;
        crate::link::verify_bpffs(&bpffs)?;
        let pin = observe(self, &bpffs, "fs", &format!("prog_{id}"), false)?
            .ok_or(Failure::Unsafe("program pin is missing"))?;
        let fd = open_owned(&pin)?;
        let (info, extension) = kernel
            .tc_extension_at(crate::PinSource(&proc_path(&fd)))
            .map_err(Failure::Kernel)?;
        if info.id != id.get() || info.kind != crate::PinProgramKind::Extension {
            return Err(Failure::Unsafe("expected managed TC extension").into());
        }
        Ok(TcProgram {
            entry: pin,
            extension,
        })
    }

    /// Validate every present pin and the ownership file before any detach effects.
    pub fn observe_tc_pins<K: crate::TcKernel>(
        &self,
        kernel: &K,
        snapshot: &TcSnapshot,
    ) -> Result<TcPins, Error> {
        let snapshot = TcDispatcherSnapshot::new(vec![snapshot.clone()])
            .map_err(|_| Failure::Unsafe("invalid TC snapshot"))?;
        self.observe_tc_dispatcher_pins(kernel, &snapshot)
    }

    /// Adopt every member of one complete revision before changing traffic.
    pub fn observe_tc_dispatcher_pins<K: crate::TcKernel>(
        &self,
        kernel: &K,
        snapshot: &TcDispatcherSnapshot,
    ) -> Result<TcPins, Error> {
        let first = snapshot
            .members()
            .first()
            .ok_or(Failure::Unsafe("empty TC snapshot"))?;
        let d = &first.details;
        for member in snapshot.members() {
            if self
                .layout()
                .tc_slot_path(d.key, d.revision, member.details.slot)
                .to_str()
                != Some(member.member.pin_path.as_str())
                || self
                    .layout()
                    .program_pin_path(member.member.program_id)
                    .to_str()
                    != Some(member.program_pin_path.as_str())
            {
                return Err(Failure::Unsafe("noncanonical TC snapshot paths").into());
            }
        }
        let ownership_dir = optional_dir(&self.runtime.root, "tc", CONFINED)?
            .ok_or(Failure::Unsafe("TC ownership collection missing"))?;
        let ownership = observe(self, &ownership_dir, "tc", &name(d.key, d.revision), false)?
            .ok_or(Failure::Unsafe("TC ownership evidence missing"))?;
        let bpffs = optional_dir(&self.runtime.root, "fs", BENEATH)?
            .ok_or(Failure::Unsafe("TC bpffs missing"))?;
        crate::link::verify_bpffs(&bpffs)?;
        let collection = optional_dir(&bpffs, "tc-ingress", CONFINED)?
            .ok_or(Failure::Unsafe("TC collection missing"))?;
        let directory = observe(
            self,
            &collection,
            "fs/tc-ingress",
            &name(d.key, d.revision),
            true,
        )?;
        let mut pins = TcPins {
            directory: directory.map(Box::new),
            program: None,
            extensions: Vec::new(),
            ownership: Some(Box::new(ownership)),
            dispatcher_id: Some(d.dispatcher_id),
            extension_ids: Vec::new(),
        };
        pins.clsact_owned(self)?;
        if let Some(directory) = &pins.directory {
            let fd = open_owned(directory)?;
            let expected: Vec<_> = snapshot
                .members()
                .iter()
                .map(|m| format!("link_{}", m.details.slot.index()))
                .collect();
            for child in
                std::fs::read_dir(proc_path(&fd)).map_err(|e| io("enumerate TC revision", e))?
            {
                let child = child.map_err(|e| io("read TC revision child", e))?;
                if child.file_name() != "dispatcher"
                    && !expected.iter().any(|n| child.file_name() == n.as_str())
                {
                    return Err(Failure::Unsafe("unknown TC revision child").into());
                }
            }
            let parent = format!("fs/tc-ingress/{}", name(d.key, d.revision));
            pins.program = observe(self, &fd, &parent, "dispatcher", false)?.map(Box::new);
            if let Some(pin) = &pins.program {
                let owned = open_owned(pin)?;
                let info = kernel
                    .program_at(crate::PinSource(&proc_path(&owned)))
                    .map_err(Failure::Kernel)?;
                if info.id != d.dispatcher_id.get() || info.kind != crate::PinProgramKind::Tc {
                    return Err(Failure::Unsafe("TC dispatcher pin identity changed").into());
                }
            }
            for (member, name) in snapshot.members().iter().zip(expected) {
                let pin = observe(self, &fd, &parent, &name, false)?.map(Box::new);
                if let Some(pin) = &pin {
                    let owned = open_owned(pin)?;
                    let info = kernel
                        .link_at(crate::PinSource(&proc_path(&owned)))
                        .map_err(Failure::Kernel)?;
                    let bpfman_model::LinkState::Attached { kernel_id } = member.member.state
                    else {
                        return Err(Failure::Unsafe("TC link is pending").into());
                    };
                    if info.id != kernel_id
                        || info.program_id != member.member.program_id
                        || !matches!(info.details, bpfman_model::KernelLinkDetails::Tracing { target_obj_id, .. } if target_obj_id == d.dispatcher_id.get())
                    {
                        return Err(Failure::Unsafe("TC extension link identity changed").into());
                    }
                    pins.extension_ids.push(kernel_id);
                }
                pins.extensions.push(pin);
            }
        }
        Ok(pins)
    }

    /// Remove only owned entries, in dependency order, retaining progress on failure.
    pub fn remove_tc_pins(&self, mut pins: TcPins) -> Result<(), EffectFailure<TcPins, Error>> {
        let result = (|| {
            let mut failures = Vec::new();
            for slot in &mut pins.extensions {
                if let Some(entry) = slot {
                    match crate::removal::remove(self, entry) {
                        Ok(()) => *slot = None,
                        Err(cause) => failures.push(cause),
                    }
                }
            }
            if !failures.is_empty() {
                return Err(Failure::Cleanup(failures).into());
            }
            for slot in [&mut pins.program, &mut pins.ownership, &mut pins.directory] {
                if let Some(entry) = slot {
                    crate::removal::remove(self, entry)?;
                }
                *slot = None;
            }
            Ok(())
        })();
        result.map_err(|cause| EffectFailure {
            cause,
            remaining: pins,
        })
    }
}

impl<E: ExtensionProgram> TcProgram<E> {
    /// Stage a singleton revision; no qdisc or filter is created here.
    pub fn stage(
        &mut self,
        w: &RuntimeWriter<'_>,
        key: XdpKey,
        dispatcher: &mut E::Dispatcher,
    ) -> Result<TcPins, EffectFailure<Option<TcPins>, Error>> {
        Self::stage_revision(
            core::slice::from_mut(self),
            w,
            key,
            NonZeroU32::MIN,
            dispatcher,
            false,
        )
    }

    /// Stage every member in a fresh revision, copying established qdisc ownership.
    pub fn stage_revision(
        programs: &mut [Self],
        w: &RuntimeWriter<'_>,
        key: XdpKey,
        revision: NonZeroU32,
        dispatcher: &mut E::Dispatcher,
        clsact_owned: bool,
    ) -> Result<TcPins, EffectFailure<Option<TcPins>, Error>> {
        if programs.is_empty() || programs.len() > 10 {
            return Err(fail(Failure::Unsafe("invalid TC member count").into()));
        }
        for program in programs.iter() {
            program.entry.check_writer(w).map_err(fail)?;
            open_owned(&program.entry).map_err(fail)?;
        }
        let bpffs = optional_dir(&w.runtime.root, "fs", BENEATH)
            .map_err(fail)?
            .ok_or_else(|| fail(Failure::Unsafe("program bpffs missing").into()))?;
        crate::link::verify_bpffs(&bpffs).map_err(fail)?;
        let collection = ensure_directory(&bpffs, "tc-ingress", CONFINED).map_err(fail)?;
        let ownership_dir = ensure_directory(&w.runtime.root, "tc", CONFINED).map_err(fail)?;
        let directory = entry(
            w,
            &collection,
            "fs/tc-ingress".into(),
            name(key, revision),
            true,
        )
        .map_err(fail)?;
        rustix::fs::mkdirat(&collection, &directory.name, rustix::fs::Mode::RWXU)
            .map_err(|e| fail(io("create TC revision", e)))?;
        let mut pins = TcPins {
            directory: Some(Box::new(directory)),
            program: None,
            extensions: Vec::new(),
            ownership: None,
            dispatcher_id: None,
            extension_ids: Vec::new(),
        };
        let result = (|| -> Result<(), Error> {
            let directory = pins
                .directory
                .as_mut()
                .ok_or(Failure::Unsafe("missing TC revision"))?;
            directory.observe()?;
            let fd = open_owned(directory)?;
            let parent = format!("fs/tc-ingress/{}", directory.name);
            let mut owner = entry(w, &ownership_dir, "tc".into(), name(key, revision), false)?;
            let owner_fd = rustix::fs::openat2(
                &ownership_dir,
                &owner.name,
                rustix::fs::OFlags::WRONLY
                    | rustix::fs::OFlags::CREATE
                    | rustix::fs::OFlags::EXCL
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
                CONFINED,
            )
            .map_err(|e| io("create TC ownership evidence", e))?;
            let observed = owner.observe();
            pins.ownership = Some(Box::new(owner));
            observed?;
            let mut file = File::from(owner_fd);
            file.write_all(&[u8::from(clsact_owned)])
                .map_err(|e| io("write TC ownership evidence", e))?;
            file.sync_all()
                .map_err(|e| io("sync TC ownership evidence", e))?;
            let mut program_pin = entry(w, &fd, parent.clone(), "dispatcher".into(), false)?;
            pins.dispatcher_id = Some(nz(dispatcher.id().map_err(Failure::Kernel)?)?);
            dispatcher
                .pin(crate::PinTarget(&proc_path(&fd).join("dispatcher")))
                .map_err(Failure::Kernel)?;
            let observed = program_pin.observe();
            pins.program = Some(Box::new(program_pin));
            observed?;
            for (slot, program) in programs.iter_mut().enumerate() {
                let name = format!("link_{slot}");
                let mut extension_pin = entry(w, &fd, parent.clone(), name.clone(), false)?;
                let slot = slot
                    .try_into()
                    .map_err(|_| Failure::Unsafe("invalid TC slot"))?;
                let link = program
                    .extension
                    .attach(dispatcher, slot)
                    .map_err(Failure::Kernel)?;
                pins.extension_ids
                    .push(nz(link.id().map_err(Failure::Kernel)?)?);
                link.pin(crate::PinTarget(&proc_path(&fd).join(&name)))
                    .map_err(Failure::Kernel)?;
                let observed = extension_pin.observe();
                pins.extensions.push(Some(Box::new(extension_pin)));
                observed?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => Ok(pins),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: Some(pins),
            }),
        }
    }
}

impl TcPins {
    /// Adopt a retained native dispatcher target through its verified pin entry.
    pub fn target<K: crate::TcKernel>(
        &self,
        w: &RuntimeWriter<'_>,
        kernel: &K,
    ) -> Result<K::Target, Error> {
        let entry = self
            .program
            .as_ref()
            .ok_or(Failure::Unsafe("TC dispatcher pin missing"))?;
        entry.check_writer(w)?;
        let fd = open_owned(entry)?;
        let (info, target) = kernel
            .tc_target_at(crate::PinSource(&proc_path(&fd)))
            .map_err(Failure::Kernel)?;
        if Some(nz(info.id)?) != self.dispatcher_id || info.kind != crate::PinProgramKind::Tc {
            return Err(Failure::Unsafe("TC target changed").into());
        }
        Ok(target)
    }

    /// Identities are available only after both persistent pins were acquired.
    pub fn ids(&self) -> Result<(NonZeroU32, NonZeroU32), Error> {
        self.dispatcher_id
            .zip(self.extension_ids.first().copied())
            .ok_or_else(|| Failure::Unsafe("incomplete TC pins").into())
    }

    /// Complete kernel identities after staging or adopting every member.
    pub fn revision_ids(&self) -> Result<(NonZeroU32, &[NonZeroU32]), Error> {
        let dispatcher = self
            .dispatcher_id
            .ok_or(Failure::Unsafe("incomplete TC dispatcher"))?;
        if self.program.is_none()
            || self.extensions.is_empty()
            || self.extensions.iter().any(Option::is_none)
            || self.extensions.len() != self.extension_ids.len()
        {
            return Err(Failure::Unsafe("incomplete TC revision pins").into());
        }
        Ok((dispatcher, &self.extension_ids))
    }

    /// Persist ownership before publication. A failed write retains live kernel cleanup.
    pub fn record_clsact(&self, w: &RuntimeWriter<'_>, owned: bool) -> Result<(), Error> {
        let entry = self
            .ownership
            .as_ref()
            .ok_or(Failure::Unsafe("missing TC ownership evidence"))?;
        entry.check_writer(w)?;
        open_owned(entry)?;
        let fd = rustix::fs::openat2(
            &entry.parent,
            &entry.name,
            rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
            CONFINED,
        )
        .map_err(|e| io("open TC ownership evidence for write", e))?;
        if crate::artifacts::identity(&fd)?
            != entry
                .identity
                .ok_or(Failure::Unsafe("TC ownership identity unavailable"))?
        {
            return Err(Failure::Unsafe("TC ownership file changed").into());
        }
        let mut file = File::from(fd);
        file.write_all(&[u8::from(owned)])
            .map_err(|e| io("write TC clsact ownership", e))?;
        file.sync_all()
            .map_err(|e| io("sync TC clsact ownership", e))
    }

    /// Read and validate durable ownership independently of operator metadata.
    pub fn clsact_owned(&self, w: &RuntimeWriter<'_>) -> Result<bool, Error> {
        let entry = self
            .ownership
            .as_ref()
            .ok_or(Failure::Unsafe("missing TC ownership evidence"))?;
        entry.check_writer(w)?;
        open_owned(entry)?;
        let fd = rustix::fs::openat2(
            &entry.parent,
            &entry.name,
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
            CONFINED,
        )
        .map_err(|e| io("open TC ownership evidence", e))?;
        if crate::artifacts::identity(&fd)?
            != entry
                .identity
                .ok_or(Failure::Unsafe("TC ownership identity unavailable"))?
        {
            return Err(Failure::Unsafe("TC ownership file changed").into());
        }
        let mut bytes = Vec::new();
        File::from(fd)
            .take(2)
            .read_to_end(&mut bytes)
            .map_err(|e| io("read TC ownership evidence", e))?;
        match bytes.as_slice() {
            [0] => Ok(false),
            [1] => Ok(true),
            _ => Err(Failure::Unsafe("invalid TC ownership evidence").into()),
        }
    }
}

impl RuntimeDirectory {
    /// Read the canonical TC freplace pin without writer authority.
    pub fn read_tc_link_pin(
        &self,
        kernel: &impl crate::LinkInspection,
        details: &bpfman_model::TcLink,
    ) -> Result<Option<bpfman_model::KernelLink>, Error> {
        let Some(bpffs) = optional_dir(&self.root, "fs", BENEATH)? else {
            return Ok(None);
        };
        crate::link::verify_bpffs(&bpffs)?;
        let Some(collection) = optional_dir(&bpffs, "tc-ingress", CONFINED)? else {
            return Ok(None);
        };
        let Some(revision) =
            optional_dir(&collection, &name(details.key, details.revision), CONFINED)?
        else {
            return Ok(None);
        };
        let fd = match rustix::fs::openat2(
            &revision,
            format!("link_{}", details.slot.index()),
            rustix::fs::OFlags::PATH | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
            CONFINED,
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(io("open TC extension pin", e)),
        };
        let stat = rustix::fs::fstat(&fd).map_err(|e| io("inspect TC extension pin", e))?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile
            || stat.st_nlink > 1
        {
            return Err(Failure::Unsafe("invalid TC extension pin inode").into());
        }
        let info = kernel
            .link_at(crate::PinSource(&proc_path(&fd)))
            .map_err(Failure::Kernel)?;
        if !matches!(info.details, bpfman_model::KernelLinkDetails::Tracing { target_obj_id, .. } if target_obj_id == details.dispatcher_id.get())
        {
            return Err(Failure::Unsafe("unexpected TC extension target").into());
        }
        Ok(Some(info))
    }
}
