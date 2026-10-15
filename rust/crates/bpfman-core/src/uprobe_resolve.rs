//! Pure library lookup over observed proc maps and glibc cache bytes.
use core::fmt;

/// Malformed or unsupported dynamic-linker cache. No file I/O is performed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidUprobeCache(&'static str);

impl fmt::Display for InvalidUprobeCache {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl core::error::Error for InvalidUprobeCache {}

/// Find the first absolute mapped library path matching a bare query.
/// Matches sonames, `.so`-stripped names and the older `libname-VERSION.so`
/// layout. Plain executable basenames and absolute queries do not match.
/// Paths remain bytes, preserving the observation without lossy conversion.
pub fn uprobe_library_from_maps<'a>(maps: &'a [u8], query: &str) -> Option<&'a [u8]> {
    for line in maps.split(|byte| *byte == b'\n') {
        let Some(path) = line
            .split(u8::is_ascii_whitespace)
            .filter(|field| !field.is_empty())
            .nth(5)
        else {
            continue;
        };
        if !path.starts_with(b"/") {
            continue;
        }
        let name = path.rsplit(|byte| *byte == b'/').next()?;
        if matches_library(name, query.as_bytes()) {
            return Some(path);
        }
    }
    None
}

/// Resolve a bare library query against a new-format glibc `ld.so.cache`.
/// The complete entry table and every string are validated before returning a
/// first match, preserving ldconfig's preferred order. Offsets use native endian,
/// as in the Go reference. Old standalone caches are explicitly unsupported.
/// No library code or constructors are executed.
pub fn uprobe_library_from_cache<'a>(
    data: &'a [u8],
    query: &str,
) -> Result<Option<&'a [u8]>, InvalidUprobeCache> {
    const MAGIC: &[u8] = b"glibc-ld.so.cache1.1";
    const TABLE: usize = 48;
    const ENTRY: usize = 24;
    if data.starts_with(b"ld.so-1.7.0\0") {
        return Err(InvalidUprobeCache(
            "old-format ld.so.cache is not supported",
        ));
    }
    if !data.starts_with(MAGIC) {
        return Err(InvalidUprobeCache("not a new-format glibc ld.so.cache"));
    }
    let count = integer(data, MAGIC.len())? as usize;
    let end = count
        .checked_mul(ENTRY)
        .and_then(|size| TABLE.checked_add(size))
        .filter(|end| *end <= data.len())
        .ok_or(InvalidUprobeCache(
            "ld.so.cache entry table exceeds file size",
        ))?;
    let mut found = None;
    for entry in data
        .get(TABLE..end)
        .ok_or(InvalidUprobeCache("truncated ld.so.cache header"))?
        .chunks_exact(ENTRY)
    {
        let key = string(data, integer(entry, 4)? as usize, end)?;
        let path = string(data, integer(entry, 8)? as usize, end)?;
        if !path.starts_with(b"/") {
            return Err(InvalidUprobeCache(
                "ld.so.cache library path must be absolute",
            ));
        }
        if found.is_none() && matches_library(key, query.as_bytes()) {
            found = Some(path);
        }
    }
    Ok(found)
}

fn integer(data: &[u8], offset: usize) -> Result<u32, InvalidUprobeCache> {
    let bytes = data
        .get(offset..offset + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(InvalidUprobeCache("truncated ld.so.cache integer"))?;
    Ok(u32::from_ne_bytes(bytes))
}

fn string(data: &[u8], offset: usize, table_end: usize) -> Result<&[u8], InvalidUprobeCache> {
    if offset < table_end {
        return Err(InvalidUprobeCache(
            "ld.so.cache string offset overlaps entry table",
        ));
    }
    let tail = data.get(offset..).ok_or(InvalidUprobeCache(
        "ld.so.cache string offset exceeds file size",
    ))?;
    let end = tail
        .iter()
        .position(|byte| *byte == 0)
        .ok_or(InvalidUprobeCache("unterminated ld.so.cache string"))?;
    Ok(&tail[..end])
}

fn matches_library(name: &[u8], query: &[u8]) -> bool {
    if name == query {
        return name.windows(3).any(|part| part == b".so");
    }
    let stem = query.strip_suffix(b".so").unwrap_or(query);
    name.strip_prefix(stem)
        .is_some_and(|rest| rest.starts_with(b".so") || rest.starts_with(b"-"))
}

#[cfg(test)]
#[path = "uprobe_resolve_tests.rs"]
mod tests;
