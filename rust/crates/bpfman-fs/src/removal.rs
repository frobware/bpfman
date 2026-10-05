//! The only managed-object unlink boundary. Never recursive; receipts, writer
//! identity, no-symlink traversal and inode checks precede every removal.
//! Writers must cooperate with the runtime lock. These checks do not sandbox
//! privileged processes concurrently replacing the final directory entry.
#![allow(clippy::disallowed_methods)]

use crate::{
    Bytecode, Error, LinkPin, MapDirectory, MapPin, ProgramPin, RuntimeWriter,
    artifacts::{Entry, identity},
    directory::CONFINED,
    error::{Failure, io},
};
use bpfman_core::EffectFailure;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, fstat, openat2, unlinkat};
use std::os::fd::OwnedFd;

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

impl RuntimeWriter<'_> {
    /// Remove an owned link pin. No live descriptor is retained by this receipt;
    /// success releases this runtime's attachment reference before record deletion.
    pub fn remove_link_pin(&self, receipt: LinkPin) -> Result<(), EffectFailure<LinkPin, Error>> {
        remove(self, &receipt.entry).map_err(|cause| EffectFailure {
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
