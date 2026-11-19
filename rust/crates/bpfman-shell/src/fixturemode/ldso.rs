//! Minimal native-endian glibc cache for the container lookup fixtures.

use std::{fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt};

const MAGIC: &[u8] = b"glibc-ld.so.cache1.1";
const HEADER_BYTES: usize = 48;
const ENTRY_BYTES: usize = 24;

pub(super) fn run(arguments: &[String]) -> Result<(), String> {
    let [output, entries @ ..] = arguments else {
        return Err("ldso-cache-writer requires an output path and entries".to_owned());
    };
    if entries.is_empty() {
        return Err("ldso-cache-writer requires at least one soname=path entry".to_owned());
    }
    let cache = encode(entries)?;
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(output)
        .and_then(|mut file| file.write_all(&cache))
        .map_err(|error| format!("write cache {output:?}: {error}"))
}

fn encode(entries: &[String]) -> Result<Vec<u8>, String> {
    let count = u32::try_from(entries.len()).map_err(|_| "too many cache entries")?;
    let strings_start = entries
        .len()
        .checked_mul(ENTRY_BYTES)
        .and_then(|size| size.checked_add(HEADER_BYTES))
        .ok_or("cache entry table is too large")?;
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&count.to_ne_bytes());
    bytes.resize(HEADER_BYTES, 0);
    let mut strings = Vec::new();
    for entry in entries {
        let (name, path) = entry
            .split_once('=')
            .filter(|(name, path)| !name.is_empty() && !path.is_empty() && !entry.contains('\0'))
            .ok_or_else(|| format!("malformed entry {entry:?}, expected soname=path"))?;
        let name = append_string(&mut strings, strings_start, name)?;
        let path = append_string(&mut strings, strings_start, path)?;
        bytes.extend_from_slice(&1_u32.to_ne_bytes());
        bytes.extend_from_slice(&name.to_ne_bytes());
        bytes.extend_from_slice(&path.to_ne_bytes());
        bytes.extend_from_slice(&[0; 12]);
    }
    bytes.extend(strings);
    Ok(bytes)
}

fn append_string(bytes: &mut Vec<u8>, base: usize, value: &str) -> Result<u32, String> {
    let offset = base
        .checked_add(bytes.len())
        .and_then(|offset| u32::try_from(offset).ok())
        .ok_or("cache string offset is too large")?;
    bytes.extend_from_slice(value.as_bytes());
    bytes.push(0);
    Ok(offset)
}
