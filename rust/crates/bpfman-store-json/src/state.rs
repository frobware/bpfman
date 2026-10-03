//! Version 1 supports private, unattached, locally loaded tracepoints only.
//! Paths are derived from the runtime layout; serialized values never authorize I/O.

use crate::error::Failure;
use bpfman_fs::RuntimeLayout;
use bpfman_model::{
    ProgramSource, ProgramSpec, ProgramType, StoredProgram, StoredProgramSummary, Symbol,
};
use bpfman_store::TracepointRecord;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    num::NonZeroU32,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct State {
    pub(super) version: u32,
    pub(super) identity: String,
    pub(super) next_generation: u64,
    pub(super) programs: Vec<Tracepoint>,
    pub(super) map_sets: Vec<MapSet>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Tracepoint {
    pub(super) id: NonZeroU32,
    pub(super) generation: u64,
    pub(super) name: String,
    pub(super) source: String,
    pub(super) license: String,
    pub(super) created_at: String,
    pub(super) metadata: BTreeMap<String, String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MapSet {
    pub(super) id: NonZeroU32,
    pub(super) generation: u64,
}

impl State {
    pub(super) fn empty() -> Result<Self, Failure> {
        let mut random = [0u8; 16];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut random)?;

        Ok(Self {
            version: 1,
            identity: random.iter().map(|b| format!("{b:02x}")).collect(),
            next_generation: 1,
            programs: Vec::new(),
            map_sets: Vec::new(),
        })
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, Failure> {
        // Check version before decoding version-specific fields.
        #[derive(Deserialize)]
        struct Header {
            version: u32,
        }

        let header: Header = serde_json::from_slice(bytes)?;
        if header.version != 1 {
            return Err(Failure::Version(header.version));
        }

        let state: Self = serde_json::from_slice(bytes)?;
        state.validate()?;
        Ok(state)
    }

    fn validate(&self) -> Result<(), Failure> {
        if self.next_generation == 0 {
            return Err(Failure::Invalid("invalid next generation"));
        }

        if self.identity.len() != 32 || !self.identity.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Failure::Invalid("invalid store identity"));
        }

        let mut map_ids = BTreeSet::new();
        let mut generations = BTreeSet::new();
        for map in &self.map_sets {
            if !map_ids.insert(map.id)
                || !generations.insert(map.generation)
                || map.generation == 0
                || map.generation >= self.next_generation
            {
                return Err(Failure::Invalid("duplicate or invalid map-set generation"));
            }
        }

        let mut program_ids = BTreeSet::new();
        for row in &self.programs {
            Symbol::try_from(row.name.as_str())
                .map_err(|_| Failure::Invalid("invalid ELF symbol"))?;
            timestamp(&row.created_at)?;
            if !program_ids.insert(row.id)
                || !self
                    .map_sets
                    .iter()
                    .any(|map| map.id == row.id && map.generation == row.generation)
            {
                return Err(Failure::Invalid(
                    "duplicate program or missing private map set",
                ));
            }
        }

        Ok(())
    }

    pub(super) fn insert(
        &mut self,
        record: TracepointRecord<'_>,
    ) -> Result<StoredProgramSummary, Failure> {
        if self.map_sets.iter().any(|m| m.id == record.id)
            || self.programs.iter().any(|p| p.id == record.id)
        {
            return Err(Failure::Invalid("program or map set already exists"));
        }

        let generation = self.next_generation;
        self.next_generation = generation
            .checked_add(1)
            .ok_or(Failure::Invalid("generation exhausted"))?;
        let row = Tracepoint {
            id: record.id,
            generation,
            name: record.name.as_str().into(),
            source: record.source.into(),
            license: record.license.into(),
            created_at: timestamp(record.created_at)?,
            metadata: record.metadata.clone(),
        };

        let summary = row.summary();
        self.map_sets.push(MapSet {
            id: row.id,
            generation,
        });
        self.programs.push(row);
        Ok(summary)
    }
}

impl Tracepoint {
    pub(super) fn summary(&self) -> StoredProgramSummary {
        StoredProgramSummary::new(
            self.id,
            self.name.clone(),
            ProgramType::Tracepoint,
            self.metadata.clone(),
            Vec::new(),
        )
    }

    pub(super) fn record(&self, layout: &RuntimeLayout) -> Result<StoredProgram, Failure> {
        let path = |p: std::path::PathBuf| {
            p.into_os_string()
                .into_string()
                .map_err(|_| Failure::Invalid("runtime path is not UTF-8"))
        };

        Ok(StoredProgram {
            id: self.id,
            spec: ProgramSpec::Tracepoint(
                Symbol::try_from(self.name.as_str())
                    .map_err(|_| Failure::Invalid("invalid ELF symbol"))?,
            ),
            source: ProgramSource::File(Some(self.source.clone())),
            object_path: path(layout.bytecode_path(self.id))?,
            pin_path: path(layout.program_pin_path(self.id))?,
            map_path: path(layout.map_directory_path(self.id))?,
            map_set: self.id,
            globals: BTreeMap::new(),
            license: self.license.clone(),
            gpl_compatible: matches!(
                self.license.as_str(),
                "GPL"
                    | "GPL v2"
                    | "GPL and additional rights"
                    | "Dual BSD/GPL"
                    | "Dual MIT/GPL"
                    | "Dual MPL/GPL"
            ),
            owner: String::new(),
            description: String::new(),
            metadata: self.metadata.clone(),
            created_at: timestamp(&self.created_at)?,
            updated_at: None,
            links: Vec::new(),
        })
    }
}

fn timestamp(raw: &str) -> Result<String, Failure> {
    use chrono::Datelike;

    let parsed = chrono::DateTime::parse_from_rfc3339(raw)
        .map_err(|_| Failure::Invalid("invalid RFC3339 timestamp"))?;
    if !(0..=9999).contains(&parsed.year()) {
        return Err(Failure::Invalid("timestamp year outside RFC3339 range"));
    }

    let base = parsed.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let nanos = parsed.timestamp_subsec_nanos();
    if nanos == 0 {
        return Ok(base);
    }

    let fraction = format!("{nanos:09}");
    Ok(format!(
        "{}.{}{}",
        &base[..19],
        fraction.trim_end_matches('0'),
        &base[19..]
    ))
}
