use super::*;
use alloc::{vec, vec::Vec};

fn cache(entries: &[(&[u8], &[u8])]) -> Vec<u8> {
    let mut data = b"glibc-ld.so.cache1.1".to_vec();
    data.extend_from_slice(&(entries.len() as u32).to_ne_bytes());
    data.resize(48 + entries.len() * 24, 0);
    for (index, (key, path)) in entries.iter().enumerate() {
        let key_offset = data.len() as u32;
        data.extend_from_slice(key);
        data.push(0);
        let path_offset = data.len() as u32;
        data.extend_from_slice(path);
        data.push(0);
        data[48 + index * 24 + 4..48 + index * 24 + 8].copy_from_slice(&key_offset.to_ne_bytes());
        data[48 + index * 24 + 8..48 + index * 24 + 12].copy_from_slice(&path_offset.to_ne_bytes());
    }
    data
}

#[test]
fn library_names_match_the_go_reference() {
    for (name, query, matches) in [
        ("libc.so.6", "libc", true),
        ("libc.so.6", "libc.so", true),
        ("libc.so.6", "libc.so.6", true),
        ("libc.so.6", "lib", false),
        ("libc.so.6", "libcrypto", false),
        ("libold-2.31.so", "libold", true),
        ("libstdc++.so.6", "libstdc++", true),
        ("binary", "binary", false),
        ("libc.so.6", "/lib/libc.so.6", false),
        ("libélan.so.1", "libélan", true),
        ("libname@version.so.1", "libname@version", true),
    ] {
        assert_eq!(
            matches_library(name.as_bytes(), query.as_bytes()),
            matches,
            "{name} {query}"
        );
    }
}

#[test]
fn maps_select_first_absolute_library_and_preserve_native_paths() {
    let maps = b"00-01 r--p 0 00:00 0 /usr/bin/binary\n01-02 rw-p 0 00:00 0\n02-03 r-xp 0 00:00 1 [vdso]\n03-04 r-xp 0 00:00 2 /lib/libc.so.6\n04-05 r-xp 0 00:00 2 /other/libc.so.6\n05-06 r-xp 0 00:00 3 /lib/libold-2.31.so (deleted)\n06-07 r-xp 0 00:00 4 /native-\xff/libnative.so.1\n";
    for query in ["libc", "libc.so", "libc.so.6"] {
        assert_eq!(
            uprobe_library_from_maps(maps, query),
            Some(b"/lib/libc.so.6".as_slice())
        );
    }
    assert_eq!(
        uprobe_library_from_maps(maps, "libold"),
        Some(b"/lib/libold-2.31.so".as_slice())
    );
    assert_eq!(
        uprobe_library_from_maps(maps, "libnative"),
        Some(b"/native-\xff/libnative.so.1".as_slice())
    );
    for query in ["binary", "libmissing", "/lib/libc.so.6"] {
        assert_eq!(uprobe_library_from_maps(maps, query), None);
    }
}

#[test]
fn cache_preserves_order_and_handles_exact_sonames_and_no_match() -> Result<(), InvalidUprobeCache>
{
    let data = cache(&[
        (b"libcrypto.so.3", b"/lib/libcrypto.so.3"),
        (b"libc.so.6", b"/preferred/libc.so.6"),
        (b"libc.so.6", b"/other/libc.so.6"),
    ]);
    for query in ["libc", "libc.so", "libc.so.6"] {
        assert_eq!(
            uprobe_library_from_cache(&data, query)?,
            Some(b"/preferred/libc.so.6".as_slice())
        );
    }
    assert_eq!(uprobe_library_from_cache(&data, "libcr")?, None);
    assert_eq!(uprobe_library_from_cache(&cache(&[]), "libc")?, None);
    assert_eq!(
        uprobe_library_from_cache(
            &cache(&[(b"libnative.so.1", b"/native-\xff/libnative.so.1")]),
            "libnative"
        )?,
        Some(b"/native-\xff/libnative.so.1".as_slice())
    );
    Ok(())
}

#[test]
fn corrupt_caches_fail_without_panics_or_partial_matches() {
    let valid = cache(&[(b"libc.so.6", b"/lib/libc.so.6")]);
    for length in 0..valid.len() {
        assert!(
            uprobe_library_from_cache(&valid[..length], "libc").is_err(),
            "truncation {length}"
        );
    }
    let mut variants = vec![b"ld.so-1.7.0\0".to_vec(), b"garbage".to_vec()];
    for (offset, value) in [
        (20, u32::MAX),
        (52, u32::MAX),
        (56, u32::MAX),
        (52, 0),
        (56, 48),
    ] {
        let mut data = valid.clone();
        data[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
        variants.push(data);
    }
    variants.push(cache(&[(b"libc.so.6", b"relative/path")]));
    let mut trailing_bad = cache(&[
        (b"libc.so.6", b"/lib/libc.so.6"),
        (b"libother.so.1", b"/lib/libother.so.1"),
    ]);
    trailing_bad.pop();
    variants.push(trailing_bad);
    for data in variants {
        assert!(uprobe_library_from_cache(&data, "libc").is_err());
    }
}
