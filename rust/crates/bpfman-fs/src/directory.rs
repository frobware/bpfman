use std::{
    fs::File,
    os::fd::{AsFd, OwnedFd},
    path::{Component, PathBuf},
};

use bpfman_lock::{AcquireOptions, with_write_lock_file};
use rustix::fs::{FileType, Mode, OFlags, ResolveFlags, fstat, mkdirat, open, openat2};

use crate::{
    Error, RuntimeDirectory, RuntimeLayout, RuntimeWriter,
    error::{Failure, io},
    layout::{DATABASE_DIRECTORY, LOCK_FILE},
};

pub(super) const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::CLOEXEC);
pub(super) const BENEATH: ResolveFlags = ResolveFlags::BENEATH.union(ResolveFlags::NO_SYMLINKS);
pub(super) const CONFINED: ResolveFlags = BENEATH.union(ResolveFlags::NO_XDEV);

impl RuntimeDirectory {
    /// Adopt an existing root without creating directories or acquiring a lock.
    /// Only absence returns None; symlinks, inaccessible paths and root aliases fail.
    pub fn open_existing(layout: RuntimeLayout) -> Result<Option<Self>, Error> {
        let filesystem =
            open("/", DIRECTORY, Mode::empty()).map_err(|e| io("open filesystem anchor", e))?;
        let relative = layout
            .root()
            .strip_prefix("/")
            .map_err(|_| Failure::Unsafe("root must be absolute"))?;
        let root = match openat2(&filesystem, relative, DIRECTORY, Mode::empty(), BENEATH) {
            Ok(root) => root,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(io("adopt existing runtime directory", e)),
        };
        let anchor = fstat(&filesystem).map_err(|e| io("inspect filesystem anchor", e))?;
        let adopted = fstat(&root).map_err(|e| io("inspect runtime directory", e))?;

        if (anchor.st_dev, anchor.st_ino) == (adopted.st_dev, adopted.st_ino) {
            return Err(Failure::Unsafe("filesystem root cannot be adopted").into());
        }

        Ok(Some(Self { root, layout }))
    }

    /// Validated path vocabulary for read-only adapters and presentation.
    pub fn layout(&self) -> &RuntimeLayout {
        &self.layout
    }

    /// Descriptor identity, without granting mutation authority.
    pub fn identity(&self) -> Result<crate::RuntimeIdentity, Error> {
        crate::artifacts::identity(&self.root).map(crate::RuntimeIdentity)
    }

    /// Open or create the configured root without following symlinks.
    ///
    /// Ancestors may cross mounts (for example `/run`); descendants may not.
    /// Requires Linux openat2: unsupported kernels fail closed, with no weaker
    /// path-based fallback. Existing names are never replaced or removed.
    pub fn open_or_create(layout: RuntimeLayout) -> Result<Self, Error> {
        let filesystem =
            open("/", DIRECTORY, Mode::empty()).map_err(|e| io("open filesystem anchor", e))?;
        let mut parent = rustix::io::fcntl_dupfd_cloexec(&filesystem, 0)
            .map_err(|e| io("duplicate filesystem anchor", e))?;

        for component in layout.root().components() {
            if let Component::Normal(name) = component {
                parent = ensure_directory(&parent, name, BENEATH)?;
            }
        }

        // Re-resolve the full supplied path at adoption, refusing any ancestor
        // replaced by a symlink during creation. After adoption, the descriptor
        // (not the pathname) defines the root's identity.
        let relative = layout
            .root()
            .strip_prefix("/")
            .map_err(|_| Failure::Unsafe("root must be absolute"))?;
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
    /// The lock identity is rechecked after acquisition; participants must not
    /// replace it during the callback. This is cooperative locking, not a sandbox.
    pub fn with_writer<T>(
        &self,
        options: AcquireOptions<'_>,
        work: impl for<'scope> FnOnce(RuntimeWriter<'scope>) -> T,
    ) -> Result<T, Error> {
        let fd = openat2(
            &self.root,
            LOCK_FILE,
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NONBLOCK,
            Mode::from_raw_mode(0o600),
            CONFINED,
        )
        .map_err(|e| io("open runtime writer lock", e))?;
        self.with_lock_file(fd, options, work)
    }

    fn with_lock_file<T>(
        &self,
        fd: OwnedFd,
        options: AcquireOptions<'_>,
        work: impl for<'scope> FnOnce(RuntimeWriter<'scope>) -> T,
    ) -> Result<T, Error> {
        let stat = fstat(&fd).map_err(|e| io("inspect runtime writer lock", e))?;

        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
            return Err(Failure::Unsafe("writer lock must be a singly linked regular file").into());
        }

        with_write_lock_file(
            File::from(fd),
            &self.layout.lock_path(),
            options,
            |permit| {
                // A waiter may have opened the old inode before .lock was
                // replaced. Do not lend authority for a now-unrelated lock.
                let current = openat2(
                    &self.root,
                    LOCK_FILE,
                    OFlags::PATH | OFlags::CLOEXEC,
                    Mode::empty(),
                    CONFINED,
                )
                .map_err(|e| io("recheck runtime writer lock", e))?;
                let current = fstat(&current).map_err(|e| io("inspect acquired writer lock", e))?;

                if (stat.st_dev, stat.st_ino) != (current.st_dev, current.st_ino)
                    || current.st_nlink != 1
                {
                    return Err(Failure::Unsafe("writer lock changed during acquisition").into());
                }

                let writer = RuntimeWriter {
                    runtime: self,
                    _permit: permit,
                };
                writer.prepare_database_directory()?;

                Ok(work(writer))
            },
        )
        .map_err(Failure::Lock)?
    }
}

impl RuntimeWriter<'_> {
    /// Borrow the adopted runtime for observations that need no writer authority.
    pub fn directory(&self) -> &RuntimeDirectory {
        self.runtime
    }

    /// Validated path vocabulary for persistence and presentation.
    pub fn layout(&self) -> &RuntimeLayout {
        &self.runtime.layout
    }

    fn prepare_database_directory(&self) -> Result<(), Error> {
        ensure_directory(&self.runtime.root, DATABASE_DIRECTORY, CONFINED)?;

        Ok(())
    }

    /// Go-compatible database filename for the separate rusqlite adapter.
    ///
    /// This is deliberately a pathname handoff, not a confined managed-object
    /// capability. SQLite owns its file access and auxiliary-file lifecycle.
    pub fn database_path(&self) -> PathBuf {
        self.runtime.layout.database_path()
    }
}

pub(super) fn ensure_directory(
    root: impl AsFd,
    name: impl AsRef<std::ffi::OsStr>,
    resolve: ResolveFlags,
) -> Result<OwnedFd, Error> {
    let name = name.as_ref();

    match openat2(&root, name, DIRECTORY, Mode::empty(), resolve) {
        Ok(fd) => return Ok(fd),
        Err(rustix::io::Errno::NOENT) => {}
        Err(error) => return Err(io("open runtime directory component", error)),
    }

    match mkdirat(&root, name, Mode::from_raw_mode(0o755)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(error) => return Err(io("create runtime directory component", error)),
    }

    openat2(root, name, DIRECTORY, Mode::empty(), resolve)
        .map_err(|e| io("verify runtime directory component", e))
}

impl crate::RuntimeWriter<'_> {
    /// Identity of the descriptor adopted by this writer, independent of path spelling.
    pub fn identity(&self) -> Result<crate::RuntimeIdentity, crate::Error> {
        self.runtime.identity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::symlink, time::Duration};

    fn options() -> AcquireOptions<'static> {
        AcquireOptions {
            timeout: Duration::from_millis(25),
            cancelled: None,
        }
    }

    #[test]
    fn replacement_of_root_path_does_not_redirect_filesystem_operations()
    -> Result<(), Box<dyn std::error::Error>> {
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
        let error = openat2(&root, "proc", DIRECTORY, Mode::empty(), CONFINED)
            .expect_err("reject mount crossing");

        assert_eq!(error, rustix::io::Errno::XDEV);

        Ok(())
    }

    #[test]
    fn stale_lock_descriptor_cannot_lend_writer_authority() -> Result<(), Box<dyn std::error::Error>>
    {
        let temporary = tempfile::tempdir()?;
        let layout = RuntimeLayout::try_from(temporary.path().to_owned())?;
        let path = layout.lock_path();
        let runtime = RuntimeDirectory::open_or_create(layout)?;

        // Deterministic handoff: this is the fd a waiter opened before flock.
        let old = openat2(
            &runtime.root,
            LOCK_FILE,
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
            CONFINED,
        )?;
        std::fs::rename(&path, temporary.path().join("old-lock"))?;
        std::fs::write(&path, b"replacement")?;
        let called = std::cell::Cell::new(false);
        let error = runtime
            .with_lock_file(old, options(), |_| called.set(true))
            .expect_err("lock inode was replaced");

        assert_eq!(error.kind(), crate::ErrorKind::UnsafeLayout);
        assert!(!called.get());
        assert!(!temporary.path().join(DATABASE_DIRECTORY).exists());
        assert_eq!(std::fs::read(path)?, b"replacement");

        Ok(())
    }
}
