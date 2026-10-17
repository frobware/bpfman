//! ELF interpretation for OCI labels, without loading kernel objects.
use std::collections::BTreeMap;

use aya_obj::{Object, ProgramSection};
use bpfman_kernel::{Error, ErrorKind};

mod foreign;

/// Canonical bpfman image labels derived from bytecode.
pub struct BytecodeMetadata {
    /// Program names and library-independent bpfman family names.
    pub programs: BTreeMap<String, String>,
    /// Declared map names and canonical Linux map-type spellings.
    pub maps: BTreeMap<String, String>,
}

/// Validate a BPF ELF and derive image metadata, without kernel or filesystem I/O.
/// The expected byte order is supplied by the requested OCI platform.
pub fn inspect_bytecode(bytes: &[u8], little_endian: bool) -> Result<BytecodeMetadata, Error> {
    let invalid = |message: &'static str| {
        Error::new(
            ErrorKind::InvalidInput,
            message,
            std::io::Error::from(std::io::ErrorKind::InvalidData),
        )
    };
    if bytes.len() < 20 || &bytes[..4] != b"\x7fELF" || bytes[4] != 2 {
        return Err(invalid("open bytecode ELF: expected ELF64 BPF object"));
    }
    if bytes[5] != if little_endian { 1 } else { 2 } {
        return Err(invalid(if little_endian {
            "bytecode endianness mismatch: expected ELFDATA2LSB"
        } else {
            "bytecode endianness mismatch: expected ELFDATA2MSB"
        }));
    }
    let word = |offset| {
        let value = [bytes[offset], bytes[offset + 1]];
        if little_endian {
            u16::from_le_bytes(value)
        } else {
            u16::from_be_bytes(value)
        }
    };
    if word(16) != 1 || word(18) != 247 {
        return Err(invalid(
            "open bytecode ELF: expected relocatable EM_BPF object",
        ));
    }
    // Aya 0.3 reads BTF headers in native byte order. Cross-platform packaging
    // needs metadata only; decode the documented ELF/BTF format independently,
    // without changing the original bytes or depending on Aya internals.
    if little_endian != cfg!(target_endian = "little") {
        return foreign::inspect(bytes, little_endian)
            .map_err(|cause| Error::new(ErrorKind::InvalidInput, "open bytecode ELF", cause));
    }
    let object = Object::parse(bytes)
        .map_err(|source| Error::new(ErrorKind::InvalidInput, "open bytecode ELF", source))?;
    let programs = object
        .programs
        .iter()
        .filter_map(|(name, program)| {
            let kind = match program.section {
                ProgramSection::KProbe => "kprobe",
                ProgramSection::KRetProbe => "kretprobe",
                ProgramSection::UProbe { .. } => "uprobe",
                ProgramSection::URetProbe { .. } => "uretprobe",
                ProgramSection::TracePoint => "tracepoint",
                ProgramSection::Xdp { .. } => "xdp",
                ProgramSection::SchedClassifier => "tc",
                ProgramSection::FEntry { .. } => "fentry",
                ProgramSection::FExit { .. } => "fexit",
                ProgramSection::Lsm { .. } => "lsm",
                _ => return None,
            };
            Some((name.clone(), kind.into()))
        })
        .collect::<BTreeMap<_, _>>();
    if programs.is_empty() {
        return Err(invalid("no supported programs found in bytecode"));
    }
    let maps = object
        .maps
        .iter()
        .filter(|(name, _)| !name.starts_with('.'))
        .map(|(name, map)| {
            map_type(map.map_type())
                .map(|kind| (name.clone(), kind.to_owned()))
                .ok_or_else(|| invalid("unsupported bytecode map type"))
        })
        .collect::<Result<_, _>>()?;
    Ok(BytecodeMetadata { programs, maps })
}

fn map_type(value: u32) -> Option<&'static str> {
    [
        "unspec",
        "hash",
        "array",
        "prog_array",
        "perf_event_array",
        "per_cpu_hash",
        "per_cpu_array",
        "stack_trace",
        "cgroup_array",
        "lru_hash",
        "lru_per_cpu_hash",
        "lpm_trie",
        "array_of_maps",
        "hash_of_maps",
        "devmap",
        "sockmap",
        "cpumap",
        "xskmap",
        "sockhash",
        "cgroup_storage",
        "reuseport_sockarray",
        "per_cpu_cgroup_storage",
        "queue",
        "stack",
        "sk_storage",
        "devmap_hash",
        "struct_ops",
        "ringbuf",
        "inode_storage",
        "task_storage",
        "bloom_filter",
        "user_ringbuf",
        "cgrp_storage",
        "arena",
    ]
    .get(value as usize)
    .copied()
}
