//! Read-only operator trust input, separate from managed cache artifacts.
use crate::{Error, error::io};
use std::{fs::File, io::Read, path::Path};

/// Open an operator-managed Sigstore trust root without blocking on special files.
/// Symlinks are allowed; the opened descriptor must refer to a regular file.
/// Missing files are errors. Callers bound reads and implement cancellation and
/// format policy. The reader confers no mutation or cleanup authority.
pub fn open_sigstore_trust_root(path: &Path) -> Result<impl Read, Error> {
    let fd = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK,
        rustix::fs::Mode::empty(),
    )
    .map_err(|e| io("open Sigstore trust root file", e))?;
    let file = File::from(fd);
    if !file
        .metadata()
        .map_err(|e| io("inspect Sigstore trust root file", e))?
        .is_file()
    {
        return Err(io(
            "inspect Sigstore trust root file",
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Sigstore trust root file must be regular",
            ),
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_special_files_and_retains_opened_symlink_target()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let fifo = temp.path().join("fifo");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::RUSR,
            0,
        )?;
        assert!(open_sigstore_trust_root(&fifo).is_err());
        assert!(open_sigstore_trust_root(temp.path()).is_err());
        assert!(open_sigstore_trust_root(&temp.path().join("missing")).is_err());

        let original = temp.path().join("original");
        let replacement = temp.path().join("replacement");
        let link = temp.path().join("root");
        std::fs::write(&original, "original")?;
        std::fs::write(&replacement, "replacement")?;
        std::os::unix::fs::symlink(&original, &link)?;
        let mut reader = open_sigstore_trust_root(&link)?;
        std::fs::rename(&replacement, &original)?;
        let mut bytes = String::new();
        reader.read_to_string(&mut bytes)?;
        assert_eq!(bytes, "original");
        Ok(())
    }
}
