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
