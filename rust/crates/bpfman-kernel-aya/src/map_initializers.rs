//! Checked ELF map references. Only newly owned outer maps may be populated.

use aya::maps::{ArrayOfMaps, HashOfMaps, Map, MapData, MapError};
use aya_obj::generated::bpf_map_type::{BPF_MAP_TYPE_ARRAY_OF_MAPS, BPF_MAP_TYPE_HASH_OF_MAPS};
use object::{Object, ObjectSection, ObjectSymbol};
use std::{collections::BTreeSet, io, os::fd::AsFd};

pub(super) struct Entry {
    outer: String,
    inner: String,
    key: u32,
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid map-of-maps initializer relocation",
    )
}

fn object_error(_: object::Error) -> io::Error {
    invalid()
}

pub(super) fn parse(bytes: &[u8], parsed: &aya_obj::Object) -> io::Result<Vec<Entry>> {
    let file = object::File::parse(bytes).map_err(object_error)?;
    let Some(section) = file.section_by_name(".maps") else {
        return Ok(Vec::new());
    };
    if section
        .data()
        .map_err(object_error)?
        .iter()
        .any(|b| *b != 0)
    {
        return Err(invalid());
    }
    if section.relocations().next().is_none() {
        return Ok(Vec::new());
    }
    let offsets = crate::image::map_value_offsets(bytes)?;
    let mut ranges = Vec::new();
    for (name, map) in &parsed.maps {
        if map.section_index() != section.index().0 {
            continue;
        }
        let symbol = file
            .symbol_by_index(object::SymbolIndex(map.symbol_index().ok_or_else(invalid)?))
            .map_err(object_error)?;
        let start = symbol.address();
        let end = start.checked_add(symbol.size()).ok_or_else(invalid)?;
        if symbol.section_index() != Some(section.index())
            || symbol.name().map_err(object_error)? != name
            || start >= end
            || end > section.size()
        {
            return Err(invalid());
        }
        ranges.push((start, end, name, map));
    }
    ranges.sort_by_key(|(start, _, _, _)| *start);
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(invalid());
    }
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for (offset, relocation) in section.relocations() {
        // R_BPF_64_ABS64 relocates pointer data; R_BPF_64_64 is for LDDW.
        if relocation.flags() != (object::RelocationFlags::Elf { r_type: 2 })
            || relocation.addend() != 0
        {
            return Err(invalid());
        }
        let (start, end, name, map) = ranges
            .iter()
            .find(|(start, end, _, _)| *start <= offset && offset < *end)
            .ok_or_else(invalid)?;
        let values = start
            .checked_add(u64::from(*offsets.get(*name).ok_or_else(invalid)?))
            .ok_or_else(invalid)?;
        let delta = offset.checked_sub(values).ok_or_else(invalid)?;
        let key = u32::try_from(delta / 8).map_err(|_| invalid())?;
        if delta % 8 != 0
            || offset.checked_add(8).ok_or_else(invalid)? > *end
            || map.inner().is_none()
            || ![
                BPF_MAP_TYPE_ARRAY_OF_MAPS as u32,
                BPF_MAP_TYPE_HASH_OF_MAPS as u32,
            ]
            .contains(&map.map_type())
            || map.key_size() != 4
            || (map.map_type() == BPF_MAP_TYPE_ARRAY_OF_MAPS as u32 && key >= map.max_entries())
            || !seen.insert((name.as_str(), key))
        {
            return Err(invalid());
        }
        let object::RelocationTarget::Symbol(index) = relocation.target() else {
            return Err(invalid());
        };
        let target = file.symbol_by_index(index).map_err(object_error)?;
        let inner = target.name().map_err(object_error)?;
        let inner_map = parsed.maps.get(inner).ok_or_else(invalid)?;
        if target.section_index() != Some(section.index())
            || inner_map.symbol_index() != Some(index.0)
            || inner_map.inner().is_some()
        {
            return Err(invalid());
        }
        entries.push(Entry {
            outer: (*name).clone(),
            inner: inner.into(),
            key,
        });
    }
    entries.sort_by(|a, b| (&a.outer, a.key).cmp(&(&b.outer, b.key)));
    Ok(entries)
}

/// Reused outers are observation-only. Check every initializer before writing
/// any newly owned outer, so conflicts cannot partially replay live entries.
pub(super) fn populate(
    bpf: &mut aya::Ebpf,
    entries: &[Entry],
    sources: &[(String, bpfman_fs::PinSource<'_>)],
    fresh: &[String],
) -> Result<(), crate::failure::LoadCause> {
    use crate::failure::LoadCause;

    let reused = |name: &str| {
        sources.iter().any(|(source, _)| source == name) && !fresh.iter().any(|n| n == name)
    };
    let map_error = |e| LoadCause::Map(Box::new(e));
    let get_map = |name: &str| {
        bpf.map(name)
            .ok_or_else(|| LoadCause::Invalid("initializer map disappeared".into()))
    };
    for entry in entries.iter().filter(|entry| reused(&entry.outer)) {
        let expected = crate::object::map_id(get_map(&entry.inner)?)?;
        let outer = get_map(&entry.outer)?;
        let current = match outer {
            Map::ArrayOfMaps(_) => {
                let array: ArrayOfMaps<_, MapData> = outer.try_into().map_err(map_error)?;
                array.get(&entry.key, 0)
            }
            Map::HashOfMaps(_) => {
                let hash: HashOfMaps<_, u32, MapData> = outer.try_into().map_err(map_error)?;
                hash.get(&entry.key, 0)
            }
            _ => return Err(LoadCause::Invalid("initializer outer type changed".into())),
        };
        let matches = match current {
            Ok(inner) => inner.info().map_err(map_error)?.id() == expected,
            Err(MapError::KeyNotFound) => false,
            Err(e) => return Err(map_error(e)),
        };
        if !matches {
            return Err(LoadCause::InitializerConflict {
                outer: entry.outer.clone(),
                key: entry.key,
            });
        }
    }

    for entry in entries.iter().filter(|entry| !reused(&entry.outer)) {
        let inner = bpf
            .map(&entry.inner)
            .ok_or_else(|| map_error(MapError::IoError(invalid())))?;
        let fd = crate::object::map_data(inner)
            .fd()
            .as_fd()
            .try_clone_to_owned()
            .map_err(|e| map_error(MapError::IoError(e)))?;
        let inner = MapData::from_fd(fd).map_err(map_error)?;
        let outer = bpf
            .map_mut(&entry.outer)
            .ok_or_else(|| map_error(MapError::IoError(invalid())))?;
        match outer {
            Map::ArrayOfMaps(_) => {
                let mut array: ArrayOfMaps<_, MapData> = outer.try_into().map_err(map_error)?;
                array.set(entry.key, &inner, 0).map_err(map_error)?;
            }
            Map::HashOfMaps(_) => {
                let mut hash: HashOfMaps<_, u32, MapData> = outer.try_into().map_err(map_error)?;
                hash.insert(entry.key, &inner, 0).map_err(map_error)?;
            }
            _ => return Err(map_error(MapError::IoError(invalid()))),
        }
    }
    Ok(())
}
