//! Admission uses real ELF/BTF metadata without opening kernel objects.

use bpfman_kernel::{ErrorKind, ObjectLoader};
use object::{Object, ObjectSection};
use std::{error::Error, fs, path::Path};

type Result = std::result::Result<(), Box<dyn Error>>;

fn fixture(name: &str) -> std::io::Result<Vec<u8>> {
    fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../e2e/testdata/bpf")
            .join(name),
    )
}

fn validate(bytes: &[u8]) -> std::result::Result<bpfman_kernel::ObjectInfo, bpfman_kernel::Error> {
    bpfman_kernel_aya::Kernel::default().validate_object(
        bytes,
        &bpfman_model::ProgramSpec::Kprobe("mom_seed_counter".try_into().map_err(|e| {
            bpfman_kernel::Error::new(ErrorKind::InvalidInput, "fixture symbol", e)
        })?),
    )
}

#[test]
fn sparse_initializers_are_validated_and_named_outer_maps_are_explicitly_unsupported() -> Result {
    let info = validate(&fixture("map_of_maps_initializers.bpf.o")?)?;
    assert_eq!(info.maps.len(), 4);
    assert!(info.shared_maps.is_empty());
    let failure = validate(&fixture("map_of_maps_initializers_shared.bpf.o")?)
        .err()
        .ok_or("expected refusal")?;
    assert_eq!(failure.kind(), ErrorKind::Unsupported);
    Ok(())
}

#[test]
fn malformed_relocations_cannot_be_silently_ignored() -> Result {
    let bytes = fixture("map_of_maps_initializers.bpf.o")?;
    let file = object::File::parse(bytes.as_slice())?;
    let (relocations, size) = file
        .section_by_name(".rel.maps")
        .ok_or("relocations")?
        .file_range()
        .ok_or("relocation data")?;
    assert!(size >= 32);
    let relocations = relocations as usize;
    let maps = file.section_by_name(".maps").ok_or("maps")?;
    let (maps_start, _) = maps.file_range().ok_or("map data")?;
    let offset = u64::from_le_bytes(bytes[relocations..relocations + 8].try_into()?);

    for mutation in 0..6 {
        let mut malformed = bytes.clone();
        match mutation {
            0 => {
                malformed[relocations..relocations + 8].copy_from_slice(&(offset + 1).to_le_bytes())
            }
            1 => malformed[relocations..relocations + 8]
                .copy_from_slice(&(maps.size() + 8).to_le_bytes()),
            2 => malformed[relocations + 8..relocations + 12].copy_from_slice(&1u32.to_le_bytes()),
            3 => malformed[relocations + 12..relocations + 16]
                .copy_from_slice(&u32::MAX.to_le_bytes()),
            4 => malformed[maps_start as usize + offset as usize] = 1,
            5 => malformed.copy_within(relocations..relocations + 16, relocations + 16),
            _ => return Err("unknown mutation".into()),
        }
        let failure = validate(&malformed)
            .err()
            .ok_or("expected malformed ELF refusal")?;
        assert_eq!(
            failure.kind(),
            ErrorKind::InvalidInput,
            "mutation {mutation}"
        );
    }
    Ok(())
}
