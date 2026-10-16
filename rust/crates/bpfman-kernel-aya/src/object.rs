//! Aya stays at the I/O edge. Parsing happens before runtime or kernel effects.

use super::failure::LoadCause;
use crate::{Kernel, LoadedObject};
use aya_obj::{Object, ProgramSection, maps::PinningType, programs::XdpAttachType};
use bpfman_kernel::{Error, ObjectInfo, ObjectLoader};
use bpfman_model::ProgramSpec;

use std::collections::BTreeMap;

pub(super) struct LocalObject {
    pub(super) bytes: Vec<u8>,
    pub(super) license: String,
    pub(super) maps: Vec<String>,
    pub(super) shared_maps: Vec<String>,
    pub(super) globals: BTreeMap<String, Vec<u8>>,
}

impl LocalObject {
    fn parse(bytes: Vec<u8>, spec: &ProgramSpec) -> Result<Self, LoadCause> {
        let object = Object::parse(&bytes).map_err(|e| LoadCause::Parse(Box::new(e)))?;
        validate_selection(&object, spec)?;

        let mut maps = Vec::new();
        let mut shared_maps = Vec::new();

        for (name, map) in &object.maps {
            if name.starts_with('.') {
                if !matches!(map.pinning(), PinningType::None) {
                    return Err(LoadCause::Invalid(
                        "internal maps cannot declare PinByName".into(),
                    ));
                }
                continue;
            }

            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return Err(LoadCause::Invalid(format!("invalid map pin name {name:?}")));
            }

            if matches!(map.pinning(), PinningType::ByName) {
                if map.inner().is_some() {
                    return Err(LoadCause::Unsupported("shared map-of-maps creation"));
                }
                shared_maps.push(name.clone());
            }
            maps.push(name.clone());
        }

        maps.sort();
        shared_maps.sort();
        let license = object
            .license
            .to_str()
            .map_err(|_| LoadCause::Invalid("ELF license is not UTF-8".into()))?
            .to_owned();

        Ok(Self {
            bytes,
            license,
            maps,
            shared_maps,
            globals: BTreeMap::new(),
        })
    }

    fn load(
        &self,
        spec: &ProgramSpec,
        sources: &[(String, bpfman_fs::PinSource<'_>)],
    ) -> Result<LoadedObject, LoadCause> {
        let object = Object::parse(&self.bytes).map_err(|e| LoadCause::Parse(Box::new(e)))?;
        for name in &self.shared_maps {
            if !sources.iter().any(|(n, _)| n == name) {
                return Err(LoadCause::Invalid(
                    "PinByName load requires prepared shared pins".into(),
                ));
            }
        }
        let mut loader = aya::EbpfLoader::new();
        for (name, source) in sources {
            use bpfman_fs::MapDefinition;
            let map = object
                .maps
                .get(name)
                .ok_or_else(|| LoadCause::Invalid("reused map is absent from ELF".into()))?;
            crate::maps::Definition {
                name,
                map,
                btf: None,
            }
            .validate(*source)
            .map_err(|e| LoadCause::MapReuse {
                name: name.clone(),
                source: e,
            })?;
            loader.map_pin_path(name.as_str(), source.path());
        }
        let selection = match spec {
            ProgramSpec::Fentry { target, .. } => {
                Selection::Tracing(target, bpfman_model::TracingKind::Fentry)
            }
            ProgramSpec::Fexit { target, .. } => {
                Selection::Tracing(target, bpfman_model::TracingKind::Fexit)
            }
            ProgramSpec::Lsm { hook, .. } => {
                Selection::Tracing(hook, bpfman_model::TracingKind::Lsm)
            }
            ProgramSpec::Tracepoint(_) => Selection::Tracepoint,
            ProgramSpec::Kprobe(_) | ProgramSpec::Kretprobe(_) => Selection::Kprobe,
            ProgramSpec::Uprobe(_) | ProgramSpec::Uretprobe(_) => Selection::Uprobe,
            ProgramSpec::Tc(_) => Selection::Tc,
            ProgramSpec::Tcx(_) => Selection::Tcx,
            ProgramSpec::Xdp(_) => Selection::Xdp(xdp_role(&self.bytes, spec.name().as_str())?),
        };
        if matches!(
            selection,
            Selection::Tc | Selection::Xdp(XdpRole::Interface { .. })
        ) {
            // TODO: Upstream Aya 0.14's extension override drops BPF_F_XDP_HAS_FRAGS.
            // Fragment helpers still work against the verification target, but
            // DEVMAP ownership records a linear program and rejects frags egress
            // with EINVAL. Enable multi-buffer DEVMAP egress when upstream exposes
            // flag preservation; do not fork Aya or install linear egress on jumbo
            // traffic as a workaround. Kernel acceptance records this boundary.
            // DEVMAP_HASH differs on Linux 6.18.54: map_type_contains_progs omits
            // it, so native frags egress initializes compatible ownership at map
            // update. Positive hash egress tests prove jumbo delivery; this does
            // not repair the extension flag or the array DEVMAP boundary.
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
        let mut native = None;
        match selection {
            Selection::Tracing(target, kind) => {
                native = crate::function_tracing::load(
                    &mut bpf,
                    &self.bytes,
                    spec.name().as_str(),
                    target.as_str(),
                    kind,
                )?;
            }
            Selection::Tc => crate::verification::load_tc(program)?,
            Selection::Tcx => {
                let program: &mut aya::programs::SchedClassifier = program
                    .try_into()
                    .map_err(|e| LoadCause::Program(Box::new(e)))?;
                program
                    .load()
                    .map_err(|e| LoadCause::Program(Box::new(e)))?;
            }
            Selection::Kprobe => {
                let program: &mut aya::programs::KProbe = program
                    .try_into()
                    .map_err(|e| LoadCause::Program(Box::new(e)))?;
                program
                    .load()
                    .map_err(|e| LoadCause::Program(Box::new(e)))?;
            }
            Selection::Uprobe => {
                let program: &mut aya::programs::UProbe = program
                    .try_into()
                    .map_err(|e| LoadCause::Program(Box::new(e)))?;
                program
                    .load()
                    .map_err(|e| LoadCause::Program(Box::new(e)))?;
            }
            Selection::Tracepoint => {
                let program: &mut aya::programs::TracePoint = program
                    .try_into()
                    .map_err(|e| LoadCause::Program(Box::new(e)))?;
                program
                    .load()
                    .map_err(|e| LoadCause::Program(Box::new(e)))?;
            }
            Selection::Xdp(role) => match role {
                XdpRole::Interface { frags } => crate::verification::load(program, frags)?,
                XdpRole::DevMap => {
                    let program: &mut aya::programs::Xdp = program
                        .try_into()
                        .map_err(|e| LoadCause::Program(Box::new(e)))?;
                    program
                        .load()
                        .map_err(|e| LoadCause::Program(Box::new(e)))?;
                }
            },
        }

        // Retain every public ELF map, including maps used by other selections.
        // Explicit map-set joins must be able to reuse these by their ELF names.
        let maps = self.maps.clone();

        Ok(LoadedObject {
            native,
            bpf,
            maps,
            name: spec.name().as_str().into(),
        })
    }
}

// Aya exposes MapData::info but has no corresponding method on its Map enum.
// Match exhaustively so newly supported map variants require an explicit review.
pub(super) fn map_data(map: &aya::maps::Map) -> &aya::maps::MapData {
    use aya::maps::Map;
    match map {
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
    }
}

pub(super) fn map_id(map: &aya::maps::Map) -> Result<u32, LoadCause> {
    map_data(map)
        .info()
        .map(|info| info.id())
        .map_err(|e| LoadCause::Map(Box::new(e)))
}

// Validate every selection before runtime initialization. The ELF section owns
// the XDP role; retaining bytecode preserves it without changing Go's store schema.
pub(super) fn validate_selection(object: &Object, spec: &ProgramSpec) -> Result<(), LoadCause> {
    let program = object.programs.get(spec.name().as_str()).ok_or_else(|| {
        LoadCause::Invalid(format!(
            "ELF program {:?} does not exist",
            spec.name().as_str()
        ))
    })?;
    match (spec, &program.section) {
        (ProgramSpec::Lsm { .. }, ProgramSection::Lsm { .. }) => Ok(()),
        (ProgramSpec::Lsm { .. }, _) => Err(LoadCause::Invalid(
            "LSM selection requires an lsm section".into(),
        )),
        (ProgramSpec::Fentry { .. }, ProgramSection::FEntry { .. })
        | (ProgramSpec::Fexit { .. }, ProgramSection::FExit { .. }) => Ok(()),
        (ProgramSpec::Fentry { .. } | ProgramSpec::Fexit { .. }, _) => Err(LoadCause::Invalid(
            "function tracing selection requires its matching fentry/fexit section".into(),
        )),
        (ProgramSpec::Tc(_) | ProgramSpec::Tcx(_), ProgramSection::SchedClassifier) => Ok(()),
        (ProgramSpec::Tc(_) | ProgramSpec::Tcx(_), _) => Err(LoadCause::Invalid(
            "TC selection requires a classifier section".into(),
        )),
        (
            ProgramSpec::Kprobe(_) | ProgramSpec::Kretprobe(_),
            ProgramSection::KProbe | ProgramSection::KRetProbe,
        ) => Ok(()),
        (ProgramSpec::Kprobe(_) | ProgramSpec::Kretprobe(_), _) => Err(LoadCause::Invalid(
            "kernel probe selection requires a kprobe or kretprobe section".into(),
        )),
        (
            ProgramSpec::Uprobe(_) | ProgramSpec::Uretprobe(_),
            ProgramSection::UProbe { .. } | ProgramSection::URetProbe { .. },
        ) => Ok(()),
        (ProgramSpec::Uprobe(_) | ProgramSpec::Uretprobe(_), _) => Err(LoadCause::Invalid(
            "user probe selection requires a uprobe or uretprobe section".into(),
        )),
        (ProgramSpec::Tracepoint(_), ProgramSection::TracePoint) => Ok(()),
        (ProgramSpec::Xdp(_), section @ ProgramSection::Xdp { .. }) => {
            XdpRole::from_section(section).map(|_| ())
        }
        (ProgramSpec::Tracepoint(_), _) => Err(LoadCause::Invalid(
            "selected ELF program is not a tracepoint".into(),
        )),
        (ProgramSpec::Xdp(_), _) => {
            Err(LoadCause::Invalid("selected ELF program is not XDP".into()))
        }
    }
}

impl ObjectLoader for Kernel {
    fn validate_object(&self, bytes: &[u8], spec: &ProgramSpec) -> Result<ObjectInfo, Error> {
        let object = LocalObject::parse(bytes.to_vec(), spec).map_err(Error::from)?;
        Ok(ObjectInfo {
            license: object.license,
            maps: object.maps,
            shared_maps: object.shared_maps,
        })
    }

    fn validate_globals(
        &self,
        bytes: &[u8],
        globals: &BTreeMap<String, Vec<u8>>,
    ) -> Result<(), Error> {
        let mut object = Object::parse(bytes).map_err(|e| LoadCause::Parse(Box::new(e)))?;
        object
            .patch_map_data(
                globals
                    .iter()
                    .map(|(name, bytes)| (name.as_str(), (bytes.as_slice(), true)))
                    .collect(),
            )
            .map_err(|e| Error::from(LoadCause::Parse(Box::new(e))))
    }
}

pub(super) fn load(
    bytes: &[u8],
    maps: &[String],
    globals: &BTreeMap<String, Vec<u8>>,
    spec: &ProgramSpec,
) -> Result<LoadedObject, Error> {
    load_with_maps(bytes, maps, globals, spec, &[])
}

pub(super) fn load_with_maps(
    bytes: &[u8],
    maps: &[String],
    globals: &BTreeMap<String, Vec<u8>>,
    spec: &ProgramSpec,
    sources: &[(String, bpfman_fs::PinSource<'_>)],
) -> Result<LoadedObject, Error> {
    let mut object = LocalObject::parse(bytes.to_vec(), spec)?;
    object.maps = maps.to_vec();
    object.globals = globals.clone();
    object.load(spec, sources).map_err(Into::into)
}

pub(super) fn xdp_frags(bytes: &[u8], name: &str) -> Result<bool, LoadCause> {
    match xdp_role(bytes, name)? {
        XdpRole::Interface { frags } => Ok(frags),
        XdpRole::DevMap => Err(LoadCause::Invalid(
            "DEVMAP egress cannot join an interface dispatcher".into(),
        )),
    }
}

#[derive(Clone, Copy)]
enum XdpRole {
    Interface { frags: bool },
    DevMap,
}

enum Selection<'a> {
    Tracing(&'a bpfman_model::Symbol, bpfman_model::TracingKind),
    Kprobe,
    Uprobe,
    Tc,
    Tcx,
    Tracepoint,
    Xdp(XdpRole),
}

impl XdpRole {
    fn from_section(section: &ProgramSection) -> Result<Self, LoadCause> {
        match section {
            ProgramSection::Xdp {
                attach_type: XdpAttachType::Interface,
                frags,
            } => Ok(Self::Interface { frags: *frags }),
            ProgramSection::Xdp {
                attach_type: XdpAttachType::DevMap,
                ..
            } => Ok(Self::DevMap),
            ProgramSection::Xdp {
                attach_type: XdpAttachType::CpuMap,
                ..
            } => Err(LoadCause::Unsupported("CPUMAP XDP")),
            _ => Err(LoadCause::Invalid("selected ELF program is not XDP".into())),
        }
    }
}

fn xdp_role(bytes: &[u8], name: &str) -> Result<XdpRole, LoadCause> {
    let object = Object::parse(bytes).map_err(|e| LoadCause::Parse(Box::new(e)))?;
    let section = object
        .programs
        .get(name)
        .map(|p| &p.section)
        .ok_or_else(|| LoadCause::Invalid("selected ELF program does not exist".into()))?;
    XdpRole::from_section(section)
}
