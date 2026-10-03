//! Managed artifacts are addressed below opened directories, never caller paths.
use crate::{
    Bytecode, Error, MapDirectory, MapPin, PreparedLoad, ProgramPin, RuntimeWriter,
    directory::{BENEATH, CONFINED, DIRECTORY, ensure_directory},
    error::{Failure, io},
};
use bpfman_core::EffectFailure;
use rustix::fs::{Mode, OFlags, RenameFlags, fstat, fstatfs, mkdirat, openat2, renameat_with};
use std::{
    fs::File,
    io::Write,
    num::NonZeroU32,
    os::fd::{AsRawFd, OwnedFd},
    path::PathBuf,
};

// Linux uapi/linux/magic.h; rustix does not currently export this constant.
pub(super) const BPF_SUPER_MAGIC: rustix::fs::FsWord = 0xcafe4a11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Identity {
    device: u64,
    inode: u64,
}

pub(super) fn identity(fd: &OwnedFd) -> Result<Identity, Error> {
    let stat = fstat(fd).map_err(|e| io("inspect owned artifact", e))?;
    Ok(Identity {
        device: stat.st_dev,
        inode: stat.st_ino,
    })
}

#[derive(Debug)]
pub(super) struct Entry {
    pub(super) root: Identity,
    pub(super) parent: OwnedFd,
    pub(super) name: String,
    pub(super) parent_path: String,
    // None retains ownership after creation if observing the new inode failed.
    // Removal refuses this case rather than guessing what now occupies the name.
    pub(super) identity: Option<Identity>,
    pub(super) directory: bool,
}

impl Entry {
    pub(super) fn observe(&mut self) -> Result<(), Error> {
        let fd = openat2(
            &self.parent,
            &self.name,
            OFlags::PATH | OFlags::CLOEXEC,
            Mode::empty(),
            CONFINED,
        )
        .map_err(|e| io("observe newly owned artifact", e))?;
        self.identity = Some(identity(&fd)?);
        Ok(())
    }
    pub(super) fn check_writer(&self, writer: &RuntimeWriter<'_>) -> Result<(), Error> {
        if self.root != identity(&writer.runtime.root)? {
            return Err(Failure::Unsafe("artifact belongs to another runtime").into());
        }
        let current = if let Some(relative) = self.parent_path.strip_prefix("fs/") {
            let bpffs = openat2(
                &writer.runtime.root,
                "fs",
                DIRECTORY,
                Mode::empty(),
                BENEATH,
            )
            .map_err(|e| io("revalidate bpffs ancestor", e))?;
            openat2(&bpffs, relative, DIRECTORY, Mode::empty(), CONFINED)
        } else if self.parent_path == "fs" {
            openat2(
                &writer.runtime.root,
                "fs",
                DIRECTORY,
                Mode::empty(),
                BENEATH,
            )
        } else {
            openat2(
                &writer.runtime.root,
                &self.parent_path,
                DIRECTORY,
                Mode::empty(),
                CONFINED,
            )
        }
        .map_err(|e| io("revalidate artifact parent", e))?;
        if identity(&current)? != identity(&self.parent)? {
            return Err(Failure::Unsafe("artifact parent was moved or replaced").into());
        }
        Ok(())
    }
}

pub(super) fn entry(
    writer: &RuntimeWriter<'_>,
    parent: &OwnedFd,
    parent_path: String,
    name: String,
    directory: bool,
) -> Result<Entry, Error> {
    Ok(Entry {
        root: identity(&writer.runtime.root)?,
        parent: rustix::io::fcntl_dupfd_cloexec(parent, 0)
            .map_err(|e| io("retain artifact parent", e))?,
        name,
        parent_path,
        identity: None,
        directory,
    })
}

pub(super) fn proc_path(fd: &OwnedFd) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()))
}

impl RuntimeWriter<'_> {
    /// Adopt or mount bpffs at the Go-compatible `fs` child. Only this intentional
    /// mount boundary may be crossed; descendants must stay on that filesystem.
    /// Existing nonempty ordinary directories are never covered by a mount.
    pub fn prepare_load(&self) -> Result<PreparedLoad, Error> {
        let mut bpffs = ensure_directory(&self.runtime.root, "fs", BENEATH)?;
        if fstatfs(&bpffs).map_err(|e| io("inspect bpffs", e))?.f_type != BPF_SUPER_MAGIC {
            // An existing mount of any other filesystem is not ours to cover.
            openat2(&self.runtime.root, "fs", DIRECTORY, Mode::empty(), CONFINED)
                .map_err(|e| io("refuse unexpected mount at fs", e))?;
            // The proc descriptor is an anchor, not the configured runtime path.
            if std::fs::read_dir(proc_path(&bpffs))
                .map_err(|e| io("inspect mount target", e))?
                .next()
                .is_some()
            {
                return Err(Failure::Unsafe("refuse to mount over a nonempty fs directory").into());
            }
            rustix::mount::mount(
                "bpf",
                proc_path(&bpffs),
                "bpf",
                rustix::mount::MountFlags::NOSUID
                    | rustix::mount::MountFlags::NODEV
                    | rustix::mount::MountFlags::NOEXEC,
                c"mode=0700",
            )
            .map_err(|e| io("mount runtime bpffs", e))?;
            bpffs = openat2(&self.runtime.root, "fs", DIRECTORY, Mode::empty(), BENEATH)
                .map_err(|e| io("adopt mounted bpffs", e))?;
        }
        if fstatfs(&bpffs).map_err(|e| io("verify bpffs", e))?.f_type != BPF_SUPER_MAGIC {
            return Err(Failure::Unsafe("fs is not bpffs").into());
        }
        let maps = ensure_directory(&bpffs, "maps", CONFINED)?;
        Ok(PreparedLoad {
            root: identity(&self.runtime.root)?,
            bpffs,
            maps,
        })
    }

    /// Publish the exact bytes validated and loaded by the kernel adapter.
    /// Staging and final artifacts carry receipts even after partial failure.
    pub fn publish_bytecode(
        &self,
        id: NonZeroU32,
        bytes: &[u8],
        provenance: &[u8],
    ) -> Result<Bytecode, EffectFailure<Vec<Bytecode>, Error>> {
        let fail = |cause| EffectFailure {
            cause,
            remaining: Vec::new(),
        };
        let programs = ensure_directory(&self.runtime.root, "programs", CONFINED).map_err(fail)?;
        let staging = ensure_directory(&self.runtime.root, ".staging", CONFINED).map_err(fail)?;
        // One load per kernel ID; never adopt even an empty pre-existing staging directory.
        let owned = entry(
            self,
            &staging,
            ".staging".into(),
            format!("load-{}", id),
            true,
        )
        .map_err(fail)?;
        mkdirat(&staging, &owned.name, Mode::from_raw_mode(0o700))
            .map_err(|e| fail(io("create bytecode staging directory", e)))?;
        let mut receipt = Bytecode {
            directory: Box::new(owned),
            files: Vec::new(),
        };
        let result = (|| {
            receipt.directory.observe()?;
            let dir = openat2(
                &staging,
                &receipt.directory.name,
                DIRECTORY,
                Mode::empty(),
                CONFINED,
            )
            .map_err(|e| io("open bytecode staging directory", e))?;
            for (name, data) in [("bytecode.o", bytes), ("provenance.json", provenance)] {
                let file_receipt = entry(
                    self,
                    &dir,
                    format!(".staging/{}", receipt.directory.name),
                    name.into(),
                    false,
                )?;
                let fd = openat2(
                    &dir,
                    name,
                    OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
                    Mode::from_raw_mode(0o644),
                    CONFINED,
                )
                .map_err(|e| io("create bytecode artifact", e))?;
                receipt.files.push(file_receipt);
                if let Some(last) = receipt.files.last_mut() {
                    last.identity = Some(identity(&fd)?);
                }
                let mut file = File::from(fd);
                file.write_all(data)
                    .map_err(|e| io("write bytecode artifact", e))?;
                file.sync_all()
                    .map_err(|e| io("sync bytecode artifact", e))?;
            }
            // Allocate/duplicate before rename: after publication updating the receipt cannot fail.
            let final_parent = rustix::io::fcntl_dupfd_cloexec(&programs, 0)
                .map_err(|e| io("retain published parent", e))?;
            let final_name = id.to_string();
            receipt.directory.check_writer(self)?;
            super::removal::open_owned(&receipt.directory)?;
            let current_programs = openat2(
                &self.runtime.root,
                "programs",
                DIRECTORY,
                Mode::empty(),
                CONFINED,
            )
            .map_err(|e| io("revalidate bytecode publication parent", e))?;
            if identity(&current_programs)? != identity(&programs)? {
                return Err(Failure::Unsafe("bytecode publication parent changed").into());
            }
            renameat_with(
                &staging,
                &receipt.directory.name,
                &programs,
                &final_name,
                RenameFlags::NOREPLACE,
            )
            .map_err(|e| io("publish bytecode without replacing existing state", e))?;
            receipt.directory.parent_path = "programs".into();
            for child in &mut receipt.files {
                child.parent_path = format!("programs/{id}");
            }
            receipt.directory.parent = final_parent;
            receipt.directory.name = final_name;
            Ok(())
        })();
        match result {
            Ok(()) => Ok(receipt),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: vec![receipt],
            }),
        }
    }
}

impl PreparedLoad {
    fn check(&self, writer: &RuntimeWriter<'_>) -> Result<(), Error> {
        if self.root != identity(&writer.runtime.root)? {
            return Err(Failure::Unsafe("bpffs belongs to another runtime").into());
        }
        let current = openat2(
            &writer.runtime.root,
            "fs",
            DIRECTORY,
            Mode::empty(),
            BENEATH,
        )
        .map_err(|e| io("revalidate bpffs", e))?;
        let maps = openat2(&current, "maps", DIRECTORY, Mode::empty(), CONFINED)
            .map_err(|e| io("revalidate map collection", e))?;
        if identity(&current)? != identity(&self.bpffs)?
            || identity(&maps)? != identity(&self.maps)?
        {
            return Err(Failure::Unsafe("bpffs or map collection was replaced").into());
        }
        Ok(())
    }

    /// Pin a loaded program using its kernel identity. Aya performs only the
    /// pin syscall; this adapter owns the descriptor-relative target and receipt.
    pub fn pin_program(
        &self,
        writer: &RuntimeWriter<'_>,
        program: &mut aya::programs::Program,
    ) -> Result<ProgramPin, EffectFailure<Option<ProgramPin>, Error>> {
        let fail = |cause| EffectFailure {
            cause,
            remaining: None,
        };
        self.check(writer).map_err(fail)?;
        let raw = program
            .info()
            .map_err(|e| fail(Failure::Program(e).into()))?
            .id();
        let id = NonZeroU32::new(raw)
            .ok_or_else(|| fail(Failure::Unsafe("zero kernel program ID").into()))?;
        let owned = entry(
            writer,
            &self.bpffs,
            "fs".into(),
            format!("prog_{id}"),
            false,
        )
        .map_err(fail)?;
        program
            .pin(proc_path(&self.bpffs).join(&owned.name))
            .map_err(|e| fail(Failure::Pin(e).into()))?;
        let mut receipt = ProgramPin {
            id,
            entry: Box::new(owned),
        };
        match receipt.entry.observe() {
            Ok(()) => Ok(receipt),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: Some(receipt),
            }),
        }
    }

    /// Create a private map directory, refusing existing state.
    pub fn create_map_directory(
        &self,
        writer: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<MapDirectory, EffectFailure<Option<MapDirectory>, Error>> {
        let fail = |cause| EffectFailure {
            cause,
            remaining: None,
        };
        self.check(writer).map_err(fail)?;
        let owned =
            entry(writer, &self.maps, "fs/maps".into(), id.to_string(), true).map_err(fail)?;
        mkdirat(&self.maps, &owned.name, Mode::from_raw_mode(0o755))
            .map_err(|e| fail(io("create private map directory", e)))?;
        let mut receipt = MapDirectory {
            entry: Box::new(owned),
        };
        match receipt.entry.observe() {
            Ok(()) => Ok(receipt),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: Some(receipt),
            }),
        }
    }
}

impl MapDirectory {
    /// Pin one private map. Names are validated here; ELF symbols are not paths.
    pub fn pin_map(
        &self,
        writer: &RuntimeWriter<'_>,
        name: &str,
        map: &aya::maps::Map,
    ) -> Result<MapPin, EffectFailure<Option<MapPin>, Error>> {
        let fail = |cause| EffectFailure {
            cause,
            remaining: None,
        };
        self.entry.check_writer(writer).map_err(fail)?;
        validate_map_name(name).map_err(fail)?;
        let parent = super::removal::open_owned(&self.entry).map_err(fail)?;
        let owned = entry(
            writer,
            &parent,
            format!("fs/maps/{}", self.entry.name),
            name.to_owned(),
            false,
        )
        .map_err(fail)?;
        map.pin(proc_path(&parent).join(name))
            .map_err(|e| fail(Failure::Pin(e).into()))?;
        let mut receipt = MapPin {
            entry: Box::new(owned),
        };
        match receipt.entry.observe() {
            Ok(()) => Ok(receipt),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: Some(receipt),
            }),
        }
    }
}

pub(super) fn validate_map_name(name: &str) -> Result<(), Error> {
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(Failure::Unsafe(
            "map pin name must contain only ASCII letters, digits, and underscores",
        )
        .into());
    }
    Ok(())
}

impl ProgramPin {
    /// Kernel identity of this owned program pin.
    pub fn id(&self) -> NonZeroU32 {
        self.id
    }
}
