//! Aya stays at the I/O edge. Parsing happens before runtime or kernel effects.

use crate::load_error::LoadCause;
use aya_obj::{Object, ProgramSection, maps::PinningType};
use bpfman_model::ProgramSpec;

use std::{
    collections::BTreeMap, fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt, path::Path,
};

mod xdp;

pub(super) struct LocalObject {
    pub(super) bytes: Vec<u8>,
    pub(super) license: String,
    pub(super) maps: Vec<String>,
    pub(super) globals: BTreeMap<String, Vec<u8>>,
}

pub(super) struct LoadedObject {
    pub(super) bpf: aya::Ebpf,
    pub(super) maps: Vec<String>,
}

impl LocalObject {
    pub(super) fn read(path: &Path, spec: &ProgramSpec) -> Result<Self, LoadCause> {
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
        validate_selection(&object, spec)?;

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
            globals: BTreeMap::new(),
        })
    }

    pub(super) fn load(&self, spec: &ProgramSpec) -> Result<LoadedObject, LoadCause> {
        let mut loader = aya::EbpfLoader::new();
        if matches!(spec, ProgramSpec::Xdp(_)) {
            loader.extension(spec.name().as_str());
        }
        for (name, value) in &self.globals {
            loader.override_global(name, value.as_slice(), true);
        }

        let mut bpf = loader
            .load(&self.bytes)
            .map_err(|e| LoadCause::Kernel(Box::new(e)))?;
        let program = bpf
            .program_mut(spec.name().as_str())
            .ok_or_else(|| LoadCause::Invalid("selected program disappeared during load".into()))?;
        match spec {
            ProgramSpec::Tracepoint(_) => {
                let program: &mut aya::programs::TracePoint = program
                    .try_into()
                    .map_err(|e| LoadCause::Program(Box::new(e)))?;
                program
                    .load()
                    .map_err(|e| LoadCause::Program(Box::new(e)))?;
            }
            ProgramSpec::Xdp(_) => xdp::load(program)?,
            _ => return Err(LoadCause::Unsupported("program type")),
        }

        let ids = program
            .info()
            .and_then(|info| info.map_ids())
            .map_err(|e| LoadCause::Program(Box::new(e)))?
            .ok_or_else(|| LoadCause::Invalid("kernel did not report program map IDs".into()))?;
        let mut maps = Vec::new();

        for name in &self.maps {
            let map = bpf
                .map(name)
                .ok_or_else(|| LoadCause::Invalid(format!("missing loaded map {name}")))?;
            if ids.contains(&map_id(map)?) {
                maps.push(name.clone());
            }
        }

        Ok(LoadedObject { bpf, maps })
    }
}

// Aya exposes MapData::info but has no corresponding method on its Map enum.
// Match exhaustively so newly supported map variants require an explicit review.
fn map_id(map: &aya::maps::Map) -> Result<u32, LoadCause> {
    use aya::maps::Map;
    let data = match map {
        Map::Array(data)
        | Map::ArrayOfMaps(data)
        | Map::BloomFilter(data)
        | Map::CgroupArray(data)
        | Map::CgroupStorage(data)
        | Map::CgrpStorage(data)
        | Map::CpuMap(data)
        | Map::DevMap(data)
        | Map::DevMapHash(data)
        | Map::HashMap(data)
        | Map::HashOfMaps(data)
        | Map::InodeStorage(data)
        | Map::LpmTrie(data)
        | Map::LruHashMap(data)
        | Map::PerCpuArray(data)
        | Map::PerCpuCgroupStorage(data)
        | Map::PerCpuHashMap(data)
        | Map::PerCpuLruHashMap(data)
        | Map::PerfEventArray(data)
        | Map::ProgramArray(data)
        | Map::Queue(data)
        | Map::ReusePortSockArray(data)
        | Map::RingBuf(data)
        | Map::SockHash(data)
        | Map::SockMap(data)
        | Map::SkStorage(data)
        | Map::Stack(data)
        | Map::StackTraceMap(data)
        | Map::Unsupported(data)
        | Map::XskMap(data) => data,
    };

    data.info()
        .map(|info| info.id())
        .map_err(|e| LoadCause::Map(Box::new(e)))
}

// Validate every selection before runtime initialization. XDP sections become
// extensions at the kernel boundary.
pub(super) fn validate_selection(object: &Object, spec: &ProgramSpec) -> Result<(), LoadCause> {
    let program = object.programs.get(spec.name().as_str()).ok_or_else(|| {
        LoadCause::Invalid(format!(
            "ELF program {:?} does not exist",
            spec.name().as_str()
        ))
    })?;
    match (spec, &program.section) {
        (ProgramSpec::Tracepoint(_), ProgramSection::TracePoint)
        | (ProgramSpec::Xdp(_), ProgramSection::Xdp { .. }) => Ok(()),
        (ProgramSpec::Tracepoint(_), _) => Err(LoadCause::Invalid(
            "selected ELF program is not a tracepoint".into(),
        )),
        (ProgramSpec::Xdp(_), _) => {
            Err(LoadCause::Invalid("selected ELF program is not XDP".into()))
        }
        _ => Err(LoadCause::Unsupported("program type")),
    }
}
