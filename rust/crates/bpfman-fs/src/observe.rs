//! Adoption for committed-state teardown. This does not create or mount objects.
use crate::{
    Bytecode, Error, MapDirectory, MapPin, ProgramPin, RuntimeWriter, UnloadArtifacts,
    artifacts::{BPF_SUPER_MAGIC, Entry, entry, identity, proc_path, validate_map_name},
    directory::{BENEATH, CONFINED, DIRECTORY},
    error::{Failure, io},
    removal::open_owned,
};
use rustix::fs::{Mode, OFlags, ResolveFlags, fstatfs, openat2};
use std::{num::NonZeroU32, os::fd::OwnedFd};

fn optional_dir(
    parent: &OwnedFd,
    name: &str,
    flags: ResolveFlags,
) -> Result<Option<OwnedFd>, Error> {
    match openat2(parent, name, DIRECTORY, Mode::empty(), flags) {
        Ok(fd) => Ok(Some(fd)),
        Err(rustix::io::Errno::NOENT) => Ok(None),
        Err(error) => Err(io("observe artifact directory", error)),
    }
}
fn observe(
    writer: &RuntimeWriter<'_>,
    parent: &OwnedFd,
    path: &str,
    name: &str,
    directory: bool,
) -> Result<Option<Entry>, Error> {
    let fd = match openat2(
        parent,
        name,
        OFlags::PATH | OFlags::CLOEXEC,
        Mode::empty(),
        CONFINED,
    ) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(io("observe stored artifact", error)),
    };
    let mut receipt = entry(writer, parent, path.into(), name.into(), directory)?;
    receipt.identity = Some(identity(&fd)?);
    open_owned(&receipt)?; // verifies type, link count, and the observed inode
    Ok(Some(receipt))
}
fn names(fd: &OwnedFd) -> Result<Vec<String>, Error> {
    let mut names = std::fs::read_dir(proc_path(fd))
        .map_err(|e| io("enumerate owned directory", e))?
        .map(|item| {
            let item = item.map_err(|e| io("read owned directory entry", e))?;
            item.file_name()
                .into_string()
                .map_err(|_| Failure::Unsafe("non-UTF-8 artifact name").into())
        })
        .collect::<Result<Vec<_>, Error>>()?;
    names.sort();
    Ok(names)
}
impl RuntimeWriter<'_> {
    /// Inspect a committed private tracepoint's canonical artifact locations.
    /// Call only after validating stored exclusive ownership under this writer.
    /// Refuse symlinks, foreign mounts/types, mismatched program/map identities,
    /// hard links and unknown bytecode children before returning any receipts.
    /// Missing artifacts permit retry after an earlier partial unload.
    pub fn observe_unload(&self, id: NonZeroU32) -> Result<UnloadArtifacts, Error> {
        let mut result = UnloadArtifacts {
            program: None,
            maps: Vec::new(),
            directory: None,
            bytecode: None,
        };
        if let Some(bpffs) = optional_dir(&self.runtime.root, "fs", BENEATH)? {
            if fstatfs(&bpffs)
                .map_err(|e| io("verify existing bpffs", e))?
                .f_type
                != BPF_SUPER_MAGIC
            {
                return Err(Failure::Unsafe("existing fs is not bpffs").into());
            }
            let mut map_ids = None;
            if let Some(receipt) = observe(self, &bpffs, "fs", &format!("prog_{id}"), false)? {
                let info =
                    aya::programs::ProgramInfo::from_pin(proc_path(&bpffs).join(&receipt.name))
                        .map_err(Failure::Program)?;
                if info.id() != id.get()
                    || info.program_type() != aya::programs::ProgramType::TracePoint.into()
                {
                    return Err(Failure::Unsafe(
                        "program pin has a different kernel identity or type",
                    )
                    .into());
                }
                map_ids = Some(info.map_ids().map_err(Failure::Program)?.ok_or(
                    Failure::Unsafe("kernel does not report program map identities"),
                )?);
                open_owned(&receipt)?;
                result.program = Some(ProgramPin {
                    id,
                    entry: Box::new(receipt),
                });
            }
            if let Some(maps) = optional_dir(&bpffs, "maps", CONFINED)? {
                if let Some(directory) = observe(self, &maps, "fs/maps", &id.to_string(), true)? {
                    let dir = open_owned(&directory)?;
                    for name in names(&dir)? {
                        validate_map_name(&name)?;
                        let receipt = observe(self, &dir, &format!("fs/maps/{id}"), &name, false)?
                            .ok_or(Failure::Unsafe("map pin disappeared during observation"))?;
                        let info = aya::maps::MapInfo::from_pin(proc_path(&dir).join(&name))
                            .map_err(Failure::Map)?;
                        if map_ids
                            .as_ref()
                            .is_some_and(|ids| !ids.contains(&info.id()))
                        {
                            return Err(
                                Failure::Unsafe("map pin does not belong to this program").into()
                            );
                        }
                        open_owned(&receipt)?;
                        result.maps.push(MapPin {
                            entry: Box::new(receipt),
                        });
                    }
                    result.directory = Some(MapDirectory {
                        entry: Box::new(directory),
                    });
                }
            }
        }
        result.bytecode = self.observe_bytecode(id)?;
        Ok(result)
    }
    fn observe_bytecode(&self, id: NonZeroU32) -> Result<Option<Bytecode>, Error> {
        let Some(programs) = optional_dir(&self.runtime.root, "programs", CONFINED)? else {
            return Ok(None);
        };
        let Some(directory) = observe(self, &programs, "programs", &id.to_string(), true)? else {
            return Ok(None);
        };
        let dir = open_owned(&directory)?;
        let mut files = Vec::new();
        for name in names(&dir)? {
            if !matches!(name.as_str(), "bytecode.o" | "provenance.json") {
                return Err(Failure::Unsafe("unknown child in bytecode directory").into());
            }
            files.push(
                observe(self, &dir, &format!("programs/{id}"), &name, false)?.ok_or(
                    Failure::Unsafe("bytecode file disappeared during observation"),
                )?,
            );
        }
        Ok(Some(Bytecode {
            directory: Box::new(directory),
            files,
        }))
    }
}
