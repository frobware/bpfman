use std::{fs::File, os::fd::{AsFd, OwnedFd}, path::{Component, PathBuf}};

use bpfman_lock::{AcquireOptions, with_write_lock_file};
use rustix::fs::{FileType, Mode, OFlags, ResolveFlags, fstat, mkdirat, open, openat2};

use crate::{RuntimeDirectory, RuntimeLayout, RuntimeWriter, Error, error::{Failure, io}, layout::{DATABASE_DIRECTORY, LOCK_FILE}};

const DIRECTORY: OFlags = OFlags::RDONLY.union(OFlags::DIRECTORY).union(OFlags::CLOEXEC);
const BENEATH: ResolveFlags = ResolveFlags::BENEATH.union(ResolveFlags::NO_SYMLINKS);
const CONFINED: ResolveFlags = BENEATH.union(ResolveFlags::NO_XDEV);

impl RuntimeDirectory {
    /// Open or create the configured root without following symlinks.
    ///
    /// Ancestors may cross mounts (for example `/run`); descendants may not.
    /// Requires Linux openat2: unsupported kernels fail closed, with no weaker
    /// path-based fallback. Existing names are never replaced or removed.
    pub fn open_or_create(layout: RuntimeLayout) -> Result<Self, Error> {
        let filesystem = open("/", DIRECTORY, Mode::empty()).map_err(|e| io("open filesystem anchor", e))?;
        let mut parent = rustix::io::dup(&filesystem).map_err(|e| io("duplicate filesystem anchor", e))?;
        for component in layout.root().components() {
            if let Component::Normal(name) = component {
                parent = ensure_directory(&parent, name, BENEATH)?;
            }
        }
        // Re-resolve the full supplied path at adoption, refusing any ancestor
        // replaced by a symlink during creation. After adoption, the descriptor
        // (not the pathname) defines the root's identity.
        let relative = layout.root().strip_prefix("/").map_err(|_| Failure::Unsafe("root must be absolute"))?;
        let root = openat2(&filesystem, relative, DIRECTORY, Mode::empty(), BENEATH)
            .map_err(|e| io("adopt runtime directory", e))?;
        let anchor = fstat(&filesystem).map_err(|e| io("inspect filesystem anchor", e))?;
        let adopted = fstat(&root).map_err(|e| io("inspect runtime directory", e))?;
        if (anchor.st_dev, anchor.st_ino) == (adopted.st_dev, adopted.st_ino) {
            return Err(Failure::Unsafe("filesystem root cannot be adopted").into());
        }
        Ok(Self { root, layout })
    }

    /// Borrow write authority for this root, with a Go-compatible flock.
    ///
    /// The lock file must be a regular, singly linked file. The `db` directory
    /// is prepared under the lock. No API accepts another runtime's permit.
    pub fn with_writer<T>(&self, options: AcquireOptions<'_>, work: impl for<'scope> FnOnce(RuntimeWriter<'scope>) -> T) -> Result<T, Error> {
        let fd = openat2(&self.root, LOCK_FILE,
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::from_raw_mode(0o600), CONFINED).map_err(|e| io("open runtime writer lock", e))?;
        let stat = fstat(&fd).map_err(|e| io("inspect runtime writer lock", e))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
            return Err(Failure::Unsafe("writer lock must be a singly linked regular file").into());
        }
        with_write_lock_file(File::from(fd), &self.layout.lock_path(), options, |permit| {
            ensure_directory(&self.root, DATABASE_DIRECTORY, CONFINED)?;
            Ok(work(RuntimeWriter { runtime: self, _permit: permit }))
        }).map_err(Failure::Lock)?
    }
}

impl RuntimeWriter<'_> {
    /// Go-compatible database filename for the separate rusqlite adapter.
    ///
    /// This is deliberately a pathname handoff, not a confined managed-object
    /// capability. SQLite owns its file access and auxiliary-file lifecycle.
    pub fn database_path(&self) -> PathBuf { self.runtime.layout.database_path() }
}

fn ensure_directory(root: impl AsFd, name: impl AsRef<std::ffi::OsStr>, resolve: ResolveFlags) -> Result<OwnedFd, Error> {
    let name = name.as_ref();
    match openat2(&root, name, DIRECTORY, Mode::empty(), resolve) {
        Ok(fd) => return Ok(fd),
        Err(rustix::io::Errno::NOENT) => {},
        Err(error) => return Err(io("open runtime directory component", error)),
    }
    match mkdirat(&root, name, Mode::from_raw_mode(0o755)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {},
        Err(error) => return Err(io("create runtime directory component", error)),
    }
    openat2(root, name, DIRECTORY, Mode::empty(), resolve).map_err(|e| io("verify runtime directory component", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::symlink, time::Duration};

    fn options() -> AcquireOptions<'static> { AcquireOptions { timeout: Duration::from_millis(25), cancelled: None } }

    #[test]
    fn replacement_of_root_path_does_not_redirect_filesystem_operations() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let path = temporary.path().join("runtime");
        let runtime = RuntimeDirectory::open_or_create(RuntimeLayout::try_from(path.clone())?)?;
        let adopted = temporary.path().join("adopted");
        std::fs::rename(&path, &adopted)?;
        let outside = temporary.path().join("outside");
        std::fs::create_dir(&outside)?;
        symlink(&outside, &path)?;
        runtime.with_writer(options(), |_| ())?;
        assert!(adopted.join(LOCK_FILE).is_file());
        assert!(adopted.join(DATABASE_DIRECTORY).is_dir());
        assert_eq!(std::fs::read_dir(outside)?.count(), 0);
        Ok(())
    }

    #[test]
    fn mount_crossing_is_rejected_by_kernel_resolution() -> Result<(), Box<dyn std::error::Error>> {
        let root = open("/", DIRECTORY, Mode::empty())?;
        // /proc is an existing separate mount; this test only attempts a read-only open.
        let error = openat2(&root, "proc", DIRECTORY, Mode::empty(), CONFINED).expect_err("reject mount crossing");
        assert_eq!(error, rustix::io::Errno::XDEV);
        Ok(())
    }
}
