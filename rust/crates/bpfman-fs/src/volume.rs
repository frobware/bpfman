//! Descriptor-confined CSI mounts. Mounts are durable state; operation receipts
//! retain failed rollback and can be adopted again after daemon restart.
use crate::{
    Error, MapPinning, PinSource, PinTarget, RuntimeWriter,
    artifacts::{BPF_SUPER_MAGIC, Identity, identity, proc_path},
    directory::{BENEATH, CONFINED, DIRECTORY, ensure_directory},
    error::{Failure, io},
};
use rustix::fs::{Mode, OFlags, fstatfs, open, openat2};
use std::{
    os::fd::OwnedFd,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

/// Map reopening needed for publishing an additional pin, without sharing Aya types.
pub trait MapReopening {
    /// Owned live map reference, released after the pin syscall.
    type Map: MapPinning;
    /// Open a previously confined map pin.
    fn open_map(&self, source: PinSource<'_>) -> crate::KernelResult<Self::Map>;
}

/// Validated CSI publish intent, independent of protobuf and persistence.
#[derive(Debug)]
pub struct VolumeRequest {
    volume: String,
    target: PathBuf,
    maps: Vec<String>,
    readonly: bool,
    group: Option<u32>,
}

fn element(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".." && !value.contains(['/', '\0'])
}

impl VolumeRequest {
    /// Validate a CSI volume identity and normalize its absolute non-root target.
    pub fn validate_target(volume: &str, target: &Path) -> Result<(), Error> {
        if !element(volume) {
            return Err(Failure::Unsafe("volume ID must be a single path element").into());
        }
        crate::RuntimeLayout::try_from(target.to_path_buf())
            .map_err(|_| Failure::Unsafe("target must be an absolute non-root path"))?;
        Ok(())
    }

    /// Parse comma-separated map names, trimming and deduplicating like Go.
    pub fn new(
        volume: &str,
        target: &Path,
        maps: &str,
        readonly: bool,
        group: Option<u32>,
    ) -> Result<Self, Error> {
        Self::validate_target(volume, target)?;
        let mut names = Vec::new();
        for name in maps.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if !element(name) {
                return Err(Failure::Unsafe("map name must be a single path element").into());
            }
            if !names.iter().any(|n| n == name) {
                names.push(name.to_owned());
            }
        }
        if names.is_empty() {
            return Err(Failure::Unsafe("at least one map is required").into());
        }
        Ok(Self {
            volume: volume.into(),
            target: crate::RuntimeLayout::try_from(target.to_path_buf())
                .map_err(|_| Error::from(Failure::Unsafe("invalid volume target")))?
                .root()
                .into(),
            maps: names,
            readonly,
            group,
        })
    }

    /// Volume identity used for operation serialization and per-pod mounts.
    pub fn volume(&self) -> &str {
        &self.volume
    }

    /// Normalized mount target.
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// Requested map pin names, in first-seen order.
    pub fn maps(&self) -> &[String] {
        &self.maps
    }
}

/// Reopened target observation for restart-safe publish idempotency.
#[derive(Debug, Eq, PartialEq)]
pub enum VolumeState {
    /// No mount exists at the target.
    Absent,
    /// Same map names and read-only flag.
    Matches,
    /// A bpffs mount exists with different arguments.
    Different,
}

#[derive(Debug)]
pub(super) struct VolumeDirectory {
    pub(super) parent: OwnedFd,
    pub(super) name: std::ffi::OsString,
    pub(super) fd: Option<OwnedFd>,
    pub(super) identity: Identity,
    pub(super) mounted: bool,
}

impl VolumeDirectory {
    fn adopt(path: &Path, create: bool) -> Result<Option<Box<Self>>, Error> {
        let layout = crate::RuntimeLayout::try_from(path.to_path_buf())
            .map_err(|_| Failure::Unsafe("invalid volume directory"))?;
        let path = layout.root();
        let parent_path = path
            .parent()
            .ok_or(Failure::Unsafe("volume has no parent"))?;
        let root =
            open("/", DIRECTORY, Mode::empty()).map_err(|e| io("open filesystem root", e))?;
        let parent = if create {
            let mut fd =
                rustix::io::fcntl_dupfd_cloexec(&root, 0).map_err(|e| io("retain root", e))?;
            for part in parent_path.components() {
                if let std::path::Component::Normal(n) = part {
                    fd = ensure_directory(&fd, n, BENEATH)?;
                }
            }
            fd
        } else {
            let relative = parent_path
                .strip_prefix("/")
                .map_err(|_| Failure::Unsafe("invalid parent"))?;
            match openat2(
                &root,
                if relative.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    relative
                },
                DIRECTORY,
                Mode::empty(),
                BENEATH,
            ) {
                Ok(fd) => fd,
                Err(rustix::io::Errno::NOENT) => return Ok(None),
                Err(e) => return Err(io("adopt volume parent", e)),
            }
        };
        let name = path
            .file_name()
            .ok_or(Failure::Unsafe("volume has no name"))?
            .to_owned();
        let fd = if create {
            ensure_directory(&parent, &name, BENEATH)?
        } else {
            match openat2(&parent, &name, DIRECTORY, Mode::empty(), BENEATH) {
                Ok(fd) => fd,
                Err(rustix::io::Errno::NOENT) => return Ok(None),
                Err(e) => return Err(io("adopt volume directory", e)),
            }
        };
        let identity = identity(&fd)?;
        let mounted = mount_id(&fd)? != mount_id(&parent)?;
        Ok(Some(Box::new(Self {
            parent,
            name,
            fd: Some(fd),
            identity,
            mounted,
        })))
    }

    fn backing(
        writer: &RuntimeWriter<'_>,
        volume: &str,
        create: bool,
    ) -> Result<Option<Box<Self>>, Error> {
        let mut parent = rustix::io::fcntl_dupfd_cloexec(&writer.runtime.root, 0)
            .map_err(|e| io("retain CSI runtime root", e))?;
        for name in ["csi", "fs"] {
            parent = if create {
                ensure_directory(&parent, name, CONFINED)?
            } else {
                match openat2(&parent, name, DIRECTORY, Mode::empty(), CONFINED) {
                    Ok(fd) => fd,
                    Err(rustix::io::Errno::NOENT) => return Ok(None),
                    Err(e) => return Err(io("adopt CSI collection", e)),
                }
            };
        }
        let fd = if create {
            ensure_directory(&parent, volume, BENEATH)?
        } else {
            match openat2(&parent, volume, DIRECTORY, Mode::empty(), BENEATH) {
                Ok(fd) => fd,
                Err(rustix::io::Errno::NOENT) => return Ok(None),
                Err(e) => return Err(io("adopt CSI backing", e)),
            }
        };
        let identity = identity(&fd)?;
        let mounted = mount_id(&fd)? != mount_id(&parent)?;
        Ok(Some(Box::new(Self {
            parent,
            fd: Some(fd),
            name: volume.into(),
            identity,
            mounted,
        })))
    }

    pub(super) fn check(&self) -> Result<(), Error> {
        let current = openat2(&self.parent, &self.name, DIRECTORY, Mode::empty(), BENEATH)
            .map_err(|e| io("recheck volume directory", e))?;
        if identity(&current)? != self.identity {
            return Err(Failure::Unsafe("volume directory was replaced").into());
        }
        Ok(())
    }

    pub(super) fn fd(&self) -> Result<&OwnedFd, Error> {
        self.fd
            .as_ref()
            .ok_or_else(|| Failure::Unsafe("volume descriptor requires readoption").into())
    }

    pub(super) fn reopen(&mut self) -> Result<(), Error> {
        let fd = openat2(&self.parent, &self.name, DIRECTORY, Mode::empty(), BENEATH)
            .map_err(|e| io("reopen volume directory", e))?;
        if identity(&fd)? != self.identity {
            return Err(Failure::Unsafe("volume directory was replaced").into());
        }
        self.fd = Some(fd);
        Ok(())
    }

    pub(super) fn refresh(&mut self) -> Result<(), Error> {
        let fd = openat2(&self.parent, &self.name, DIRECTORY, Mode::empty(), BENEATH)
            .map_err(|e| io("adopt volume mount change", e))?;
        self.identity = identity(&fd)?;
        self.mounted = mount_id(&fd)? != mount_id(&self.parent)?;
        self.fd = Some(fd);
        Ok(())
    }

    pub(super) fn require_bpf(&self) -> Result<(), Error> {
        if self.mounted
            && fstatfs(self.fd()?)
                .map_err(|e| io("inspect volume filesystem", e))?
                .f_type
                != BPF_SUPER_MAGIC
        {
            return Err(
                Failure::Unsafe("volume target is mounted with a foreign filesystem").into(),
            );
        }
        Ok(())
    }
}

fn mount_id(fd: &OwnedFd) -> Result<u64, Error> {
    let stat = rustix::fs::statx(
        fd,
        "",
        rustix::fs::AtFlags::EMPTY_PATH,
        rustix::fs::StatxFlags::MNT_ID,
    )
    .map_err(|e| io("inspect volume mount identity", e))?;
    if stat.stx_mask & rustix::fs::StatxFlags::MNT_ID.bits() == 0 {
        return Err(Failure::Unsafe("kernel did not report mount identity").into());
    }
    Ok(stat.stx_mnt_id)
}

/// Owned pending CSI mount cleanup. Never cloneable or automatically retried.
#[derive(Debug)]
pub struct VolumeCleanup {
    root: Identity,
    target_path: PathBuf,
    target: Option<Box<VolumeDirectory>>,
    remove_target: bool,
    backing: Option<Box<VolumeDirectory>>,
}

impl VolumeCleanup {
    /// Whether a repeated teardown request names this receipt's normalized target.
    /// Matching grants no authority; cleanup still requires the original writer root.
    pub fn matches_target(&self, target: &Path) -> bool {
        crate::RuntimeLayout::try_from(target.to_path_buf())
            .is_ok_and(|layout| layout.root() == self.target_path)
    }
}

/// Failed forward operation and every cleanup error from its one rollback pass.
#[derive(Debug)]
pub struct VolumeFailure {
    /// Primary forward or teardown error.
    pub cause: std::sync::Arc<Error>,
    /// Errors encountered while compensating independent acquisitions.
    pub cleanup: Vec<Error>,
    /// Remaining ownership, if cleanup could not complete.
    pub pending: Option<VolumeCleanup>,
}

impl std::fmt::Display for VolumeFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "volume operation failed with {} cleanup failure(s)",
            self.cleanup.len()
        )
    }
}

impl std::error::Error for VolumeFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.cause.as_ref())
    }
}

impl From<Error> for VolumeFailure {
    fn from(cause: Error) -> Self {
        Self {
            cause: cause.into(),
            cleanup: Vec::new(),
            pending: None,
        }
    }
}

fn cancelled(flag: &AtomicBool) -> Result<(), Error> {
    if flag.load(Ordering::Acquire) {
        return Err(io(
            "CSI operation cancelled",
            std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"),
        ));
    }
    Ok(())
}

impl RuntimeWriter<'_> {
    /// Observe an existing target using its mount and map names, without remembered requests.
    pub fn volume_state(&self, request: &VolumeRequest) -> Result<VolumeState, Error> {
        let Some(target) = VolumeDirectory::adopt(request.target(), false)? else {
            return Ok(VolumeState::Absent);
        };
        target.require_bpf()?;
        if !target.mounted {
            return Ok(VolumeState::Absent);
        }
        let flags = fstatfs(target.fd()?).map_err(|e| io("inspect published flags", e))?;
        let readonly = flags.f_flags & 1 != 0;
        let mut names = std::fs::read_dir(proc_path(target.fd()?))
            .map_err(|e| io("list published maps", e))?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| io("list published maps", e))?;
        names.retain(|n| n != "maps.debug" && n != "progs.debug");
        names.sort();
        let mut requested: Vec<_> = request.maps.iter().map(std::ffi::OsString::from).collect();
        requested.sort();
        Ok(if readonly == request.readonly && names == requested {
            VolumeState::Matches
        } else {
            VolumeState::Different
        })
    }

    /// Publish a per-volume bpffs and bind mount. All acquired mounts are
    /// compensated once on failure; unresolved ownership is returned explicitly.
    pub fn publish_volume(
        &self,
        request: &VolumeRequest,
        map_set: std::num::NonZeroU32,
        kernel: &impl MapReopening,
        cancel: &AtomicBool,
    ) -> Result<(), VolumeFailure> {
        cancelled(cancel)?;
        let mut owned = VolumeCleanup {
            root: identity(&self.runtime.root)?,
            target_path: request.target.clone(),
            target: None,
            remove_target: false,
            backing: None,
        };
        let mut cleanup_attempted = false;
        let outcome = (|| -> Result<(), Error> {
            // Adopt and remove a stale per-volume mount before rebuilding it.
            if let Some(stale) = VolumeDirectory::backing(self, request.volume(), false)? {
                stale.require_bpf()?;
                owned.backing = Some(stale);
                let stale = owned
                    .backing
                    .take()
                    .ok_or(Failure::Unsafe("missing stale volume"))?;
                match crate::removal::remove_volume_directory(stale, true) {
                    Ok(()) => (),
                    Err((error, retained)) => {
                        owned.backing = Some(retained);
                        cleanup_attempted = true;
                        return Err(error);
                    }
                }
            }
            owned.backing = VolumeDirectory::backing(self, request.volume(), true)?;
            let backing = owned
                .backing
                .as_mut()
                .ok_or(Failure::Unsafe("missing volume directory"))?;
            cancelled(cancel)?;
            backing.check()?;
            rustix::mount::mount(
                "bpf",
                proc_path(backing.fd()?),
                "bpf",
                rustix::mount::MountFlags::NOSUID
                    | rustix::mount::MountFlags::NODEV
                    | rustix::mount::MountFlags::NOEXEC
                    | rustix::mount::MountFlags::RELATIME,
                c"",
            )
            .map_err(|e| io("mount CSI bpffs", e))?;
            backing.mounted = true;
            backing.refresh()?;
            backing.require_bpf()?;
            if let Some(group) = request.group {
                rustix::fs::fchown(
                    backing.fd()?,
                    None,
                    Some(rustix::process::Gid::from_raw(group)),
                )
                .map_err(|e| io("set volume group", e))?;
            }
            let bpffs = openat2(&self.runtime.root, "fs", DIRECTORY, Mode::empty(), BENEATH)
                .map_err(|e| io("open map filesystem", e))?;
            let maps = openat2(
                &bpffs,
                format!("maps/{map_set}"),
                DIRECTORY,
                Mode::empty(),
                CONFINED,
            )
            .map_err(|e| io("open program maps", e))?;
            for name in &request.maps {
                cancelled(cancel)?;
                let source = openat2(
                    &maps,
                    name,
                    OFlags::PATH | OFlags::CLOEXEC,
                    Mode::empty(),
                    CONFINED,
                )
                .map_err(|e| io("open requested map", e))?;
                let map = kernel
                    .open_map(PinSource(&proc_path(&source)))
                    .map_err(Failure::Kernel)?;
                cancelled(cancel)?;
                let path = proc_path(backing.fd()?).join(name);
                map.pin(PinTarget(&path)).map_err(Failure::Kernel)?;
                if let Some(group) = request.group {
                    let pin = openat2(
                        backing.fd()?,
                        name,
                        OFlags::PATH | OFlags::CLOEXEC,
                        Mode::empty(),
                        CONFINED,
                    )
                    .map_err(|e| io("adopt published map", e))?;
                    // Pin descriptors anchor chmod/chown; bpffs map pins cannot be opened for ordinary read I/O.
                    rustix::fs::chown(
                        proc_path(&pin),
                        None,
                        Some(rustix::process::Gid::from_raw(group)),
                    )
                    .map_err(|e| io("set map group", e))?;
                    rustix::fs::chmod(proc_path(&pin), Mode::from_raw_mode(0o660))
                        .map_err(|e| io("set map mode", e))?;
                }
            }
            cancelled(cancel)?;
            owned.target = VolumeDirectory::adopt(request.target(), true)?;
            let target = owned
                .target
                .as_mut()
                .ok_or(Failure::Unsafe("missing bind target"))?;
            target.check()?;
            if target.mounted {
                return Err(Failure::Unsafe("bind target became mounted").into());
            }
            rustix::mount::mount_bind(proc_path(backing.fd()?), proc_path(target.fd()?))
                .map_err(|e| io("bind CSI volume", e))?;
            target.mounted = true;
            target.refresh()?;
            if request.readonly {
                rustix::mount::mount_remount(
                    proc_path(target.fd()?),
                    rustix::mount::MountFlags::BIND | rustix::mount::MountFlags::RDONLY,
                    "",
                )
                .map_err(|e| io("remount CSI volume read-only", e))?;
            }
            cancelled(cancel)?;
            Ok(())
        })();
        match outcome {
            Ok(()) => Ok(()),
            Err(cause) => {
                if cleanup_attempted {
                    return Err(VolumeFailure {
                        cause: cause.into(),
                        pending: Some(owned),
                        cleanup: Vec::new(),
                    });
                }
                let (pending, cleanup) = self.cleanup_volume(owned);
                Err(VolumeFailure {
                    cause: cause.into(),
                    pending,
                    cleanup,
                })
            }
        }
    }

    /// One complete pass of independent CSI mount cleanup, ignoring forward cancellation.
    pub fn cleanup_volume(&self, mut owned: VolumeCleanup) -> (Option<VolumeCleanup>, Vec<Error>) {
        let mut errors = Vec::new();
        match identity(&self.runtime.root) {
            Ok(root) if root == owned.root => (),
            Ok(_) => {
                errors.push(Failure::Unsafe("volume belongs to another runtime").into());
                return (Some(owned), errors);
            }
            Err(error) => {
                errors.push(error);
                return (Some(owned), errors);
            }
        }
        for (slot, remove) in [
            (&mut owned.target, owned.remove_target),
            (&mut owned.backing, true),
        ] {
            if let Some(directory) = slot.take() {
                if let Err((error, retained)) =
                    crate::removal::remove_volume_directory(directory, remove)
                {
                    errors.push(error);
                    *slot = Some(retained);
                }
            }
        }
        (
            if owned.target.is_some() || owned.backing.is_some() {
                Some(owned)
            } else {
                None
            },
            errors,
        )
    }

    /// Adopt and tear down a previously published volume, including after restart.
    pub fn unpublish_volume(&self, volume: &str, target: &Path) -> Result<(), VolumeFailure> {
        VolumeRequest::validate_target(volume, target)?;
        let target_path = target;
        let target = VolumeDirectory::adopt(target, false)?;
        let backing = VolumeDirectory::backing(self, volume, false)?;
        if let Some(d) = &target {
            d.require_bpf()?;
        }
        if let Some(d) = &backing {
            d.require_bpf()?;
        }
        let owned = VolumeCleanup {
            root: identity(&self.runtime.root)?,
            target_path: crate::RuntimeLayout::try_from(target_path.to_path_buf())
                .map_err(|_| Error::from(Failure::Unsafe("invalid volume target")))?
                .root()
                .into(),
            target,
            remove_target: true,
            backing,
        };
        let (pending, mut errors) = self.cleanup_volume(owned);
        if errors.is_empty() {
            return Ok(());
        }
        let cause = errors.remove(0);
        Err(VolumeFailure {
            cause: cause.into(),
            cleanup: errors,
            pending,
        })
    }
}
