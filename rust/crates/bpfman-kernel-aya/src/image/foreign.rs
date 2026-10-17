//! Metadata-only foreign-endian ELF/BTF interpretation. Original bytes are never
//! rewritten. Every record, string reference and type reference is bounded.
use super::BytecodeMetadata;
use object::{Object, ObjectSection, ObjectSymbol};
use std::{collections::BTreeMap, io};

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "malformed bytecode metadata")
}
fn data_error(_: object::Error) -> io::Error {
    invalid()
}

#[derive(Clone, Copy)]
struct Order(bool);
impl Order {
    fn word(self, bytes: &[u8], offset: usize) -> io::Result<u32> {
        let word = bytes
            .get(offset..offset.checked_add(4).ok_or_else(invalid)?)
            .ok_or_else(invalid)?;
        let word = <[u8; 4]>::try_from(word).map_err(|_| invalid())?;
        Ok(if self.0 {
            u32::from_le_bytes(word)
        } else {
            u32::from_be_bytes(word)
        })
    }
}

struct Type<'a> {
    name: &'a str,
    kind: u32,
    ty: u32,
    data: &'a [u8],
}

struct Btf<'a> {
    order: Order,
    records: Vec<Type<'a>>,
    strings: &'a [u8],
}

impl<'a> Btf<'a> {
    fn string(&self, offset: u32) -> io::Result<&'a str> {
        let bytes = self.strings.get(offset as usize..).ok_or_else(invalid)?;
        let end = bytes.iter().position(|b| *b == 0).ok_or_else(invalid)?;
        std::str::from_utf8(&bytes[..end]).map_err(|_| invalid())
    }

    fn parse(bytes: &'a [u8], order: Order) -> io::Result<Self> {
        let magic = if order.0 { [0x9f, 0xeb] } else { [0xeb, 0x9f] };
        if bytes.get(..2) != Some(&magic) || bytes.get(2..4) != Some(&[1, 0]) {
            return Err(invalid());
        }
        let header = order.word(bytes, 4)? as usize;
        if header < 24 || header > bytes.len() {
            return Err(invalid());
        }
        let section = |offset, length| -> io::Result<&'a [u8]> {
            let start = header
                .checked_add(order.word(bytes, offset)? as usize)
                .ok_or_else(invalid)?;
            let end = start
                .checked_add(order.word(bytes, length)? as usize)
                .ok_or_else(invalid)?;
            bytes.get(start..end).ok_or_else(invalid)
        };
        let mut types = section(8, 12)?;
        let strings = section(16, 20)?;
        if order
            .word(bytes, 8)?
            .checked_add(order.word(bytes, 12)?)
            .ok_or_else(invalid)?
            > order.word(bytes, 16)?
            || strings.first() != Some(&0)
            || strings.last() != Some(&0)
        {
            return Err(invalid());
        }
        let mut btf = Self {
            order,
            strings,
            records: Vec::new(),
        };
        while !types.is_empty() {
            let info = order.word(types, 4)?;
            let kind = (info >> 24) & 31;
            let count = (info & 0xffff) as usize;
            let extra = match kind {
                1 | 14 | 17 => 4,
                2 | 7..=12 | 16 | 18 => 0,
                3 => 12,
                4 | 5 | 15 | 19 => count * 12,
                6 | 13 => count * 8,
                _ => return Err(invalid()),
            };
            let data = types.get(12..12 + extra).ok_or_else(invalid)?;
            btf.records.push(Type {
                name: btf.string(order.word(types, 0)?)?,
                kind,
                ty: order.word(types, 8)?,
                data,
            });
            types = &types[12 + extra..];
        }
        // Validate every reference, including records unrelated to exported maps.
        for record in &btf.records {
            let reference = |id| {
                if id as usize <= btf.records.len() {
                    Ok(())
                } else {
                    Err(invalid())
                }
            };
            match record.kind {
                2 | 8..=14 | 17 | 18 => reference(record.ty)?,
                _ => {}
            }
            match record.kind {
                3 => {
                    reference(order.word(record.data, 0)?)?;
                    reference(order.word(record.data, 4)?)?;
                }
                4 | 5 => {
                    for member in record.data.chunks_exact(12) {
                        btf.string(order.word(member, 0)?)?;
                        reference(order.word(member, 4)?)?;
                    }
                }
                6 | 13 | 19 => {
                    for member in record
                        .data
                        .chunks_exact(if record.kind == 19 { 12 } else { 8 })
                    {
                        btf.string(order.word(member, 0)?)?;
                        if record.kind == 13 {
                            reference(order.word(member, 4)?)?;
                        }
                    }
                }
                15 => {
                    for entry in record.data.chunks_exact(12) {
                        reference(order.word(entry, 0)?)?;
                    }
                }
                _ => {}
            }
        }
        Ok(btf)
    }

    fn get(&self, id: u32) -> io::Result<&Type<'a>> {
        id.checked_sub(1)
            .and_then(|id| self.records.get(id as usize))
            .ok_or_else(invalid)
    }

    fn extension(&self, bytes: &[u8]) -> io::Result<()> {
        let order = self.order;
        let magic = if order.0 { [0x9f, 0xeb] } else { [0xeb, 0x9f] };
        if bytes.get(..2) != Some(&magic) || bytes.get(2..4) != Some(&[1, 0]) {
            return Err(invalid());
        }
        let header = order.word(bytes, 4)? as usize;
        if header < 24 || header > bytes.len() {
            return Err(invalid());
        }
        for (offset, minimum) in [(8, 8), (16, 16), (24, 16)] {
            if offset + 8 > header {
                continue;
            }
            let length = order.word(bytes, offset + 4)? as usize;
            let start = header
                .checked_add(order.word(bytes, offset)? as usize)
                .ok_or_else(invalid)?;
            let end = start.checked_add(length).ok_or_else(invalid)?;
            let mut data = bytes.get(start..end).ok_or_else(invalid)?;
            if data.is_empty() {
                continue;
            }
            let size = order.word(data, 0)? as usize;
            if size < minimum {
                return Err(invalid());
            }
            data = &data[4..];
            while !data.is_empty() {
                self.string(order.word(data, 0)?)?;
                let count = order.word(data, 4)? as usize;
                let length = count
                    .checked_mul(size)
                    .and_then(|length| length.checked_add(8))
                    .ok_or_else(invalid)?;
                let records = data.get(8..length).ok_or_else(invalid)?;
                for record in records.chunks_exact(size) {
                    match offset {
                        8 => {
                            self.get(order.word(record, 4)?)?;
                        }
                        16 => {
                            self.string(order.word(record, 4)?)?;
                            self.string(order.word(record, 8)?)?;
                        }
                        24 => {
                            self.get(order.word(record, 4)?)?;
                            self.string(order.word(record, 8)?)?;
                        }
                        _ => {}
                    }
                }
                data = &data[length..];
            }
        }
        Ok(())
    }

    fn resolve(&self, mut id: u32) -> io::Result<&Type<'a>> {
        // Bound qualifier/typedef chains, including cyclic malformed input.
        for _ in 0..32 {
            let record = self.get(id)?;
            if matches!(record.kind, 8..=11 | 18) {
                id = record.ty;
            } else {
                return Ok(record);
            }
        }
        Err(invalid())
    }

    fn maps(&self) -> io::Result<BTreeMap<String, String>> {
        let mut maps = BTreeMap::new();
        for section in self
            .records
            .iter()
            .filter(|r| r.kind == 15 && r.name == ".maps")
        {
            for entry in section.data.chunks_exact(12) {
                let variable = self.get(self.order.word(entry, 0)?)?;
                if variable.kind != 14 {
                    return Err(invalid());
                }
                let definition = self.resolve(variable.ty)?;
                if definition.kind != 4 {
                    return Err(invalid());
                }
                let mut kind = None;
                for member in definition.data.chunks_exact(12) {
                    if self.string(self.order.word(member, 0)?)? != "type" {
                        continue;
                    }
                    let pointer = self.resolve(self.order.word(member, 4)?)?;
                    if pointer.kind != 2 {
                        return Err(invalid());
                    }
                    let array = self.resolve(pointer.ty)?;
                    if array.kind != 3 {
                        return Err(invalid());
                    }
                    if kind.replace(self.order.word(array.data, 8)?).is_some() {
                        return Err(invalid());
                    }
                }
                let kind = super::map_type(kind.ok_or_else(invalid)?).ok_or_else(invalid)?;
                if maps.insert(variable.name.into(), kind.into()).is_some() {
                    return Err(invalid());
                }
            }
        }
        Ok(maps)
    }
}

pub(super) fn inspect(bytes: &[u8], little: bool) -> io::Result<BytecodeMetadata> {
    let order = Order(little);
    let object = object::File::parse(bytes).map_err(data_error)?;
    let mut programs = BTreeMap::new();
    let mut maps = BTreeMap::new();
    if let Some(section) = object.section_by_name(".BTF") {
        let btf = Btf::parse(section.data().map_err(data_error)?, order)?;
        if let Some(extension) = object.section_by_name(".BTF.ext") {
            btf.extension(extension.data().map_err(data_error)?)?;
        }
        maps = btf.maps()?;
    } else if object.section_by_name(".maps").is_some()
        || object.section_by_name(".BTF.ext").is_some()
    {
        return Err(invalid());
    }
    for symbol in object.symbols().filter(|symbol| symbol.is_definition()) {
        let Some(index) = symbol.section_index() else {
            continue;
        };
        let section = object.section_by_index(index).map_err(data_error)?;
        let name = symbol.name().map_err(data_error)?;
        if name.is_empty() {
            continue;
        }
        let section_name = section.name().map_err(data_error)?;
        let start = symbol
            .address()
            .checked_sub(section.address())
            .ok_or_else(invalid)?;
        let end = start.checked_add(symbol.size()).ok_or_else(invalid)?;
        if end > section.size() {
            return Err(invalid());
        }
        if symbol.kind() == object::SymbolKind::Text && section.kind() == object::SectionKind::Text
        {
            if let Some(kind) = program_type(section_name) {
                if symbol.size() == 0 || symbol.size() % 8 != 0 {
                    return Err(invalid());
                }
                programs.insert(name.into(), kind.into());
            }
        } else if symbol.kind() == object::SymbolKind::Data && section_name.starts_with("maps") {
            let data = section
                .data()
                .map_err(data_error)?
                .get(start as usize..end as usize)
                .ok_or_else(invalid)?;
            if data.len() < 20 {
                return Err(invalid());
            }
            let kind = super::map_type(order.word(data, 0)?).ok_or_else(invalid)?;
            maps.insert(name.into(), kind.into());
        } else if symbol.kind() == object::SymbolKind::Data
            && section_name == ".maps"
            && !maps.contains_key(name)
        {
            return Err(invalid());
        }
    }
    if programs.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "no supported programs found in bytecode",
        ));
    }
    Ok(BytecodeMetadata { programs, maps })
}

fn program_type(section: &str) -> Option<&'static str> {
    let section = section.strip_prefix('?').unwrap_or(section);
    [
        "kretprobe",
        "kprobe",
        "uretprobe",
        "uprobe",
        "tracepoint",
        "fentry",
        "fexit",
        "lsm",
        "xdp",
        "tcx",
        "tc",
        "classifier",
    ]
    .into_iter()
    .find(|prefix| section.starts_with(prefix))
    .map(|kind| if kind == "classifier" { "tc" } else { kind })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn btf(little: bool) -> Vec<u8> {
        let strings = b"\0.maps\0counter\0type\0int\0";
        let records: &[u32] = &[
            20,
            1 << 24,
            4,
            32, // int (1)
            0,
            2 << 24,
            3, // ptr to array (2)
            0,
            3 << 24,
            0,
            1,
            1,
            6, // array length encodes per_cpu_array (3)
            0,
            (4 << 24) | 1,
            8,
            15,
            2,
            0, // struct { type: ptr } (4)
            7,
            14 << 24,
            4,
            1, // map variable (5)
            1,
            (15 << 24) | 1,
            8,
            5,
            0,
            8, // .maps datasec (6)
        ];
        let mut bytes = if little {
            vec![0x9f, 0xeb, 1, 0]
        } else {
            vec![0xeb, 0x9f, 1, 0]
        };
        for value in [
            24,
            0,
            records.len() as u32 * 4,
            records.len() as u32 * 4,
            strings.len() as u32,
        ]
        .into_iter()
        .chain(records.iter().copied())
        {
            bytes.extend_from_slice(&if little {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            });
        }
        bytes.extend_from_slice(strings);
        bytes
    }

    #[test]
    fn map_labels_match_in_both_orders_and_malformed_records_fail() {
        for little in [false, true] {
            let bytes = btf(little);
            let maps = Btf::parse(&bytes, Order(little))
                .expect("BTF")
                .maps()
                .expect("maps");
            assert_eq!(
                maps.get("counter").map(String::as_str),
                Some("per_cpu_array")
            );
            for length in 0..bytes.len() {
                assert!(
                    Btf::parse(&bytes[..length], Order(little)).is_err(),
                    "truncation {length}"
                );
            }
            let mut invalid_ref = bytes.clone();
            // ptr target is not a valid type ID.
            invalid_ref[48..52].copy_from_slice(&if little {
                99u32.to_le_bytes()
            } else {
                99u32.to_be_bytes()
            });
            assert!(Btf::parse(&invalid_ref, Order(little)).is_err());
            // A typedef cycle must stop when resolving the map definition.
            let mut cycle = Btf::parse(&bytes, Order(little)).expect("BTF");
            cycle.records[3].kind = 8;
            cycle.records[3].ty = 4;
            assert!(cycle.maps().is_err());
        }
    }

    #[test]
    fn extension_counts_strings_and_spans_are_bounded() {
        for little in [false, true] {
            let bytes = btf(little);
            let btf = Btf::parse(&bytes, Order(little)).expect("BTF");
            let make = |count: u32| {
                let mut ext = if little {
                    vec![0x9f, 0xeb, 1, 0]
                } else {
                    vec![0xeb, 0x9f, 1, 0]
                };
                // func-info with one record referencing the known map-struct ID.
                for value in [24, 0, 20, 20, 0, 8, 1, count, 0, 4] {
                    ext.extend_from_slice(&if little {
                        value.to_le_bytes()
                    } else {
                        value.to_be_bytes()
                    });
                }
                ext
            };
            let ext = make(1);
            assert!(btf.extension(&ext).is_ok());
            for length in 0..ext.len() {
                assert!(btf.extension(&ext[..length]).is_err());
            }
            assert!(btf.extension(&make(u32::MAX)).is_err());
        }
    }
}
