//! Aya stays at the I/O edge. Parsing happens before runtime or kernel effects.
use crate::load_error::LoadCause;
use aya_obj::{Object, ProgramSection, maps::PinningType};
use bpfman_model::Symbol;
use std::{fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt, path::Path};

pub(super) struct LocalObject {
    pub(super) bytes: Vec<u8>,
    pub(super) license: String,
    pub(super) maps: Vec<String>,
}

impl LocalObject {
    pub(super) fn read(path: &Path, name: &Symbol) -> Result<Self, LoadCause> {
        // O_NONBLOCK makes a FIFO fail regular-file validation instead of hanging.
        // This is input I/O, not a managed-object filesystem operation.
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(LoadCause::Read)?;
        if !file.metadata().map_err(LoadCause::Read)?.is_file() {
            return Err(LoadCause::Invalid(
                "source must be a regular ELF file".into(),
            ));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(LoadCause::Read)?;
        let object = Object::parse(&bytes).map_err(|e| LoadCause::Parse(Box::new(e)))?;
        let program = object.programs.get(name.as_str()).ok_or_else(|| {
            LoadCause::Invalid(format!("ELF program {:?} does not exist", name.as_str()))
        })?;
        if !matches!(program.section, ProgramSection::TracePoint) {
            return Err(LoadCause::Invalid(
                "selected ELF program is not a tracepoint".into(),
            ));
        }
        let mut maps = Vec::new();
        for (name, map) in &object.maps {
            if !matches!(map.pinning(), PinningType::None) {
                return Err(LoadCause::Unsupported("PinByName maps"));
            }
            if name.starts_with('.') {
                continue;
            }
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return Err(LoadCause::Invalid(format!("invalid map pin name {name:?}")));
            }
            maps.push(name.clone());
        }
        maps.sort();
        let license = object
            .license
            .to_str()
            .map_err(|_| LoadCause::Invalid("ELF license is not UTF-8".into()))?
            .to_owned();
        Ok(Self {
            bytes,
            license,
            maps,
        })
    }

    pub(super) fn load(&self, name: &Symbol) -> Result<aya::Ebpf, LoadCause> {
        let mut bpf = aya::EbpfLoader::new()
            .load(&self.bytes)
            .map_err(|e| LoadCause::Kernel(Box::new(e)))?;
        let program = bpf
            .program_mut(name.as_str())
            .ok_or_else(|| LoadCause::Invalid("selected program disappeared during load".into()))?;
        let tracepoint: &mut aya::programs::TracePoint = program
            .try_into()
            .map_err(|e| LoadCause::Program(Box::new(e)))?;
        tracepoint
            .load()
            .map_err(|e| LoadCause::Program(Box::new(e)))?;
        Ok(bpf)
    }
}
