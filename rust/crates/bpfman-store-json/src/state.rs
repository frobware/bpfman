//! Version 3 adds XDP extension loads; version 2 adds standalone tracepoint links.
//! Version 1 retains its existing program operations; link creation requires a
//! separately initialized version 2 or newer store. Never upgrade a snapshot implicitly.
//! Paths are derived from the runtime layout; serialized values never authorize I/O.

use crate::error::Failure;
use bpfman_fs::RuntimeLayout;
use bpfman_model::{
    LinkDetails, LinkState, ProgramSource, ProgramSpec, ProgramType, StoredLink, StoredProgram,
    StoredProgramSummary, Symbol,
};
use bpfman_store::LoadRecord;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    num::{NonZeroU32, NonZeroU64},
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct State {
    pub(super) version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) xdp: Vec<crate::xdp::Row>,
    pub(super) identity: String,
    pub(super) next_generation: u64,
    pub(super) programs: Vec<Program>,
    pub(super) map_sets: Vec<MapSet>,
    #[serde(default = "first_link_id", skip_serializing_if = "is_first_link_id")]
    pub(super) next_link_id: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) links: Vec<Link>,
}

fn first_link_id() -> u64 {
    1
}

fn is_first_link_id(id: &u64) -> bool {
    *id == 1
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Link {
    pub(super) id: NonZeroU64,
    pub(super) program_id: NonZeroU32,
    pub(super) target: String,
    pub(super) state: LinkProgress,
    pub(super) metadata: BTreeMap<String, String>,
    pub(super) created_at: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum LinkProgress {
    Pending,
    Attached { kernel_id: NonZeroU32 },
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Program {
    #[serde(default, skip_serializing_if = "Kind::is_tracepoint")]
    pub(super) kind: Kind,
    pub(super) id: NonZeroU32,
    pub(super) generation: u64,
    pub(super) name: String,
    pub(super) source: String,
    pub(super) license: String,
    pub(super) created_at: String,
    pub(super) metadata: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) globals: BTreeMap<String, Vec<u8>>,
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
            version: 4,
            xdp: Vec::new(),
            identity: random.iter().map(|b| format!("{b:02x}")).collect(),
            next_generation: 1,
            programs: Vec::new(),
            map_sets: Vec::new(),
            next_link_id: 1,
            links: Vec::new(),
        })
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self, Failure> {
        // Check version before decoding version-specific fields.
        #[derive(Deserialize)]
        struct Header {
            version: u32,
        }

        let header: Header = serde_json::from_slice(bytes)?;

        if !matches!(header.version, 1..=4) {
            return Err(Failure::Version(header.version));
        }

        let state: Self = serde_json::from_slice(bytes)?;
        state.validate()?;

        Ok(state)
    }

    pub(super) fn validate(&self) -> Result<(), Failure> {
        if self.version == 1 && (!self.links.is_empty() || self.next_link_id != 1) {
            return Err(Failure::LinkVersion);
        }

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
            if self.version < 3 && row.kind == Kind::Xdp {
                return Err(Failure::XdpVersion);
            }
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

        if self.next_link_id == 0 || self.next_link_id > i64::MAX as u64 + 1 {
            return Err(Failure::Invalid("invalid next link ID"));
        }

        if self.version < 4 && !self.xdp.is_empty() {
            return Err(Failure::DispatcherVersion);
        }
        let mut link_ids = BTreeSet::new();
        let mut kernel_ids = BTreeSet::new();

        for link in &self.links {
            if !link_ids.insert(link.id)
                || link.id.get() >= self.next_link_id
                || !self
                    .programs
                    .iter()
                    .any(|p| p.id == link.program_id && p.kind == Kind::Tracepoint)
            {
                return Err(Failure::Invalid(
                    "duplicate link, invalid ID, or missing program",
                ));
            }

            if let LinkProgress::Attached { kernel_id } = link.state {
                if !kernel_ids.insert(kernel_id) {
                    return Err(Failure::Invalid("duplicate kernel link ID"));
                }
            }

            link.target
                .parse::<bpfman_model::Tracepoint>()
                .map_err(|_| Failure::Invalid("invalid tracepoint target"))?;
            timestamp(&link.created_at)?;
        }

        let mut keys = BTreeSet::new();
        let mut dispatchers = BTreeSet::new();
        for row in &self.xdp {
            if !keys.insert(row.key())
                || !link_ids.insert(row.link_id)
                || row.link_id.get() >= self.next_link_id
                || !kernel_ids.insert(row.extension_link_id)
                || !kernel_ids.insert(row.outer_link_id)
                || !self
                    .programs
                    .iter()
                    .any(|p| p.id == row.program_id && p.kind == Kind::Xdp)
            {
                return Err(Failure::Invalid(
                    "duplicate or invalid XDP snapshot identity",
                ));
            }
            let details = row.details()?;
            if !dispatchers.insert(details.dispatcher_id) {
                return Err(Failure::Invalid("duplicate dispatcher ID"));
            }
        }

        Ok(())
    }

    pub(super) fn insert(
        &mut self,
        record: &LoadRecord<'_>,
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
        let row = Program {
            kind: match record.spec {
                ProgramSpec::Tracepoint(_) => Kind::Tracepoint,
                ProgramSpec::Xdp(_) if self.version >= 3 => Kind::Xdp,
                ProgramSpec::Xdp(_) => return Err(Failure::XdpVersion),
                _ => return Err(Failure::Unsupported("program type")),
            },
            id: record.id,
            generation,
            name: record.spec.name().as_str().into(),
            source: record.source.into(),
            license: record.license.into(),
            created_at: timestamp(record.created_at)?,
            metadata: record.metadata.clone(),
            globals: record.globals.clone(),
        };

        let summary = row.summary();
        self.map_sets.push(MapSet {
            id: row.id,
            generation,
        });
        self.programs.push(row);

        Ok(summary)
    }

    pub(super) fn xdp_snapshot(
        &self,
        row: &crate::xdp::Row,
        layout: &RuntimeLayout,
    ) -> Result<bpfman_model::XdpSnapshot, Failure> {
        let program = self
            .programs
            .iter()
            .find(|p| p.id == row.program_id && p.kind == Kind::Xdp)
            .ok_or(Failure::Invalid("missing XDP program"))?;
        row.snapshot(layout, &program.name)
    }

    pub(super) fn link_ids(&self, program: NonZeroU32) -> Vec<NonZeroU64> {
        let mut links: Vec<_> = self
            .links
            .iter()
            .filter(|row| row.program_id == program)
            .map(|row| row.id)
            .collect();
        links.extend(
            self.xdp
                .iter()
                .filter(|row| row.program_id == program)
                .map(|row| row.link_id),
        );
        links.sort_unstable();

        links
    }
}

impl Program {
    pub(super) fn summary(&self) -> StoredProgramSummary {
        StoredProgramSummary::new(
            self.id,
            self.name.clone(),
            self.kind.model(),
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
            spec: {
                let name = Symbol::try_from(self.name.as_str())
                    .map_err(|_| Failure::Invalid("invalid ELF symbol"))?;
                match self.kind {
                    Kind::Tracepoint => ProgramSpec::Tracepoint(name),
                    Kind::Xdp => ProgramSpec::Xdp(name),
                }
            },
            source: ProgramSource::File(Some(self.source.clone())),
            object_path: path(layout.bytecode_path(self.id))?,
            pin_path: path(layout.program_pin_path(self.id))?,
            map_path: path(layout.map_directory_path(self.id))?,
            map_set: self.id,
            globals: self
                .globals
                .iter()
                .map(|(name, bytes)| (name.clone(), Some(bytes.clone())))
                .collect(),
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

impl Link {
    pub(super) fn record(&self, layout: &RuntimeLayout) -> Result<StoredLink, Failure> {
        let target = self
            .target
            .parse()
            .map_err(|_| Failure::Invalid("invalid tracepoint target"))?;
        let state = match self.state {
            LinkProgress::Pending => LinkState::Pending,
            LinkProgress::Attached { kernel_id } => LinkState::Attached { kernel_id },
        };

        Ok(StoredLink {
            id: self.id,
            program_id: self.program_id,
            details: LinkDetails::Tracepoint(target),
            state,
            pin_path: layout
                .link_pin_path(self.id)
                .into_os_string()
                .into_string()
                .map_err(|_| Failure::Invalid("runtime path is not UTF-8"))?,
            metadata: self.metadata.clone(),
            created_at: timestamp(&self.created_at)?,
        })
    }
}

pub(super) fn timestamp(raw: &str) -> Result<String, Failure> {
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

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    #[default]
    Tracepoint,
    Xdp,
}

impl Kind {
    fn is_tracepoint(&self) -> bool {
        *self == Self::Tracepoint
    }

    pub(super) fn model(self) -> ProgramType {
        match self {
            Self::Tracepoint => ProgramType::Tracepoint,
            Self::Xdp => ProgramType::Xdp,
        }
    }
}
