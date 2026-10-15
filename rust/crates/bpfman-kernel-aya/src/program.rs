use crate::{
    Kernel, LoadedObject,
    failure::{filesystem, map},
};
use bpfman_fs::{MapDirectory, MapPin, PreparedLoad, ProgramPin, RuntimeWriter};
use bpfman_kernel::{Acquisition, Error, ProgramLoad, ProgramResources, Removal, UnloadArtifacts};
use bpfman_model::{ProgramSpec, Symbol};
use std::{collections::BTreeMap, num::NonZeroU32};

impl ProgramResources for Kernel {
    type ProgramPin = ProgramPin;
    type MapPin = MapPin;
    type MapDirectory = MapDirectory;

    fn validate_map(&self, w: &RuntimeWriter<'_>, pin: &MapPin) -> Result<(), Error> {
        pin.validate(w).map_err(filesystem)
    }
    fn validate_map_directory(
        &self,
        w: &RuntimeWriter<'_>,
        directory: &MapDirectory,
    ) -> Result<(), Error> {
        directory.validate(w).map_err(filesystem)
    }
    fn map_set(pin: &MapPin) -> Option<NonZeroU32> {
        pin.map_set()
    }
    fn shared_map(pin: &MapPin) -> Option<&str> {
        pin.shared_name()
    }
    fn directory_map_set(directory: &MapDirectory) -> Option<NonZeroU32> {
        directory.map_set()
    }
    fn observe_map_unload(
        &self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU32,
        set: NonZeroU32,
        remove_maps: bool,
        shared: &[String],
    ) -> Result<UnloadArtifacts<Self>, Error> {
        let inventory = crate::maps::inventory(w, id, set)?;
        let a = w
            .observe_map_unload(self, id, set, remove_maps, shared, inventory.as_ref())
            .map_err(filesystem)?;
        Ok(UnloadArtifacts {
            program: a.program,
            maps: a.maps,
            directory: a.directory,
            bytecode: a.bytecode,
        })
    }

    fn program_id(pin: &ProgramPin) -> NonZeroU32 {
        pin.id()
    }

    fn remove_program(&self, w: &RuntimeWriter<'_>, r: ProgramPin) -> Removal<ProgramPin> {
        w.remove_program_pin(r).map_err(map)
    }

    fn remove_map(&self, w: &RuntimeWriter<'_>, r: MapPin) -> Removal<MapPin> {
        w.remove_map_pin(r).map_err(map)
    }

    fn remove_map_directory(
        &self,
        w: &RuntimeWriter<'_>,
        r: MapDirectory,
    ) -> Removal<MapDirectory> {
        w.remove_empty_map_directory(r).map_err(map)
    }

    fn observe_unload(
        &self,
        w: &RuntimeWriter<'_>,
        id: NonZeroU32,
    ) -> Result<UnloadArtifacts<Self>, Error> {
        let a = w.observe_unload(self, id).map_err(filesystem)?;
        Ok(UnloadArtifacts {
            program: a.program,
            maps: a.maps,
            directory: a.directory,
            bytecode: a.bytecode,
        })
    }
}

impl ProgramLoad for Kernel {
    type Prepared = PreparedLoad;
    type Loaded = LoadedObject;

    fn prepare_load(&self, w: &RuntimeWriter<'_>) -> Result<PreparedLoad, Error> {
        w.prepare_load().map_err(filesystem)
    }

    fn load_program(
        &self,
        _w: &RuntimeWriter<'_>,
        bytes: &[u8],
        maps: &[String],
        globals: &BTreeMap<String, Vec<u8>>,
        spec: &ProgramSpec,
    ) -> Result<LoadedObject, Error> {
        crate::object::load(bytes, maps, globals, spec)
    }

    fn prepare_maps(
        &self,
        w: &RuntimeWriter<'_>,
        p: &PreparedLoad,
        bytes: &[u8],
        shared: &[String],
        owner: Option<NonZeroU32>,
    ) -> Result<Vec<MapPin>, bpfman_core::EffectFailure<Vec<MapPin>, Error>> {
        if owner.is_some() {
            return Ok(Vec::new());
        }
        self.prepare_shared_maps(w, p, bytes, shared)
    }

    #[allow(clippy::too_many_arguments)]
    fn load_program_with_maps(
        &self,
        w: &RuntimeWriter<'_>,
        bytes: &[u8],
        maps: &[String],
        globals: &BTreeMap<String, Vec<u8>>,
        spec: &ProgramSpec,
        owner: Option<NonZeroU32>,
        shared: &[String],
    ) -> Result<LoadedObject, Error> {
        if owner.is_none() && shared.is_empty() {
            return crate::object::load(bytes, maps, globals, spec);
        }
        w.with_load_maps(owner, maps, shared, |sources| {
            crate::object::load_with_maps(bytes, maps, globals, spec, sources).map_err(Into::into)
        })
        .map_err(filesystem)
    }

    fn shared_map_names(
        &self,
        w: &RuntimeWriter<'_>,
        set: NonZeroU32,
    ) -> Result<Vec<String>, Error> {
        w.shared_map_names(self, set).map_err(filesystem)
    }

    fn map_inventory(
        &self,
        w: &RuntimeWriter<'_>,
        k: &LoadedObject,
        owner: Option<NonZeroU32>,
    ) -> Result<BTreeMap<String, u32>, Error> {
        if let Some(owner) = owner {
            return w
                .directory()
                .read_map_pins(self, owner)
                .map(|maps| maps.into_iter().map(|m| (m.name, m.id)).collect())
                .map_err(filesystem);
        }
        k.maps
            .iter()
            .map(|name| {
                let map = k.bpf.map(name).ok_or_else(|| {
                    Error::from(crate::failure::LoadCause::Invalid(
                        "loaded map disappeared".into(),
                    ))
                })?;
                Ok((name.clone(), crate::object::map_id(map)?))
            })
            .collect()
    }

    fn map_names(k: &LoadedObject) -> &[String] {
        &k.maps
    }

    fn pin_program(
        &self,
        w: &RuntimeWriter<'_>,
        p: &PreparedLoad,
        k: &mut LoadedObject,
        name: &Symbol,
    ) -> Acquisition<ProgramPin> {
        if name.as_str() != k.name {
            return Err(bpfman_core::EffectFailure {
                cause: crate::failure::LoadCause::Invalid("selected program changed".into()).into(),
                remaining: None,
            });
        }
        p.pin_program(w, k).map_err(map)
    }

    fn create_map_directory(
        &self,
        w: &RuntimeWriter<'_>,
        p: &PreparedLoad,
        id: NonZeroU32,
    ) -> Acquisition<MapDirectory> {
        p.create_map_directory(w, id).map_err(map)
    }

    fn pin_map(
        &self,
        w: &RuntimeWriter<'_>,
        k: &LoadedObject,
        d: &MapDirectory,
        name: &str,
    ) -> Acquisition<MapPin> {
        let resource = k.bpf.map(name).ok_or_else(|| bpfman_core::EffectFailure {
            cause: Error::from(crate::failure::LoadCause::Invalid(
                "missing loaded map".into(),
            )),
            remaining: None,
        })?;
        d.pin_map(w, name, &crate::pinning::MapRef(resource))
            .map_err(map)
    }
}
