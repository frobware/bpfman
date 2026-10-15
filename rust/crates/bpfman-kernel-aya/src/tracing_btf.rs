//! Checked native-endian BTF lookup, including module string/type numbering.
use std::io;

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "malformed kernel BTF")
}

fn word(bytes: &[u8], offset: usize) -> io::Result<u32> {
    Ok(u32::from_ne_bytes(
        bytes
            .get(offset..offset + 4)
            .ok_or_else(invalid)?
            .try_into()
            .map_err(|_| invalid())?,
    ))
}

pub(super) struct Parts<'a> {
    types: &'a [u8],
    strings: &'a [u8],
}

impl<'a> Parts<'a> {
    pub(super) fn parse(bytes: &'a [u8]) -> io::Result<Self> {
        if bytes.get(..2) != Some(0xeb9fu16.to_ne_bytes().as_slice())
            || bytes.get(2..4) != Some(&[1, 0])
        {
            return Err(invalid());
        }
        let header = word(bytes, 4)? as usize;
        if header < 24 || header > bytes.len() {
            return Err(invalid());
        }
        let section = |off: usize, len: usize| {
            let start = header
                .checked_add(word(bytes, off)? as usize)
                .ok_or_else(invalid)?;
            let end = start
                .checked_add(word(bytes, len)? as usize)
                .ok_or_else(invalid)?;
            bytes.get(start..end).ok_or_else(invalid)
        };
        let types = section(8, 12)?;
        let strings = section(16, 20)?;
        let type_end = word(bytes, 8)?
            .checked_add(word(bytes, 12)?)
            .ok_or_else(invalid)?;
        if type_end > word(bytes, 16)? {
            return Err(invalid());
        }
        Ok(Self { types, strings })
    }

    /// Validate record extents and return the type count and unique function ID.
    /// Module IDs and string offsets continue after the vmlinux base.
    pub(super) fn function(
        &self,
        name: &str,
        base: Option<(&Self, u32)>,
    ) -> io::Result<(u32, Option<u32>)> {
        let base_count = base.map_or(0, |(_, count)| count);
        let base_strings = base.map_or(&[][..], |(parts, _)| parts.strings);
        let mut count = 0u32;
        let mut offset = 0usize;
        let mut found = None;
        while offset < self.types.len() {
            let info = word(self.types, offset + 4)?;
            // The fixed header includes a type/size union even for empty kinds.
            word(self.types, offset + 8)?;
            let kind = (info >> 24) & 0x1f;
            let vlen = (info & 0xffff) as usize;
            let extra = match kind {
                1 | 14 | 17 => 4,
                2 | 7..=12 | 16 | 18 => 0,
                3 => 12,
                4 | 5 | 15 | 19 => vlen * 12,
                6 | 13 => vlen * 8,
                _ => return Err(invalid()),
            };
            let end = offset
                .checked_add(12 + extra)
                .filter(|end| *end <= self.types.len())
                .ok_or_else(invalid)?;
            count = count.checked_add(1).ok_or_else(invalid)?;
            if kind == 12 {
                let name_offset = word(self.types, offset)? as usize;
                let tail = if name_offset < base_strings.len() {
                    &base_strings[name_offset..]
                } else {
                    self.strings
                        .get(name_offset - base_strings.len()..)
                        .ok_or_else(invalid)?
                };
                let end = tail.iter().position(|b| *b == 0).ok_or_else(invalid)?;
                if tail[..end] == *name.as_bytes() {
                    if found.is_some() {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "ambiguous kernel function in BTF",
                        ));
                    }
                    found = Some(base_count.checked_add(count).ok_or_else(invalid)?);
                }
            }
            offset = end;
        }
        Ok((count, found))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn btf(records: &[(u32, u32, u32)], strings: &[u8]) -> Vec<u8> {
        let mut bytes = vec![];
        bytes.extend_from_slice(&0xeb9fu16.to_ne_bytes());
        bytes.extend_from_slice(&[1, 0]);
        for value in [
            24,
            0,
            records.len() as u32 * 12,
            records.len() as u32 * 12,
            strings.len() as u32,
        ] {
            bytes.extend_from_slice(&value.to_ne_bytes());
        }
        for &(name, info, ty) in records {
            for value in [name, info, ty] {
                bytes.extend_from_slice(&value.to_ne_bytes());
            }
        }
        bytes.extend_from_slice(strings);
        bytes
    }

    #[test]
    fn split_ids_and_strings_continue_after_the_base() {
        let base = btf(&[(0, 2 << 24, 0), (1, 12 << 24, 1)], b"\0base\0");
        let base = Parts::parse(&base).expect("base");
        assert_eq!(base.function("base", None).expect("lookup"), (2, Some(2)));
        let module = btf(&[(6, 12 << 24, 1), (1, 12 << 24, 1)], b"module\0");
        let module = Parts::parse(&module).expect("module");
        assert_eq!(
            module
                .function("module", Some((&base, 2)))
                .expect("own string"),
            (2, Some(3))
        );
        assert_eq!(
            module
                .function("base", Some((&base, 2)))
                .expect("base string"),
            (2, Some(4))
        );
        assert_eq!(
            module
                .function("missing", Some((&base, 2)))
                .expect("missing"),
            (2, None)
        );
    }

    #[test]
    fn ambiguous_functions_and_malformed_later_records_are_rejected() {
        let bytes = btf(&[(1, 12 << 24, 0), (1, 12 << 24, 0)], b"\0name\0");
        assert!(
            Parts::parse(&bytes)
                .expect("header")
                .function("name", None)
                .is_err()
        );
        let bytes = btf(&[(1, 12 << 24, 0)], b"\0name\0");
        for end in 0..bytes.len() {
            assert!(Parts::parse(&bytes[..end]).is_err(), "truncation {end}");
        }
        for records in [
            vec![(1, 31 << 24, 0)],
            vec![(1, (4 << 24) | 1, 0)],
            vec![(99, 12 << 24, 0)],
            vec![(1, 12 << 24, 0), (0, 0, 0)],
        ] {
            let bytes = btf(&records, b"\0name\0");
            assert!(
                Parts::parse(&bytes)
                    .expect("header")
                    .function("name", None)
                    .is_err()
            );
        }
        let bytes = btf(&[(0, 12 << 24, 0)], b"unterminated");
        assert!(
            Parts::parse(&bytes)
                .expect("header")
                .function("unterminated", None)
                .is_err()
        );
    }
}
