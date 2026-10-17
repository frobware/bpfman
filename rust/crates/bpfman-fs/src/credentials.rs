//! Read-only operator credential input, separate from managed runtime artifacts.
use crate::{Error, error::io};
use std::{fs::File, io::Read, path::Path};

/// Open an optional registry credential file without blocking on special files.
/// Ordinary operator-managed symlinks are allowed; the opened descriptor must
/// refer to a regular file. The reader confers no mutation or cleanup authority.
/// Callers bound reads and implement their own cancellation and format policy.
pub fn open_registry_credentials(path: &Path) -> Result<Option<impl Read>, Error> {
    let fd = match rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(cause) => return Err(io("open registry credential file", cause)),
    };
    let file = File::from(fd);
    let metadata = file
        .metadata()
        .map_err(|e| io("inspect registry credential file", e))?;
    if !metadata.is_file() {
        return Err(io(
            "inspect registry credential file",
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "registry credential file must be regular",
            ),
        ));
    }
    Ok(Some(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_fifo_and_directory_without_blocking() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let fifo = temp.path().join("fifo");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::RUSR,
            0,
        )?;
        assert!(open_registry_credentials(&fifo).is_err());
        assert!(open_registry_credentials(temp.path()).is_err());
        assert!(open_registry_credentials(&temp.path().join("missing"))?.is_none());
        Ok(())
    }
}
