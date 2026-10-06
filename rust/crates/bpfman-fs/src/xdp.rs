//! XDP acquisitions and adoption stay beneath verified runtime descriptors.

use crate::{
    Error, RuntimeDirectory, RuntimeWriter,
    artifacts::{Entry, Identity, entry, identity, proc_path},
    directory::{BENEATH, CONFINED, ensure_directory},
    error::{Failure, io},
    observe::{observe, optional_dir},
    removal::open_owned,
};
use crate::{ExtensionProgram, LinkPinning, OuterLink, ProgramPinning};
use bpfman_core::EffectFailure;
use bpfman_model::{InterfaceName, XdpKey, XdpLink, XdpSnapshot};
use std::{num::NonZeroU32, os::fd::OwnedFd};

/// Adopted extension and attach point; no managed attachment has been created.
pub struct PreparedXdp<E: ExtensionProgram, N> {
    namespace: N,
    extension: E,
    extension_entry: Entry,
    collection: OwnedFd,
    key: XdpKey,
}

/// Owned revision directory; never a recursive removal capability.
pub struct XdpRevision {
    entry: Box<Entry>,
    key: XdpKey,
    revision: NonZeroU32,
}

/// Owned dispatcher program pin.
pub struct XdpProgramPin {
    entry: Box<Entry>,
    key: XdpKey,
    revision: NonZeroU32,
    id: NonZeroU32,
}

/// Owned extension link pin.
pub struct XdpExtensionPin {
    entry: Entry,
    id: NonZeroU32,
}

/// Outer link ownership. A live fd survives failed pinning; pinned evidence does
/// not retain a link fd, so observations cannot prolong a detached attachment.
pub struct XdpOuter<L: OuterLink> {
    root: Identity,
    state: OuterState<L>,
}

enum OuterState<L: OuterLink> {
    Live(L),
    Pinned { entry: Box<Entry>, id: NonZeroU32 },
}

/// Complete preflight observations for last-detach; missing artifacts allow retry.
pub struct XdpArtifacts<L: OuterLink> {
    /// Outer interface link.
    pub outer: Option<XdpOuter<L>>,
    /// Extension link.
    pub extension: Option<XdpExtensionPin>,
    /// Dispatcher program pin.
    pub program: Option<XdpProgramPin>,
    /// Revision directory.
    pub directory: Option<XdpRevision>,
}

mod replacement;
pub use replacement::XdpSwitch;

/// Complete revision observations for replacement or final teardown.
pub struct XdpDispatcherArtifacts<L: OuterLink> {
    /// Durable outer interface link.
    pub outer: Option<XdpOuter<L>>,
    /// All observed extension pins, in slot order.
    pub extensions: Vec<XdpExtensionPin>,
    /// Dispatcher program pin.
    pub program: Option<XdpProgramPin>,
    /// Dependent revision container.
    pub directory: Option<XdpRevision>,
}

type OuterAcquisition<L> = Result<XdpOuter<L>, EffectFailure<Option<XdpOuter<L>>, Error>>;

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
fn require_detached(kernel: &impl crate::XdpKernel, snapshot: &XdpSnapshot) -> Result<(), Error> {
    let Some(fd) = kernel
        .outer_by_id(snapshot.outer_link_id)
        .map_err(Failure::Kernel)?
    else {
        return Ok(());
    };
    let info = fd.info().map_err(Failure::Kernel)?;
    if info.id != snapshot.outer_link_id.get()
        || info.program != snapshot.details.dispatcher_id.get()
        || info.ifindex != 0
    {
        return Err(
            Failure::Unsafe("outer pin is missing but its link is not proven detached").into(),
        );
    }
    Ok(())
}

impl RuntimeWriter<'_> {
    /// Resolve a selected namespace/interface and adopt a canonical EXT program pin.
    pub fn prepare_xdp<K: crate::XdpKernel>(
        &self,
        kernel: &K,
        program: NonZeroU32,
        interface: &InterfaceName,
        netns: &bpfman_model::NetworkNamespace,
    ) -> Result<PreparedXdp<K::Extension, K::Namespace>, Error> {
        let (key, namespace) = kernel
            .interface(interface, netns)
            .map_err(Failure::Kernel)?;
        let bpffs = optional_dir(&self.runtime.root, "fs", BENEATH)?
            .ok_or(Failure::Unsafe("program bpffs is missing"))?;
        crate::link::verify_bpffs(&bpffs)?;
        let pin = observe(self, &bpffs, "fs", &format!("prog_{program}"), false)?
            .ok_or(Failure::Unsafe("program pin is missing"))?;
        let owned = open_owned(&pin)?;
        let (info, extension) = kernel
            .extension_at(crate::PinSource(&proc_path(&owned)))
            .map_err(Failure::Kernel)?;
        if info.id != program.get() || info.kind != crate::PinProgramKind::Extension {
            return Err(Failure::Unsafe("expected the managed extension program").into());
        }
        let collection = ensure_directory(&bpffs, "xdp", CONFINED)?;
        Ok(PreparedXdp {
            namespace,
            extension,
            extension_entry: pin,
            collection,
            key,
        })
    }

    /// Observe every artifact and validate kernel identities before teardown.
    pub fn observe_xdp<K: crate::XdpKernel>(
        &self,
        kernel: &K,
        snapshot: &XdpSnapshot,
    ) -> Result<XdpArtifacts<K::Outer>, Error> {
        let complete = bpfman_model::XdpDispatcherSnapshot::new(vec![snapshot.clone()])
            .map_err(|_| Failure::Unsafe("invalid singleton snapshot"))?;
        let mut all = self.observe_xdp_dispatcher(kernel, &complete)?;
        Ok(XdpArtifacts {
            outer: all.outer,
            extension: all.extensions.pop(),
            program: all.program,
            directory: all.directory,
        })
    }

    /// Observe all members and reject unknown revision children before mutation.
    pub fn observe_xdp_dispatcher<K: crate::XdpKernel>(
        &self,
        kernel: &K,
        snapshot: &bpfman_model::XdpDispatcherSnapshot,
    ) -> Result<XdpDispatcherArtifacts<K::Outer>, Error> {
        let members = snapshot.members();
        let first = members
            .first()
            .ok_or(Failure::Unsafe("empty dispatcher snapshot"))?;
        let details = &first.details;
        let mut found = XdpDispatcherArtifacts {
            outer: None,
            extensions: Vec::new(),
            program: None,
            directory: None,
        };
        let Some(bpffs) = optional_dir(&self.runtime.root, "fs", BENEATH)? else {
            require_detached(kernel, first)?;
            return Ok(found);
        };
        crate::link::verify_bpffs(&bpffs)?;
        let Some(collection) = optional_dir(&bpffs, "xdp", CONFINED)? else {
            require_detached(kernel, first)?;
            return Ok(found);
        };
        if let Some(pin) = observe(self, &collection, "fs/xdp", &outer_name(details.key), false)? {
            let owned = open_owned(&pin)?;
            let fd = kernel
                .outer_at(crate::PinSource(&proc_path(&owned)))
                .map_err(Failure::Kernel)?;
            let info = fd.info().map_err(Failure::Kernel)?;
            if info.id != first.outer_link_id.get()
                || info.program != details.dispatcher_id.get()
                || (info.ifindex != 0 && info.ifindex != details.key.ifindex.get())
            {
                return Err(Failure::Unsafe("outer link identity differs from snapshot").into());
            }
            found.outer = Some(XdpOuter {
                root: pin.root,
                state: OuterState::Pinned {
                    entry: Box::new(pin),
                    id: first.outer_link_id,
                },
            });
        } else {
            require_detached(kernel, first)?;
        }
        let rev = revision_name(details.key, details.revision);
        if let Some(directory) = observe(self, &collection, "fs/xdp", &rev, true)? {
            let fd = open_owned(&directory)?;
            for child in std::fs::read_dir(proc_path(&fd))
                .map_err(|e| io("enumerate dispatcher revision", e))?
            {
                let child = child.map_err(|e| io("read dispatcher child", e))?;
                if child.file_name() != "dispatcher"
                    && !members.iter().any(|m| {
                        child.file_name() == format!("link_{}", m.details.slot.index()).as_str()
                    })
                {
                    return Err(Failure::Unsafe("unknown dispatcher revision child").into());
                }
            }
            let parent = format!("fs/xdp/{rev}");
            if let Some(pin) = observe(self, &fd, &parent, "dispatcher", false)? {
                let owned = open_owned(&pin)?;
                let info = kernel
                    .program_at(crate::PinSource(&proc_path(&owned)))
                    .map_err(Failure::Kernel)?;
                if info.id != details.dispatcher_id.get() || info.kind != crate::PinProgramKind::Xdp
                {
                    return Err(Failure::Unsafe("dispatcher program differs from snapshot").into());
                }
                found.program = Some(XdpProgramPin {
                    entry: Box::new(pin),
                    key: details.key,
                    revision: details.revision,
                    id: details.dispatcher_id,
                });
            }
            for member in members {
                if let Some(pin) = observe(
                    self,
                    &fd,
                    &parent,
                    &format!("link_{}", member.details.slot.index()),
                    false,
                )? {
                    let owned = open_owned(&pin)?;
                    let info = kernel
                        .link_at(crate::PinSource(&proc_path(&owned)))
                        .map_err(Failure::Kernel)?;
                    let bpfman_model::LinkState::Attached { kernel_id } = member.member.state
                    else {
                        return Err(Failure::Unsafe("XDP link is not committed").into());
                    };
                    if info.id != kernel_id
                        || info.program_id != member.member.program_id
                        || !matches!(info.details, bpfman_model::KernelLinkDetails::Tracing { target_obj_id, .. } if target_obj_id == details.dispatcher_id.get())
                    {
                        return Err(Failure::Unsafe("extension link differs from snapshot").into());
                    }
                    found.extensions.push(XdpExtensionPin {
                        entry: pin,
                        id: kernel_id,
                    });
                }
            }
            found.directory = Some(XdpRevision {
                entry: Box::new(directory),
                key: details.key,
                revision: details.revision,
            });
        }
        Ok(found)
    }

    /// Stop traffic synchronously, then remove only the owned outer pin.
    pub fn remove_xdp_outer<K: crate::XdpKernel>(
        &self,
        kernel: &K,
        receipt: XdpOuter<K::Outer>,
    ) -> Result<(), EffectFailure<XdpOuter<K::Outer>, Error>> {
        let result = (|| {
            if identity(&self.runtime.root)? != receipt.root {
                return Err(Failure::Unsafe("XDP link belongs to another runtime").into());
            }
            match &receipt.state {
                OuterState::Live(fd) => fd.detach().map_err(|e| Failure::Kernel(e).into()),
                OuterState::Pinned { entry: pin, id } => {
                    pin.check_writer(self)?;
                    let owned = open_owned(pin)?;
                    let fd = kernel
                        .outer_at(crate::PinSource(&proc_path(&owned)))
                        .map_err(Failure::Kernel)?;
                    let info = fd.info().map_err(Failure::Kernel)?;
                    if info.id != id.get() {
                        return Err(Failure::Unsafe("outer XDP identity changed").into());
                    }
                    fd.detach().map_err(Failure::Kernel)?;
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

impl<E: ExtensionProgram, N> PreparedXdp<E, N> {
    /// Observed namespace and interface identity.
    pub fn key(&self) -> XdpKey {
        self.key
    }

    /// Revalidate the retained namespace and managed program under their original writer.
    pub fn validate<K: crate::XdpKernel<Extension = E, Namespace = N>>(
        &self,
        kernel: &K,
        writer: &RuntimeWriter<'_>,
    ) -> Result<(), Error> {
        kernel
            .validate_namespace(&self.namespace)
            .map_err(Failure::Kernel)?;
        self.extension_entry.check_writer(writer)?;
        open_owned(&self.extension_entry)?;
        Ok(())
    }

    /// Exclusively create the first revision directory; never adopt prior residue.
    pub fn create_revision(
        &self,
        writer: &RuntimeWriter<'_>,
        revision: NonZeroU32,
    ) -> Result<XdpRevision, EffectFailure<Option<XdpRevision>, Error>> {
        self.extension_entry.check_writer(writer).map_err(fail)?;
        let mut receipt = XdpRevision {
            key: self.key,
            revision,
            entry: Box::new(
                entry(
                    writer,
                    &self.collection,
                    "fs/xdp".into(),
                    revision_name(self.key, revision),
                    true,
                )
                .map_err(fail)?,
            ),
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
        dispatcher: &E::Dispatcher,
    ) -> Result<XdpExtensionPin, EffectFailure<Option<XdpExtensionPin>, Error>> {
        self.pin_extension_slot(writer, directory, dispatcher, bpfman_model::XdpSlot::FIRST)
    }

    /// Attach and pin a validated slot in this owned dispatcher revision.
    pub fn pin_extension_slot(
        &mut self,
        writer: &RuntimeWriter<'_>,
        directory: &XdpRevision,
        dispatcher: &E::Dispatcher,
        slot: bpfman_model::XdpSlot,
    ) -> Result<XdpExtensionPin, EffectFailure<Option<XdpExtensionPin>, Error>> {
        if directory.key != self.key {
            return Err(fail(
                Failure::Unsafe("revision belongs to another attach point").into(),
            ));
        }
        let name = format!("link_{}", slot.index());
        self.extension_entry.check_writer(writer).map_err(fail)?;
        open_owned(&self.extension_entry).map_err(fail)?;
        directory.entry.check_writer(writer).map_err(fail)?;
        let dir = open_owned(&directory.entry).map_err(fail)?;
        let mut pin = entry(
            writer,
            &dir,
            format!("fs/xdp/{}", directory.entry.name),
            name.clone(),
            false,
        )
        .map_err(fail)?;
        let link = self
            .extension
            .attach(dispatcher, slot)
            .map_err(|e| fail(Failure::Kernel(e).into()))?;
        let id = nz(link.id().map_err(|e| fail(Failure::Kernel(e).into()))?).map_err(fail)?;
        link.pin(crate::PinTarget(&proc_path(&dir).join(&name)))
            .map_err(|e| fail(Failure::Kernel(e).into()))?;
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
    pub fn pin_outer<K: crate::XdpKernel<Extension = E, Namespace = N>>(
        &self,
        kernel: &K,
        writer: &RuntimeWriter<'_>,
        dispatcher: &E::Dispatcher,
    ) -> OuterAcquisition<K::Outer> {
        let mut pin = entry(
            writer,
            &self.collection,
            "fs/xdp".into(),
            outer_name(self.key),
            false,
        )
        .map_err(fail)?;
        pin.check_writer(writer).map_err(fail)?;
        let fd = kernel
            .attach_outer(dispatcher, self.key, &self.namespace)
            .map_err(|e| fail(Failure::Kernel(e).into()))?;
        // Keep ownership even if info fails. ID is only needed after pinning;
        // the live receipt authorizes detach through its owned descriptor.
        let info = match fd.info() {
            Ok(info) => info,
            Err(e) => {
                return Err(EffectFailure {
                    cause: Failure::Kernel(e).into(),
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
        if let Err(e) = fd.pin(crate::PinTarget(
            &proc_path(&self.collection).join(&pin.name),
        )) {
            return Err(EffectFailure {
                cause: Failure::Kernel(e).into(),
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
        program: &mut impl ProgramPinning,
    ) -> Result<XdpProgramPin, EffectFailure<Option<XdpProgramPin>, Error>> {
        self.entry.check_writer(writer).map_err(fail)?;
        let directory = open_owned(&self.entry).map_err(fail)?;
        let id = nz(program.id().map_err(|e| fail(Failure::Kernel(e).into()))?).map_err(fail)?;
        let mut pin = entry(
            writer,
            &directory,
            format!("fs/xdp/{}", self.entry.name),
            "dispatcher".into(),
            false,
        )
        .map_err(fail)?;
        program
            .pin(crate::PinTarget(&proc_path(&directory).join("dispatcher")))
            .map_err(|e| fail(Failure::Kernel(e).into()))?;
        let result = pin.observe();
        let receipt = XdpProgramPin {
            entry: Box::new(pin),
            id,
            key: self.key,
            revision: self.revision,
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

impl<L: OuterLink> XdpOuter<L> {
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
    /// Inspect a canonical recorded-slot link pin without acquiring writer authority.
    pub fn read_xdp_link_pin(
        &self,
        kernel: &impl crate::LinkInspection,
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
            format!("link_{}", details.slot.index()),
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
        let info = kernel
            .link_at(crate::PinSource(&proc_path(&pin)))
            .map_err(Failure::Kernel)?;
        if !matches!(info.details, bpfman_model::KernelLinkDetails::Tracing { target_obj_id, .. } if target_obj_id == details.dispatcher_id.get())
        {
            return Err(Failure::Unsafe("unexpected extension link target or type").into());
        }
        Ok(Some(info))
    }
}
