//! Whole-store publication beneath verified directory descriptors.

use crate::{
    Error, RuntimeDirectory, RuntimeIdentity, RuntimeWriter,
    artifacts::identity,
    directory::{CONFINED, DIRECTORY},
    error::{Failure, io},
    layout::{DATABASE_DIRECTORY, DATABASE_FILE},
};
use rustix::fs::{FileType, Mode, OFlags, fstat, openat2, renameat};
use std::{
    fs::File,
    io::{Read, Write},
    os::fd::OwnedFd,
};

const PENDING: &str = "store.next";
const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Opened store directory for consistent whole-file reads. This carries no
/// mutation authority; replacement additionally requires the original writer.
pub struct StoreSnapshot {
    root: RuntimeIdentity,
    directory: OwnedFd,
}

fn regular(fd: &OwnedFd) -> Result<(), Error> {
    let stat = fstat(fd).map_err(|e| io("inspect store snapshot", e))?;

    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
        return Err(Failure::Unsafe("store snapshot must be a singly linked regular file").into());
    }

    Ok(())
}

fn read_opened(fd: OwnedFd) -> Result<Vec<u8>, Error> {
    let stat = fstat(&fd).map_err(|e| io("inspect opened store snapshot", e))?;

    // A publisher may have replaced this entry after open. The unlinked inode
    // remains an immutable snapshot owned by this descriptor. Additional hard
    // links are still refused; staging files must retain exactly one link.
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink > 1 {
        return Err(Failure::Unsafe(
            "store snapshot must be a regular file without extra hard links",
        )
        .into());
    }

    let mut bytes = Vec::new();
    File::from(fd)
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io("read store snapshot", e))?;

    if bytes.len() as u64 > MAX_BYTES {
        return Err(Failure::Unsafe("store snapshot exceeds size limit").into());
    }

    Ok(bytes)
}

impl StoreSnapshot {
    /// Read the currently published file, refusing symlinks and special files.
    /// A concurrent atomic publication yields either complete snapshot.
    #[tracing::instrument(name = "snapshot.read", level = "trace", skip_all, err)]
    pub fn read(&self) -> Result<Option<Vec<u8>>, Error> {
        let fd = match openat2(
            &self.directory,
            DATABASE_FILE,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
            CONFINED,
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(io("open store snapshot", e)),
        };
        read_opened(fd).map(Some)
    }
}

impl RuntimeDirectory {
    /// Open an existing whole-file store directory without taking the writer lock.
    /// None means the directory is absent. No filesystem objects are created.
    pub fn open_store_snapshot(&self) -> Result<Option<StoreSnapshot>, Error> {
        let directory = match openat2(
            &self.root,
            DATABASE_DIRECTORY,
            DIRECTORY,
            Mode::empty(),
            CONFINED,
        ) {
            Ok(directory) => directory,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(io("open store directory", e)),
        };

        Ok(Some(StoreSnapshot {
            root: self.identity()?,
            directory,
        }))
    }
}

impl RuntimeWriter<'_> {
    /// Open the store directory prepared by writer acquisition.
    /// SQLite and snapshot backends share one store slot to prevent a second inventory.
    pub fn open_store_snapshot(&self) -> Result<StoreSnapshot, Error> {
        self.runtime
            .open_store_snapshot()?
            .ok_or_else(|| Failure::Unsafe("store directory disappeared").into())
    }

    /// Replace exactly the observed snapshot after writing and syncing its contents.
    /// Err means publication did not occur. Rename is the final fallible operation.
    /// This guarantees atomic visibility, not directory-entry durability on power
    /// loss: a post-rename fsync failure cannot safely authorize load compensation.
    /// An interrupted write may leave the private staging file, reused on the next
    /// attempt after verifying its type and identity. Published state stays intact.
    #[tracing::instrument(name = "snapshot.publish", level = "trace", skip_all, fields(bytes = bytes.len()), err)]
    pub fn publish_store_snapshot(
        &self,
        snapshot: &StoreSnapshot,
        expected: Option<&[u8]>,
        bytes: &[u8],
    ) -> Result<(), Error> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err(Failure::Unsafe("store snapshot exceeds size limit").into());
        }

        let current = self.open_store_snapshot()?;

        if current.root != snapshot.root
            || identity(&current.directory)? != identity(&snapshot.directory)?
        {
            return Err(
                Failure::Unsafe("store snapshot belongs to another root or directory").into(),
            );
        }

        if snapshot.read()?.as_deref() != expected {
            return Err(Failure::Unsafe("store snapshot changed before publication").into());
        }

        let fd = openat2(
            &snapshot.directory,
            PENDING,
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::from_raw_mode(0o600),
            CONFINED,
        )
        .map_err(|e| io("open pending store snapshot", e))?;
        regular(&fd)?;
        let pending_identity = identity(&fd)?;
        let mut file = File::from(fd);
        file.set_len(0)
            .map_err(|e| io("truncate pending store snapshot", e))?;
        file.write_all(bytes)
            .map_err(|e| io("write pending store snapshot", e))?;
        file.sync_all()
            .map_err(|e| io("sync pending store snapshot", e))?;
        let pending = openat2(
            &snapshot.directory,
            PENDING,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::empty(),
            CONFINED,
        )
        .map_err(|e| io("recheck pending store snapshot", e))?;
        regular(&pending)?;

        if identity(&pending)? != pending_identity || snapshot.read()?.as_deref() != expected {
            return Err(Failure::Unsafe("store snapshot changed during publication").into());
        }

        renameat(
            &snapshot.directory,
            PENDING,
            &snapshot.directory,
            DATABASE_FILE,
        )
        .map_err(|e| io("publish store snapshot", e))
    }
}

#[cfg(test)]
mod tests;
