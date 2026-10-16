//! The only managed-object unlink boundary. Never recursive; receipts, writer
//! identity, no-symlink traversal and inode checks precede every removal.
//! Writers must cooperate with the runtime lock. These checks do not sandbox
//! privileged processes concurrently replacing the final directory entry.
#![allow(clippy::disallowed_methods)]

use crate::{
    Bytecode, Error, LinkPin, MapDirectory, MapPin, ProgramPin, RuntimeWriter,
    artifacts::{Entry, identity, proc_path},
    directory::CONFINED,
    error::{Failure, io},
};
use bpfman_core::EffectFailure;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, fstat, openat2, unlinkat};
use std::os::fd::OwnedFd;

#[derive(Clone, Copy, Debug)]
pub(super) enum LinkKind {
    Tcx(bpfman_model::TcKey),
    PerfEvent,
    Tracing(bpfman_model::TracingKind),
    Extension,
}

pub(super) fn open_owned(entry: &Entry) -> Result<OwnedFd, Error> {
    let flags = if entry.directory {
        OFlags::RDONLY | OFlags::DIRECTORY
    } else {
        OFlags::PATH
    };
    let fd = openat2(
        &entry.parent,
        &entry.name,
        flags | OFlags::CLOEXEC,
        Mode::empty(),
        CONFINED,
    )
    .map_err(|e| io("open owned artifact for removal", e))?;

    if Some(identity(&fd)?) != entry.identity {
        return Err(Failure::Unsafe("owned artifact was replaced or identity is unknown").into());
    }

    let stat = fstat(&fd).map_err(|e| io("inspect owned artifact type", e))?;
    let expected = if entry.directory {
        FileType::Directory
    } else {
        FileType::RegularFile
    };

    if FileType::from_raw_mode(stat.st_mode) != expected || (!entry.directory && stat.st_nlink != 1)
    {
        return Err(Failure::Unsafe("owned artifact has unexpected type or hard links").into());
    }

    Ok(fd)
}

pub(super) fn remove(writer: &RuntimeWriter<'_>, entry: &Entry) -> Result<(), Error> {
    entry.check_writer(writer)?;
    let _owned = open_owned(entry)?;
    let flags = if entry.directory {
        AtFlags::REMOVEDIR
    } else {
        AtFlags::empty()
    };
    unlinkat(&entry.parent, &entry.name, flags).map_err(|e| io("remove owned artifact", e))
}

pub(super) fn remove_link(
    writer: &RuntimeWriter<'_>,
    kernel: &impl crate::LinkRetention,
    entry: &Entry,
    id: std::num::NonZeroU32,
    kind: LinkKind,
) -> Result<(), Error> {
    entry.check_writer(writer)?;
    let (info, retained) = {
        let owned = open_owned(entry)?;
        kernel
            .retain_link(crate::PinSource(&proc_path(&owned)))
            .map_err(Failure::Kernel)?
    };
    let expected_kind = match kind {
        LinkKind::Tcx(key) => {
            matches!(info.details, bpfman_model::KernelLinkDetails::Tcx { ifindex, direction } if direction == key.direction && (ifindex == 0 || ifindex == key.ifindex.get()))
        }
        LinkKind::Tracing(kind) => {
            let expected = match kind {
                bpfman_model::TracingKind::Fentry => 24,
                bpfman_model::TracingKind::Fexit => 25,
                bpfman_model::TracingKind::Lsm => 27,
            };
            matches!(info.details, bpfman_model::KernelLinkDetails::Tracing { attach_type, .. } if attach_type == expected)
        }
        LinkKind::PerfEvent => info.details == bpfman_model::KernelLinkDetails::PerfEvent,
        // EXT selects freplace; Aya creates its tracing link with attach type zero.
        LinkKind::Extension => matches!(
            info.details,
            bpfman_model::KernelLinkDetails::Tracing { attach_type: 0, .. }
        ),
    };
    if info.id != id || !expected_kind {
        return Err(Failure::Unsafe("owned link identity or type changed").into());
    }

    // Every O_PATH descriptor to the pin must close before the live
    // descriptor: it can keep the unlinked inode's pin reference alive.
    if matches!(kind, LinkKind::Tcx(_)) {
        kernel.detach_retained(&retained).map_err(Failure::Kernel)?;
    }
    remove(writer, entry)?;
    drop(retained);
    Ok(())
}

impl RuntimeWriter<'_> {
    /// Remove an owned standalone pin while retaining its live link descriptor.
    /// Closing that descriptor afterward releases the last managed reference
    /// through the kernel's direct close path rather than queued pin release.
    pub fn remove_link_pin(
        &self,
        kernel: &impl crate::LinkRetention,
        receipt: LinkPin,
    ) -> Result<(), EffectFailure<LinkPin, Error>> {
        let result = remove_link(self, kernel, &receipt.entry, receipt.id, receipt.kind);
        result.map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })
    }

    /// Consume an owned program pin. Failure preserves the receipt for retry.
    pub fn remove_program_pin(
        &self,
        receipt: ProgramPin,
    ) -> Result<(), EffectFailure<ProgramPin, Error>> {
        remove(self, &receipt.entry).map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })
    }

    /// Consume exactly one private map pin; never remove its parent implicitly.
    pub fn remove_map_pin(&self, receipt: MapPin) -> Result<(), EffectFailure<MapPin, Error>> {
        remove(self, &receipt.entry).map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })
    }

    /// Remove an owned map directory after all its map-pin receipts are consumed.
    /// An unexpected entry or unresolved pin prevents rmdir; nothing is recursive.
    pub fn remove_empty_map_directory(
        &self,
        receipt: MapDirectory,
    ) -> Result<(), EffectFailure<MapDirectory, Error>> {
        remove(self, &receipt.entry).map_err(|cause| EffectFailure {
            cause,
            remaining: receipt,
        })
    }

    /// Remove each owned bytecode file once, then its directory only if every
    /// child removal succeeded. Retain all failures and unresolved ownership.
    pub fn remove_bytecode(
        &self,
        mut receipt: Bytecode,
    ) -> Result<(), EffectFailure<Bytecode, Error>> {
        let mut causes = Vec::new();

        if let Err(cause) = receipt
            .directory
            .check_writer(self)
            .and_then(|()| open_owned(&receipt.directory).map(|_| ()))
        {
            return Err(EffectFailure {
                cause,
                remaining: receipt,
            });
        }

        receipt.files.retain(|entry| match remove(self, entry) {
            Ok(()) => false,
            Err(cause) => {
                causes.push(cause);
                true
            }
        });

        if causes.is_empty() {
            if let Err(cause) = remove(self, &receipt.directory) {
                causes.push(cause);
            }
        }

        if causes.is_empty() {
            Ok(())
        } else {
            Err(EffectFailure {
                cause: Failure::Cleanup(causes).into(),
                remaining: receipt,
            })
        }
    }
}
