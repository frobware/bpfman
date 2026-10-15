//! Confined shared-map creation and retained, existing-only loader sources.
use crate::MapPinning;
use crate::{
    Error, MapPin, PinSource, PinTarget, PreparedLoad, RuntimeWriter,
    artifacts::{entry, proc_path, validate_map_name},
    directory::{CONFINED, DIRECTORY, ensure_directory},
    error::{Failure, io},
    observe::observe,
    removal::open_owned,
};
use bpfman_core::EffectFailure;
use rustix::fs::{Mode, openat2};
use std::{num::NonZeroU32, os::fd::OwnedFd, path::PathBuf};

/// Map creation and compatibility checks at a filesystem-authorized source.
pub trait MapDefinition {
    /// Owned, unpinned map handle.
    type Map: crate::MapPinning;
    /// Create without making any persistent pins.
    fn create(&self) -> crate::KernelResult<Self::Map>;
    /// Require compatible type, key/value size, capacity and flags.
    fn validate(&self, source: PinSource<'_>) -> crate::KernelResult<()>;
}

impl PreparedLoad {
    /// Share a compatible existing pin or acquire one new pin, retaining partial
    /// ownership. Existing pins are never returned as new cleanup authority.
    pub fn prepare_shared_map(
        &self,
        writer: &RuntimeWriter<'_>,
        name: &str,
        definition: &impl MapDefinition,
    ) -> Result<Option<MapPin>, EffectFailure<Option<MapPin>, Error>> {
        let fail = |cause| EffectFailure {
            cause,
            remaining: None,
        };
        self.check(writer).map_err(fail)?;
        validate_map_name(name).map_err(fail)?;
        let shared = ensure_directory(&self.bpffs, "shared", CONFINED).map_err(fail)?;
        if let Some(receipt) = observe(writer, &shared, "fs/shared", name, false).map_err(fail)? {
            let fd = open_owned(&receipt).map_err(fail)?;
            definition
                .validate(PinSource(&proc_path(&fd)))
                .map_err(|e| fail(Failure::Kernel(e).into()))?;
            open_owned(&receipt).map_err(fail)?;
            return Ok(None);
        }
        let map = definition
            .create()
            .map_err(|e| fail(Failure::Kernel(e).into()))?;
        let owned = entry(writer, &shared, "fs/shared".into(), name.into(), false).map_err(fail)?;
        map.pin(PinTarget(&proc_path(&shared).join(name)))
            .map_err(|e| fail(Failure::Kernel(e).into()))?;
        let mut pin = MapPin {
            entry: Box::new(owned),
        };
        match pin.entry.observe() {
            Ok(()) => Ok(Some(pin)),
            Err(cause) => Err(EffectFailure {
                cause,
                remaining: Some(pin),
            }),
        }
    }
}

impl RuntimeWriter<'_> {
    /// Correlate canonical map-set aliases with shared-name pins by exact map ID.
    /// Matching names alone never establish a shared reference.
    pub fn shared_map_names(
        &self,
        kernel: &impl crate::ProgramInspection,
        set: NonZeroU32,
    ) -> Result<Vec<String>, Error> {
        let maps = self.directory().read_map_pins(kernel, set)?;
        let Some(bpffs) =
            crate::observe::optional_dir(&self.runtime.root, "fs", crate::directory::BENEATH)?
        else {
            return Ok(Vec::new());
        };
        let Some(shared) = crate::observe::optional_dir(&bpffs, "shared", CONFINED)? else {
            return Ok(Vec::new());
        };
        let mut names = Vec::new();
        for map in maps {
            if let Some(receipt) = observe(self, &shared, "fs/shared", &map.name, false)? {
                let pin = open_owned(&receipt)?;
                let id = kernel
                    .map_at(PinSource(&proc_path(&pin)))
                    .map_err(Failure::Kernel)?;
                if id == map.id {
                    names.push(map.name);
                }
            }
        }
        Ok(names)
    }

    /// Open every reused map by descriptor beneath the current runtime. Only
    /// existing pins can reach the callback; no caller-selected paths are accepted.
    pub fn with_load_maps<T>(
        &self,
        owner: Option<NonZeroU32>,
        maps: &[String],
        shared: &[String],
        action: impl FnOnce(&[(String, PinSource<'_>)]) -> crate::KernelResult<T>,
    ) -> Result<T, Error> {
        let bpffs = self.prepare_load()?;
        let mut sources = Vec::<(String, OwnedFd, PathBuf)>::new();
        for name in maps {
            if owner.is_none() && !shared.contains(name) {
                continue;
            }
            validate_map_name(name)?;
            let (parent, parent_path) = match owner {
                Some(id) => (
                    openat2(
                        &bpffs.maps,
                        id.to_string(),
                        DIRECTORY,
                        Mode::empty(),
                        CONFINED,
                    )
                    .map_err(|e| io("open owner map set", e))?,
                    format!("fs/maps/{id}"),
                ),
                None => (
                    openat2(&bpffs.bpffs, "shared", DIRECTORY, Mode::empty(), CONFINED)
                        .map_err(|e| io("open shared map pins", e))?,
                    "fs/shared".into(),
                ),
            };
            let receipt = observe(self, &parent, &parent_path, name, false)?
                .ok_or(Failure::Unsafe("required shared map pin is missing"))?;
            let fd = open_owned(&receipt)?;
            let path = proc_path(&fd);
            sources.push((name.clone(), fd, path));
        }
        let pins: Vec<_> = sources
            .iter()
            .map(|(name, _, path)| (name.clone(), PinSource(path)))
            .collect();
        action(&pins).map_err(|e| Failure::Kernel(e).into())
    }
}

impl MapPin {
    /// Map-set identity of a per-set pin; shared-name pins have no map-set owner.
    pub fn map_set(&self) -> Option<NonZeroU32> {
        self.entry
            .parent_path
            .strip_prefix("fs/maps/")?
            .parse()
            .ok()
    }

    /// Shared name, distinct from per-map-set aliases.
    pub fn shared_name(&self) -> Option<&str> {
        (self.entry.parent_path == "fs/shared").then_some(self.entry.name.as_str())
    }

    /// Check the original root, parent and inode without removing anything.
    pub fn validate(&self, writer: &RuntimeWriter<'_>) -> Result<(), Error> {
        self.entry.check_writer(writer)?;
        open_owned(&self.entry)?;
        Ok(())
    }
}

impl crate::MapDirectory {
    /// Canonical map-set identity of this owned directory.
    pub fn map_set(&self) -> Option<NonZeroU32> {
        self.entry.name.parse().ok()
    }

    /// Check the original root, parent and inode without removing anything.
    pub fn validate(&self, writer: &RuntimeWriter<'_>) -> Result<(), Error> {
        self.entry.check_writer(writer)?;
        open_owned(&self.entry)?;
        Ok(())
    }
}
