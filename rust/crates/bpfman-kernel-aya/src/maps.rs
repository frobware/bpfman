//! Shared-map compatibility and exact identities, independent of map contents.
use crate::{
    Kernel,
    failure::{LoadCause, filesystem},
};
use aya::maps::{MapData, MapInfo};
use bpfman_fs::{KernelResult, MapDefinition, MapPinning, PinSource, PinTarget, RuntimeWriter};
use bpfman_kernel::Error;
use std::{collections::BTreeMap, num::NonZeroU32};

pub(super) struct Definition<'a> {
    pub(super) btf: Option<std::os::fd::BorrowedFd<'a>>,
    pub(super) name: &'a str,
    pub(super) map: &'a aya_obj::Map,
}

impl MapDefinition for Definition<'_> {
    type Map = SharedMap;

    fn create(&self) -> KernelResult<SharedMap> {
        let map = match self.map.inner() {
            Some(inner) => {
                use std::os::fd::AsFd;

                // Aya's standalone public constructor omits inner_map_fd.
                // Keep the unpinned template alive through outer creation;
                // neither it nor its descriptor escapes this operation.
                let template = MapData::create(inner, "bpfman_inner", self.btf)?;
                let fd =
                    crate::pin_syscall::map_of_maps(self.map, self.name, template.fd().as_fd())?;
                MapData::from_fd(fd)?
            }
            None => MapData::create(self.map.clone(), self.name, self.btf)?,
        };
        Ok(SharedMap(map))
    }

    fn validate(&self, source: PinSource<'_>) -> KernelResult<()> {
        let info = MapInfo::from_pin(source.path())?;
        let (_, native) = crate::syscall::map(info.id())?;
        let kind = info.map_type()? as u32;
        // Aya's reuse path does not compare dimensions. Require them before
        // giving it an existing-only opened pin, rather than relying on verifier
        // rejection (which cannot distinguish all incompatible maps).
        // Like cilium/ebpf, compare observable outer attributes. Linux does not
        // expose an empty outer map's inner template through map info; inner
        // insertion compatibility remains enforced by the kernel.
        if kind != self.map.map_type()
            || info.key_size() != self.map.key_size()
            || info.value_size() != self.map.value_size()
            || info.max_entries() != self.map.max_entries()
            || info.map_flags() != self.map.map_flags()
            || native.map_extra != self.map.map_extra()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("incompatible shared map {:?}", self.name),
            )
            .into());
        }
        Ok(())
    }
}

pub(super) struct SharedMap(MapData);
impl MapPinning for SharedMap {
    fn pin(&self, target: PinTarget<'_>) -> KernelResult<()> {
        self.0.pin(target.path()).map_err(Into::into)
    }
}

pub(super) fn inventory(
    writer: &RuntimeWriter<'_>,
    id: NonZeroU32,
    set: NonZeroU32,
) -> Result<Option<BTreeMap<String, u32>>, Error> {
    let Some(bytes) = writer.read_provenance(id).map_err(filesystem)? else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| {
        Error::new(
            bpfman_kernel::ErrorKind::InvalidData,
            "decode map provenance",
            e,
        )
    })?;
    let Some(maps) = value.get("map_inventory") else {
        return Ok(None);
    };
    if value.get("program_id").and_then(|v| v.as_u64()) != Some(u64::from(id.get()))
        || value.get("map_set").and_then(|v| v.as_u64()) != Some(u64::from(set.get()))
    {
        return Err(LoadCause::Invalid(
            "map provenance has a different program or map-set identity".into(),
        )
        .into());
    }
    let maps: BTreeMap<String, NonZeroU32> = serde_json::from_value(maps.clone()).map_err(|e| {
        Error::new(
            bpfman_kernel::ErrorKind::InvalidData,
            "decode map inventory",
            e,
        )
    })?;
    if maps.keys().any(|name| {
        name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    }) {
        return Err(LoadCause::Invalid("invalid map inventory name".into()).into());
    }
    Ok(Some(
        maps.into_iter()
            .map(|(name, id)| (name, id.get()))
            .collect(),
    ))
}

impl Kernel {
    pub(super) fn prepare_shared_maps(
        &self,
        writer: &RuntimeWriter<'_>,
        prepared: &bpfman_fs::PreparedLoad,
        bytes: &[u8],
        names: &[String],
    ) -> Result<Vec<bpfman_fs::MapPin>, bpfman_core::EffectFailure<Vec<bpfman_fs::MapPin>, Error>>
    {
        let mut acquired = Vec::new();
        let mut object = aya_obj::Object::parse(bytes).map_err(|e| bpfman_core::EffectFailure {
            cause: Error::from(LoadCause::Parse(Box::new(e))),
            remaining: Vec::new(),
        })?;
        use std::os::fd::AsFd;
        // Retain DATASEC/VAR so Aya fixes their ELF extents and symbol offsets.
        // The unsupported-DATASEC fallback converts sections to structs sized by
        // member count, which cannot represent padded/multi-variable .rodata.
        // Lower optional newer kinds while preserving map key/value type IDs.
        // Keep the original ELF unchanged for loading and provenance.
        let btf = if names.is_empty() {
            None
        } else {
            object
                .fixup_and_sanitize_btf(&aya_obj::btf::BtfFeatures::new(
                    false, false, true, false, false, false, false, false,
                ))
                .map_err(|e| bpfman_core::EffectFailure {
                    cause: Error::new(
                        bpfman_kernel::ErrorKind::InvalidInput,
                        "sanitize shared-map BTF",
                        e,
                    ),
                    remaining: Vec::new(),
                })?
                .map(|btf| crate::pin_syscall::load_btf(&btf.to_bytes()))
                .transpose()
                .map_err(|e| bpfman_core::EffectFailure {
                    cause: Error::new(
                        bpfman_kernel::ErrorKind::Unavailable,
                        "load shared-map BTF",
                        e,
                    ),
                    remaining: Vec::new(),
                })?
        };
        for name in names {
            let Some(map) = object.maps.get(name) else {
                return Err(bpfman_core::EffectFailure {
                    cause: LoadCause::Invalid("shared map disappeared".into()).into(),
                    remaining: acquired,
                });
            };
            match prepared.prepare_shared_map(
                writer,
                name,
                &Definition {
                    name,
                    map,
                    btf: btf.as_ref().map(|fd| fd.as_fd()),
                },
            ) {
                Ok(pin) => acquired.extend(pin),
                Err(failure) => {
                    acquired.extend(failure.remaining);
                    return Err(bpfman_core::EffectFailure {
                        remaining: acquired,
                        cause: filesystem(failure.cause),
                    });
                }
            }
        }
        Ok(acquired)
    }
}

/// Retained map reference used to publish a CSI pin.
pub struct VolumeMap(MapData);

impl MapPinning for VolumeMap {
    fn pin(&self, target: PinTarget<'_>) -> KernelResult<()> {
        self.0.pin(target.path()).map_err(Into::into)
    }
}

impl bpfman_fs::MapReopening for Kernel {
    type Map = VolumeMap;

    fn open_map(&self, source: PinSource<'_>) -> KernelResult<Self::Map> {
        Ok(VolumeMap(MapData::from_pin(source.path())?))
    }
}
