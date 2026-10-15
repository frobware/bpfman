//! Read immutable published bytecode beneath the retained runtime root.
use crate::{
    Error, RuntimeWriter,
    directory::CONFINED,
    error::{Failure, io},
};
use std::{fs::File, io::Read, num::NonZeroU32};

const MAX_BYTES: u64 = 64 * 1024 * 1024;

impl RuntimeWriter<'_> {
    /// Read optional published provenance beneath the retained runtime root.
    /// Missing files allow adoption of older Go-created artifacts; malformed or
    /// replaced files remain errors. The returned bytes confer no authority.
    pub fn read_provenance(&self, program: NonZeroU32) -> Result<Option<Vec<u8>>, Error> {
        let fd = match rustix::fs::openat2(
            &self.runtime.root,
            format!("programs/{program}/provenance.json"),
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
            CONFINED,
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(io("open managed provenance", e)),
        };
        let stat = rustix::fs::fstat(&fd).map_err(|e| io("inspect managed provenance", e))?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile
            || stat.st_nlink != 1
            || stat.st_size > MAX_BYTES as i64
        {
            return Err(Failure::Unsafe("invalid managed provenance file").into());
        }
        let mut bytes = Vec::new();
        File::from(fd)
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io("read managed provenance", e))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(Failure::Unsafe("managed provenance exceeds size limit").into());
        }
        Ok(Some(bytes))
    }

    /// Read a managed program's published ELF without following symlinks,
    /// crossing mounts, or accepting special files or extra hard links.
    pub fn read_bytecode(&self, program: NonZeroU32) -> Result<Vec<u8>, Error> {
        let fd = rustix::fs::openat2(
            &self.runtime.root,
            format!("programs/{program}/bytecode.o"),
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
            CONFINED,
        )
        .map_err(|e| io("open managed bytecode", e))?;
        let stat = rustix::fs::fstat(&fd).map_err(|e| io("inspect managed bytecode", e))?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile
            || stat.st_nlink != 1
        {
            return Err(
                Failure::Unsafe("managed bytecode must be a singly linked regular file").into(),
            );
        }
        if stat.st_size > MAX_BYTES as i64 {
            return Err(Failure::Unsafe("managed bytecode exceeds size limit").into());
        }
        let mut bytes = Vec::new();
        File::from(fd)
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io("read managed bytecode", e))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(Failure::Unsafe("managed bytecode exceeds size limit").into());
        }
        Ok(bytes)
    }
}
