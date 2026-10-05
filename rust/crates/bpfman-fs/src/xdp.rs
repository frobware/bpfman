//! XDP acquisitions and adoption stay beneath verified runtime descriptors.

use crate::{
    Error, RuntimeDirectory, RuntimeWriter,
    artifacts::{Entry, Identity, entry, identity, proc_path},
    directory::{BENEATH, CONFINED, ensure_directory},
    error::{Failure, io},
    observe::{observe, optional_dir},
    removal::open_owned,
};
use aya::programs::{Extension, ProgramInfo, ProgramType, Xdp, links::FdLink};
use bpfman_core::EffectFailure;
use bpfman_model::{InterfaceName, XdpKey, XdpLink, XdpSnapshot};
use std::{
    num::{NonZeroU32, NonZeroU64},
    os::{
        fd::{AsFd, OwnedFd},
        unix::fs::MetadataExt,
    },
};
mod syscall;

/// Adopted extension and attach point; no managed attachment has been created.
pub struct PreparedXdp {
    extension: Extension,
    extension_entry: Entry,
    collection: OwnedFd,
    key: XdpKey,
}

/// Owned revision directory; never a recursive removal capability.
pub struct XdpRevision {
    entry: Entry,
}

/// Owned dispatcher program pin.
pub struct XdpProgramPin {
    entry: Entry,
    id: NonZeroU32,
}

/// Owned extension link pin.
pub struct XdpExtensionPin {
    entry: Entry,
    id: NonZeroU32,
}

/// Outer link ownership. A live fd survives failed pinning; pinned evidence does
/// not retain a link fd, so observations cannot prolong a detached attachment.
pub struct XdpOuter {
    root: Identity,
    state: OuterState,
}

enum OuterState {
    Live(OwnedFd),
    Pinned { entry: Box<Entry>, id: NonZeroU32 },
}

/// Complete preflight observations for last-detach; missing artifacts allow retry.
pub struct XdpArtifacts {
    /// Outer interface link.
    pub outer: Option<XdpOuter>,
    /// Extension link.
    pub extension: Option<XdpExtensionPin>,
    /// Dispatcher program pin.
    pub program: Option<XdpProgramPin>,
    /// Revision directory.
    pub directory: Option<XdpRevision>,
}

fn nz(value: u32) -> Result<NonZeroU32, Error> {
    NonZeroU32::new(value).ok_or_else(|| Failure::Unsafe("zero kernel identity").into())
}

fn revision_name(key: XdpKey, revision: NonZeroU32) -> String {
    format!("dispatcher_{}_{}_{}", key.nsid, key.ifindex, revision)
}

fn outer_name(key: XdpKey) -> String {
    format!("dispatcher_{}_{}_link", key.nsid, key.ifindex)
}

fn fail<R>(cause: Error) -> EffectFailure<Option<R>, Error> {
    EffectFailure {
        cause,
        remaining: None,
    }
}

// A missing pin does not prove the interface link is detached: another process
// may still hold its descriptor. Refuse teardown until that live link is restored
// to its owned pin or detached. Never adopt removal authority from an ID alone.
fn require_detached(snapshot: &XdpSnapshot) -> Result<(), Error> {
    let fd = match syscall::by_id(snapshot.outer_link_id.get()) {
        Ok(fd) => fd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io("inspect missing outer XDP pin", e)),
    };
    let info = syscall::info(fd.as_fd()).map_err(|e| io("inspect unpinned XDP link", e))?;
    if info.kind != 6
        || info.id != snapshot.outer_link_id.get()
        || info.program != snapshot.details.dispatcher_id.get()
        || info.data[0] != 0
    {
        return Err(
            Failure::Unsafe("outer pin is missing but its link is not proven detached").into(),
        );
    }

    Ok(())
}

impl RuntimeWriter<'_> {
    /// Resolve an interface in this process's network namespace and adopt a
    /// canonical EXT program pin. Explicit namespace switching is not performed.
    pub fn prepare_xdp(
        &self,
        program: NonZeroU32,
        interface: &InterfaceName,
    ) -> Result<PreparedXdp, Error> {
        let nsid = NonZeroU64::new(
            std::fs::metadata("/proc/self/ns/net")
                .map_err(|e| io("inspect network namespace", e))?
                .ino(),
        )
        .ok_or(Failure::Unsafe("zero namespace inode"))?;
        let ifindex =
            nz(syscall::interface(interface.as_str())
                .map_err(|e| io("resolve XDP interface", e))?)?;
        let bpffs = optional_dir(&self.runtime.root, "fs", BENEATH)?
            .ok_or(Failure::Unsafe("program bpffs is missing"))?;
        crate::link::verify_bpffs(&bpffs)?;
        let pin = observe(self, &bpffs, "fs", &format!("prog_{program}"), false)?
            .ok_or(Failure::Unsafe("program pin is missing"))?;
        let owned = open_owned(&pin)?;
        let info = ProgramInfo::from_pin(proc_path(&owned)).map_err(Failure::Program)?;
        if info.id() != program.get() || info.program_type() != ProgramType::Extension.into() {
            return Err(Failure::Unsafe("expected the managed extension program").into());
        }
        let extension = Extension::from_pin(proc_path(&owned)).map_err(Failure::Program)?;
        let collection = ensure_directory(&bpffs, "xdp", CONFINED)?;
        Ok(PreparedXdp {
            extension,
            extension_entry: pin,
            collection,
            key: XdpKey { nsid, ifindex },
        })
    }

    /// Observe every artifact and validate kernel identities before teardown.
    pub fn observe_xdp(&self, snapshot: &XdpSnapshot) -> Result<XdpArtifacts, Error> {
        let details = &snapshot.details;
        let mut found = XdpArtifacts {
            outer: None,
            extension: None,
            program: None,
            directory: None,
        };
        let Some(bpffs) = optional_dir(&self.runtime.root, "fs", BENEATH)? else {
            require_detached(snapshot)?;
            return Ok(found);
        };
        crate::link::verify_bpffs(&bpffs)?;
        let Some(collection) = optional_dir(&bpffs, "xdp", CONFINED)? else {
            require_detached(snapshot)?;
            return Ok(found);
        };
        if let Some(pin) = observe(self, &collection, "fs/xdp", &outer_name(details.key), false)? {
            let owned = open_owned(&pin)?;
            let fd = syscall::open(&proc_path(&owned)).map_err(|e| io("open outer XDP link", e))?;
            let info = syscall::info(fd.as_fd()).map_err(|e| io("inspect outer XDP link", e))?;
            if info.kind != 6
                || info.id != snapshot.outer_link_id.get()
                || info.program != details.dispatcher_id.get()
                || (info.data[0] != 0 && info.data[0] != details.key.ifindex.get())
            {
                return Err(Failure::Unsafe("outer link identity differs from snapshot").into());
            }
            found.outer = Some(XdpOuter {
                root: pin.root,
                state: OuterState::Pinned {
                    entry: Box::new(pin),
                    id: snapshot.outer_link_id,
                },
            });
        } else {
            require_detached(snapshot)?;
        }
        let rev = revision_name(details.key, details.revision);
        if let Some(directory) = observe(self, &collection, "fs/xdp", &rev, true)? {
            let fd = open_owned(&directory)?;
            for child in std::fs::read_dir(proc_path(&fd))
                .map_err(|e| io("enumerate dispatcher revision", e))?
            {
                let child = child.map_err(|e| io("read dispatcher child", e))?;
                if child.file_name() != "dispatcher" && child.file_name() != "link_0" {
                    return Err(Failure::Unsafe("unknown dispatcher revision child").into());
                }
            }
            let parent = format!("fs/xdp/{rev}");
            if let Some(pin) = observe(self, &fd, &parent, "dispatcher", false)? {
                let owned = open_owned(&pin)?;
                let info = ProgramInfo::from_pin(proc_path(&owned)).map_err(Failure::Program)?;
                if info.id() != details.dispatcher_id.get()
                    || info.program_type() != ProgramType::Xdp.into()
                {
                    return Err(Failure::Unsafe("dispatcher program differs from snapshot").into());
                }
                found.program = Some(XdpProgramPin {
                    entry: pin,
                    id: details.dispatcher_id,
                });
            }
            if let Some(pin) = observe(self, &fd, &parent, "link_0", false)? {
                let owned = open_owned(&pin)?;
                let link =
                    syscall::open(&proc_path(&owned)).map_err(|e| io("open extension link", e))?;
                let info =
                    syscall::info(link.as_fd()).map_err(|e| io("inspect extension link", e))?;
                let bpfman_model::LinkState::Attached { kernel_id } = snapshot.member.state else {
                    return Err(Failure::Unsafe("XDP link is not committed").into());
                };
                if info.kind != 2
                    || info.id != kernel_id.get()
                    || info.program != snapshot.member.program_id.get()
                    || info.data[1] != details.dispatcher_id.get()
                {
                    return Err(Failure::Unsafe("extension link differs from snapshot").into());
                }
                found.extension = Some(XdpExtensionPin {
                    entry: pin,
                    id: kernel_id,
                });
            }
            found.directory = Some(XdpRevision { entry: directory });
        }
        Ok(found)
    }

    /// Stop traffic synchronously, then remove only the owned outer pin.
    pub fn remove_xdp_outer(
        &self,
        receipt: XdpOuter,
    ) -> Result<(), EffectFailure<XdpOuter, Error>> {
        let result = (|| {
            if identity(&self.runtime.root)? != receipt.root {
                return Err(Failure::Unsafe("XDP link belongs to another runtime").into());
            }
            match &receipt.state {
                OuterState::Live(fd) => {
                    syscall::detach(fd.as_fd()).map_err(|e| io("detach live XDP link", e))
                }
                OuterState::Pinned { entry: pin, id } => {
                    pin.check_writer(self)?;
                    let owned = open_owned(pin)?;
                    let fd = syscall::open(&proc_path(&owned))
                        .map_err(|e| io("reopen outer XDP link", e))?;
                    let info =
                        syscall::info(fd.as_fd()).map_err(|e| io("inspect outer XDP link", e))?;
                    if info.id != id.get() || info.kind != 6 {
                        return Err(Failure::Unsafe("outer XDP identity changed").into());
                    }
                    syscall::detach(fd.as_fd()).map_err(|e| io("detach outer XDP link", e))?;
                    crate::removal::remove(self, pin)
                }
            }
        })();
        result.map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })
    }

    /// Remove the owned extension pin after traffic is stopped.
    pub fn remove_xdp_extension(
        &self,
        receipt: XdpExtensionPin,
    ) -> Result<(), EffectFailure<XdpExtensionPin, Error>> {
        crate::removal::remove(self, &receipt.entry).map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })
    }

    /// Remove the owned dispatcher program pin after traffic is stopped.
    pub fn remove_xdp_program(
        &self,
        receipt: XdpProgramPin,
    ) -> Result<(), EffectFailure<XdpProgramPin, Error>> {
        crate::removal::remove(self, &receipt.entry).map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })
    }

    /// Remove an empty owned revision directory after its child pins are gone.
    pub fn remove_xdp_revision(
        &self,
        receipt: XdpRevision,
    ) -> Result<(), EffectFailure<XdpRevision, Error>> {
        crate::removal::remove(self, &receipt.entry).map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })
    }
}

impl PreparedXdp {
    /// Observed namespace and interface identity.
    pub fn key(&self) -> XdpKey {
        self.key
    }

    /// Exclusively create the first revision directory; never adopt prior residue.
    pub fn create_revision(
        &self,
        writer: &RuntimeWriter<'_>,
        revision: NonZeroU32,
    ) -> Result<XdpRevision, EffectFailure<Option<XdpRevision>, Error>> {
        self.extension_entry.check_writer(writer).map_err(fail)?;
        let mut receipt = XdpRevision {
            entry: entry(
                writer,
                &self.collection,
                "fs/xdp".into(),
                revision_name(self.key, revision),
                true,
            )
            .map_err(fail)?,
        };
        receipt.entry.check_writer(writer).map_err(fail)?;
        rustix::fs::mkdirat(
            &self.collection,
            &receipt.entry.name,
            rustix::fs::Mode::RWXU,
        )
        .map_err(|e| fail(io("create XDP revision", e)))?;
        match receipt.entry.observe() {
            Ok(()) => Ok(receipt),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: Some(receipt),
            }),
        }
    }

    /// Attach and pin the extension at slot zero of this dispatcher.
    pub fn pin_extension(
        &mut self,
        writer: &RuntimeWriter<'_>,
        directory: &XdpRevision,
        dispatcher: &Xdp,
    ) -> Result<XdpExtensionPin, EffectFailure<Option<XdpExtensionPin>, Error>> {
        self.extension_entry.check_writer(writer).map_err(fail)?;
        open_owned(&self.extension_entry).map_err(fail)?;
        directory.entry.check_writer(writer).map_err(fail)?;
        let dir = open_owned(&directory.entry).map_err(fail)?;
        let mut pin = entry(
            writer,
            &dir,
            format!("fs/xdp/{}", directory.entry.name),
            "link_0".into(),
            false,
        )
        .map_err(fail)?;
        let link_id = self
            .extension
            .attach_to_program(
                dispatcher
                    .fd()
                    .map_err(|e| fail(Failure::Program(e).into()))?,
                "prog0",
            )
            .map_err(|e| fail(Failure::Program(e).into()))?;
        let link: FdLink = self
            .extension
            .take_link(link_id)
            .map_err(|e| fail(Failure::Program(e).into()))?
            .into();
        let id = nz(link.info().map_err(|e| fail(Failure::Link(e).into()))?.id()).map_err(fail)?;
        let pinned = link
            .pin(proc_path(&dir).join("link_0"))
            .map_err(|e| fail(Failure::Pin(e).into()))?;
        drop(pinned);
        match pin.observe() {
            Ok(()) => Ok(XdpExtensionPin { entry: pin, id }),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: Some(XdpExtensionPin { entry: pin, id }),
            }),
        }
    }

    /// Attach without replacing an existing interface program. A pin failure
    /// retains the live fd so compensation can synchronously detach it.
    pub fn pin_outer(
        &self,
        writer: &RuntimeWriter<'_>,
        dispatcher: &Xdp,
    ) -> Result<XdpOuter, EffectFailure<Option<XdpOuter>, Error>> {
        let mut pin = entry(
            writer,
            &self.collection,
            "fs/xdp".into(),
            outer_name(self.key),
            false,
        )
        .map_err(fail)?;
        pin.check_writer(writer).map_err(fail)?;
        let fd = syscall::outer(
            dispatcher
                .fd()
                .map_err(|e| fail(Failure::Program(e).into()))?
                .as_fd(),
            self.key.ifindex.get(),
        )
        .map_err(|e| fail(io("attach outer XDP link", e)))?;
        // Keep ownership even if info fails. ID is only needed after pinning;
        // the live receipt authorizes detach through its owned descriptor.
        let info = match syscall::info(fd.as_fd()) {
            Ok(info) => info,
            Err(e) => {
                return Err(EffectFailure {
                    cause: io("inspect acquired XDP link", e),
                    remaining: Some(XdpOuter {
                        root: pin.root,
                        state: OuterState::Live(fd),
                    }),
                });
            }
        };
        let id = match nz(info.id) {
            Ok(id) => id,
            Err(cause) => {
                return Err(EffectFailure {
                    cause,
                    remaining: Some(XdpOuter {
                        root: pin.root,
                        state: OuterState::Live(fd),
                    }),
                });
            }
        };
        if let Err(e) = syscall::pin(fd.as_fd(), &proc_path(&self.collection).join(&pin.name)) {
            return Err(EffectFailure {
                cause: io("pin outer XDP link", e),
                remaining: Some(XdpOuter {
                    root: pin.root,
                    state: OuterState::Live(fd),
                }),
            });
        }
        drop(fd);
        let result = pin.observe();
        let receipt = XdpOuter {
            root: pin.root,
            state: OuterState::Pinned {
                entry: Box::new(pin),
                id,
            },
        };
        match result {
            Ok(()) => Ok(receipt),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: Some(receipt),
            }),
        }
    }
}

impl XdpRevision {
    /// Pin the loaded dispatcher beneath this owned revision.
    pub fn pin_program(
        &self,
        writer: &RuntimeWriter<'_>,
        program: &mut Xdp,
    ) -> Result<XdpProgramPin, EffectFailure<Option<XdpProgramPin>, Error>> {
        self.entry.check_writer(writer).map_err(fail)?;
        let directory = open_owned(&self.entry).map_err(fail)?;
        let id = nz(program
            .info()
            .map_err(|e| fail(Failure::Program(e).into()))?
            .id())
        .map_err(fail)?;
        let mut pin = entry(
            writer,
            &directory,
            format!("fs/xdp/{}", self.entry.name),
            "dispatcher".into(),
            false,
        )
        .map_err(fail)?;
        program
            .pin(proc_path(&directory).join("dispatcher"))
            .map_err(|e| fail(Failure::Pin(e).into()))?;
        let result = pin.observe();
        let receipt = XdpProgramPin { entry: pin, id };
        match result {
            Ok(()) => Ok(receipt),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: Some(receipt),
            }),
        }
    }
}

impl XdpProgramPin {
    /// Kernel dispatcher identity.
    pub fn id(&self) -> NonZeroU32 {
        self.id
    }
}

impl XdpExtensionPin {
    /// Kernel extension-link identity.
    pub fn id(&self) -> NonZeroU32 {
        self.id
    }
}

impl XdpOuter {
    /// Kernel identity only after pinning succeeded. Unpinned partial ownership
    /// cannot be committed as a persistent attachment.
    pub fn id(&self) -> Result<NonZeroU32, Error> {
        match &self.state {
            OuterState::Pinned { id, .. } => Ok(*id),
            OuterState::Live(_) => Err(Failure::Unsafe("outer link has not been pinned").into()),
        }
    }
}

impl RuntimeDirectory {
    /// Inspect a canonical first-slot link pin without acquiring writer authority.
    pub fn read_xdp_link_pin(
        &self,
        details: &XdpLink,
    ) -> Result<Option<bpfman_model::KernelLink>, Error> {
        let Some(bpffs) = optional_dir(&self.root, "fs", BENEATH)? else {
            return Ok(None);
        };
        crate::link::verify_bpffs(&bpffs)?;
        let Some(collection) = optional_dir(&bpffs, "xdp", CONFINED)? else {
            return Ok(None);
        };
        let Some(revision) = optional_dir(
            &collection,
            &revision_name(details.key, details.revision),
            CONFINED,
        )?
        else {
            return Ok(None);
        };
        use rustix::fs::{Mode, OFlags, openat2};
        let pin = match openat2(
            &revision,
            "link_0",
            OFlags::PATH | OFlags::CLOEXEC,
            Mode::empty(),
            CONFINED,
        ) {
            Ok(pin) => pin,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(io("open XDP extension pin", e)),
        };
        let stat = rustix::fs::fstat(&pin).map_err(|e| io("inspect XDP pin inode", e))?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile
            || stat.st_nlink > 1
        {
            return Err(Failure::Unsafe("invalid XDP pin inode").into());
        }
        let fd =
            syscall::open(&proc_path(&pin)).map_err(|e| io("open pinned XDP extension link", e))?;
        let info =
            syscall::info(fd.as_fd()).map_err(|e| io("inspect pinned XDP extension link", e))?;
        if info.kind != 2 || info.data[1] != details.dispatcher_id.get() {
            return Err(Failure::Unsafe("unexpected extension link target or type").into());
        }
        Ok(Some(bpfman_model::KernelLink {
            details: bpfman_model::KernelLinkDetails::Tracing {
                attach_type: info.data[0],
                target_obj_id: info.data[1],
                target_btf_id: info.data[2],
            },
            id: nz(info.id)?,
            program_id: nz(info.program)?,
        }))
    }
}
