//! Checked ELF map references. Only newly owned outer maps may be populated.

use aya::maps::{ArrayOfMaps, HashOfMaps, Map, MapData, MapError};
use aya_obj::generated::bpf_map_type::{BPF_MAP_TYPE_ARRAY_OF_MAPS, BPF_MAP_TYPE_HASH_OF_MAPS};
use object::{Object, ObjectSection, ObjectSymbol};
use std::{collections::BTreeSet, io, os::fd::AsFd};

pub(super) struct Entry {
    pub(super) outer: String,
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

pub(super) fn populate(bpf: &mut aya::Ebpf, entries: &[Entry]) -> Result<(), MapError> {
    for entry in entries {
        let inner = bpf
            .map(&entry.inner)
            .ok_or_else(|| MapError::IoError(invalid()))?;
        let fd = crate::object::map_data(inner)
            .fd()
            .as_fd()
            .try_clone_to_owned()?;
        let inner = MapData::from_fd(fd)?;
        let outer = bpf
            .map_mut(&entry.outer)
            .ok_or_else(|| MapError::IoError(invalid()))?;
        match outer {
            Map::ArrayOfMaps(_) => {
                let mut array: ArrayOfMaps<_, MapData> = outer.try_into()?;
                array.set(entry.key, &inner, 0)?;
            }
            Map::HashOfMaps(_) => {
                let mut hash: HashOfMaps<_, u32, MapData> = outer.try_into()?;
                hash.insert(entry.key, &inner, 0)?;
            }
            _ => return Err(MapError::IoError(invalid())),
        }
    }
    Ok(())
}
