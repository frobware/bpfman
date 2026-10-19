//! Daemon endpoint publication under a separate, lifetime-scoped socket lock.
use crate::{
    Error, RuntimeDirectory, RuntimeLayout, RuntimeWriter,
    artifacts::{Identity, identity, proc_path},
    directory::{CONFINED, DIRECTORY},
    error::{Failure, io},
};
use rustix::fs::{
    FileType, Mode, OFlags, RenameFlags, chmod, fstat, mkdirat, openat2, renameat_with,
};
use std::{
    ffi::OsString,
    os::fd::OwnedFd,
    path::{Path, PathBuf},
};

/// Confined endpoint parent and a private staging/lock directory.
/// The lock is independent of the program writer, and lives for one server run.
pub struct SocketDirectory {
    parent: RuntimeDirectory,
    private: RuntimeDirectory,
    name: OsString,
}

/// Borrowed endpoint authority. The transport only sees a private staging path;
/// its unconditional unlink-on-drop cannot remove the published endpoint.
pub struct SocketBinding<'a> {
    writer: &'a RuntimeWriter<'a>,
    parent: &'a RuntimeDirectory,
    name: &'a std::ffi::OsStr,
    path: PathBuf,
    published: Option<Identity>,
}

impl SocketDirectory {
    /// Create/adopt a socket directory without following symlinks. The endpoint
    /// must be absolute, have a non-root parent, and name one ordinary component.
    /// The persistent private directory is restricted to its owner's access.
    pub fn open(path: &Path) -> Result<Self, Error> {
        let name = path
            .file_name()
            .filter(|_| path.is_absolute())
            .ok_or(Failure::Unsafe("socket requires an absolute file path"))?
            .to_owned();
        let layout = RuntimeLayout::try_from(
            path.parent()
                .ok_or(Failure::Unsafe("socket parent missing"))?
                .to_owned(),
        )
        .map_err(|_| Failure::Unsafe("invalid socket parent"))?;
        let parent = RuntimeDirectory::open_or_create(layout)?;
        let mut private_name = OsString::from(".bpfman-");
        private_name.push(&name);
        match mkdirat(&parent.root, &private_name, Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(e) => return Err(io("create private socket directory", e)),
        }
        let root = openat2(
            &parent.root,
            &private_name,
            DIRECTORY,
            Mode::empty(),
            CONFINED,
        )
        .map_err(|e| io("open private socket directory", e))?;
        let stat = fstat(&root).map_err(|e| io("inspect private socket directory", e))?;
        if stat.st_mode & 0o777 != 0o700 || stat.st_uid != rustix::process::geteuid().as_raw() {
            return Err(Failure::Unsafe(
                "private socket directory must be owned by the daemon user with mode 0700",
            )
            .into());
        }
        let private = RuntimeDirectory {
            root,
            layout: RuntimeLayout::try_from(parent.layout.root().join(private_name))
                .map_err(|_| Failure::Unsafe("invalid private socket directory"))?,
        };
        Ok(Self {
            parent,
            private,
            name,
        })
    }

    /// Hold endpoint authority for the whole callback, then remove only the
    /// published socket's captured inode. Return callback and cleanup outcomes
    /// separately so neither an operation failure nor a cleanup failure is lost.
    /// Persistent lock/staging directories are deliberately retained for reuse.
    pub fn with_socket<T>(
        &self,
        options: bpfman_lock::AcquireOptions<'_>,
        work: impl FnOnce(&mut SocketBinding<'_>) -> T,
    ) -> Result<(T, Result<(), Error>), Error> {
        self.private.with_cache_writer(options, |writer| {
            clear_stale(&self.private.root, std::ffi::OsStr::new("listener"))?;
            let mut binding = SocketBinding {
                path: proc_path(&self.private.root).join("listener"),
                writer: &writer,
                parent: &self.parent,
                name: &self.name,
                published: None,
            };
            let result = work(&mut binding);
            let cleanup = if let Some(expected) = binding.published {
                crate::removal::remove_socket(&self.parent.root, &self.name, expected)
            } else {
                Ok(())
            };
            Ok((result, cleanup))
        })?
    }
}

impl SocketBinding<'_> {
    /// Private path for binding. Its opened parent remains alive throughout serving.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Set Go-compatible permissions and atomically publish the bound socket.
    /// Refuse live endpoints and all non-socket entries. Only a socket returning
    /// ECONNREFUSED is treated as stale; other connection errors fail closed.
    pub fn publish(&mut self) -> Result<(), Error> {
        if self.published.is_some() {
            return Err(Failure::Unsafe("socket already published").into());
        }
        let private = &self.writer.runtime.root;
        let socket = open_socket(private, std::ffi::OsStr::new("listener"))?;
        let expected = identity(&socket)?;
        // chmod the captured O_PATH inode through procfs, avoiding a second
        // lookup of the listener name (fchmod cannot accept an O_PATH fd).
        chmod(proc_path(&socket), Mode::from_raw_mode(0o660))
            .map_err(|e| io("set gRPC socket permissions", e))?;
        clear_stale(&self.parent.root, self.name)?;
        renameat_with(
            private,
            "listener",
            &self.parent.root,
            self.name,
            RenameFlags::NOREPLACE,
        )
        .map_err(|e| io("publish gRPC socket", e))?;
        self.published = Some(expected);
        Ok(())
    }
}

pub(super) fn open_socket(parent: &OwnedFd, name: &std::ffi::OsStr) -> Result<OwnedFd, Error> {
    let fd = openat2(
        parent,
        name,
        OFlags::PATH | OFlags::CLOEXEC,
        Mode::empty(),
        CONFINED,
    )
    .map_err(|e| io("open socket entry", e))?;
    let stat = fstat(&fd).map_err(|e| io("inspect socket entry", e))?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::Socket || stat.st_nlink != 1 {
        return Err(Failure::Unsafe("endpoint must be a singly linked socket").into());
    }
    Ok(fd)
}

fn clear_stale(parent: &OwnedFd, name: &std::ffi::OsStr) -> Result<(), Error> {
    let fd = match openat2(
        parent,
        name,
        OFlags::PATH | OFlags::CLOEXEC,
        Mode::empty(),
        CONFINED,
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(e) => return Err(io("inspect existing socket", e)),
    };
    let expected = identity(&fd)?;
    let _socket = open_socket(parent, name)?;
    let address = rustix::net::SocketAddrUnix::new(proc_path(parent).join(name))
        .map_err(|e| io("prepare socket probe address", e))?;
    let probe = rustix::net::socket_with(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::STREAM,
        rustix::net::SocketFlags::NONBLOCK | rustix::net::SocketFlags::CLOEXEC,
        None,
    )
    .map_err(|e| io("create socket probe", e))?;
    match rustix::net::connect(&probe, &address) {
        Err(e) if e == rustix::io::Errno::CONNREFUSED => {
            crate::removal::remove_socket(parent, name, expected)
        }
        Ok(_) => Err(Failure::Unsafe("gRPC socket is already listening").into()),
        Err(e) => Err(io("probe existing socket", e)),
    }
}
