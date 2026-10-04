//! Full record decoding is separate from the cheap stored-summary projection.

use crate::{
    Error, Store,
    error::Failure,
    open::{require_supported, schema_version},
    queries,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use bpfman_model::{
    ImagePullPolicy, ProgramSource, ProgramSpec, ProgramType, StoredProgram, Symbol,
};
use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
};

fn invalid(id: i64, reason: impl Into<String>) -> Failure {
    Failure::InvalidRecord {
        id,
        reason: reason.into(),
    }
}

fn id(raw: i64) -> Result<NonZeroU32, Failure> {
    u32::try_from(raw)
        .ok()
        .and_then(NonZeroU32::new)
        .ok_or_else(|| invalid(raw, "identity must be a nonzero u32"))
}

fn labels(raw: Option<String>, program: i64) -> Result<BTreeMap<String, String>, Failure> {
    raw.filter(|s| !s.is_empty())
        .map(|s| {
            serde_json::from_str::<Option<BTreeMap<String, String>>>(&s)
                .map(|m| m.unwrap_or_default())
                .map_err(|e| invalid(program, format!("invalid string map: {e}")))
        })
        .transpose()
        .map(|m| m.unwrap_or_default())
}

pub(super) fn timestamp(raw: String, program: i64) -> Result<String, Failure> {
    use chrono::Datelike;
    let parsed = chrono::DateTime::parse_from_rfc3339(&raw)
        .map_err(|_| invalid(program, "invalid RFC3339 timestamp"))?;

    if !(0..=9999).contains(&parsed.year()) {
        return Err(invalid(program, "timestamp year outside RFC3339 range"));
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

fn decode(row: queries::ProgramRow) -> Result<StoredProgram, Failure> {
    let raw = row.id;
    let name = row.name;
    let symbol =
        Symbol::try_from(name.as_str()).map_err(|_| invalid(raw, "invalid program name"))?;
    let kind = row.kind;
    let kind = kind
        .parse::<ProgramType>()
        .map_err(|_| invalid(raw, "unknown program type"))?;
    let target = row.attach_func;
    let target = || {
        Symbol::try_from(target.as_deref().unwrap_or_default())
            .map_err(|_| invalid(raw, "missing or invalid attach target"))
    };
    let spec = match kind {
        ProgramType::Xdp => ProgramSpec::Xdp(symbol),
        ProgramType::Tc => ProgramSpec::Tc(symbol),
        ProgramType::Tcx => ProgramSpec::Tcx(symbol),
        ProgramType::Tracepoint => ProgramSpec::Tracepoint(symbol),
        ProgramType::Kprobe => ProgramSpec::Kprobe(symbol),
        ProgramType::Kretprobe => ProgramSpec::Kretprobe(symbol),
        ProgramType::Uprobe => ProgramSpec::Uprobe(symbol),
        ProgramType::Uretprobe => ProgramSpec::Uretprobe(symbol),
        ProgramType::Fentry => ProgramSpec::Fentry {
            name: symbol,
            target: target()?,
        },
        ProgramType::Fexit => ProgramSpec::Fexit {
            name: symbol,
            target: target()?,
        },
        ProgramType::Lsm => ProgramSpec::Lsm {
            name: symbol,
            hook: target()?,
        },
    };
    let source_path = row.source_path;
    let image = row.image_source;
    let source = match image {
        None => ProgramSource::File(source_path.filter(|s| !s.is_empty())),
        Some(image) => {
            let value: serde_json::Value = serde_json::from_str(&image)
                .map_err(|e| invalid(raw, format!("invalid image provenance: {e}")))?;
            let field = |name| {
                value
                    .get(name)
                    .and_then(|s| s.as_str())
                    .ok_or_else(|| invalid(raw, "invalid image provenance field"))
            };
            let pull_policy = match field("pull_policy")? {
                "Always" => ImagePullPolicy::Always,
                "IfNotPresent" => ImagePullPolicy::IfNotPresent,
                "Never" => ImagePullPolicy::Never,
                _ => return Err(invalid(raw, "invalid image pull policy")),
            };
            ProgramSource::Image {
                url: field("url")?.into(),
                digest: value
                    .get("digest")
                    .and_then(|s| s.as_str())
                    .unwrap_or_default()
                    .into(),
                pull_policy,
            }
        }
    };
    let global_json = row.global_data;
    let encoded = global_json
        .map(|s| {
            serde_json::from_str::<Option<BTreeMap<String, Option<String>>>>(&s)
                .map_err(|e| invalid(raw, format!("invalid globals: {e}")))
        })
        .transpose()?
        .flatten()
        .unwrap_or_default();
    let globals = encoded
        .into_iter()
        .map(|(key, value)| {
            value
                .map(|s| {
                    STANDARD
                        .decode(s)
                        .map_err(|e| invalid(raw, format!("invalid global bytes: {e}")))
                })
                .transpose()
                .map(|value| (key, value))
        })
        .collect::<Result<_, _>>()?;

    Ok(StoredProgram {
        id: id(raw)?,
        spec,
        source,
        object_path: row.object_path,
        pin_path: row.pin_path,
        globals,
        owner: row.owner.unwrap_or_default(),
        description: row.description.unwrap_or_default(),
        license: row.license.unwrap_or_default(),
        gpl_compatible: row.gpl_compatible != 0,
        metadata: labels(row.metadata, raw)?,
        created_at: timestamp(row.created_at, raw)?,
        updated_at: row.updated_at.map(|s| timestamp(s, raw)).transpose()?,
        map_set: id(row.map_set)?,
        map_path: row.map_path,
        links: Vec::new(),
    })
}

impl Store {
    /// Read full committed records and link identities in one read-only snapshot.
    /// Invalid values fail decoding; missing map-set references are never omitted.
    pub fn read_records(&mut self) -> Result<Vec<StoredProgram>, Error> {
        self.reader.read(read).map_err(Error::from)
    }
}

fn read(connection: &mut rusqlite::Connection) -> Result<Vec<StoredProgram>, Failure> {
    let tx = connection.transaction()?;
    require_supported(schema_version(&tx)?)?;
    let mut records = queries::programs(&tx)?
        .into_iter()
        .map(decode)
        .collect::<Result<Vec<_>, _>>()?;

    for row in queries::links(&tx)? {
        let program = row.program_id;
        let raw = row.link_id;
        let link = u64::try_from(raw)
            .ok()
            .and_then(NonZeroU64::new)
            .ok_or_else(|| invalid(program, "invalid link ID"))?;

        if let Some(record) = records
            .iter_mut()
            .find(|p| i64::from(p.id.get()) == program)
        {
            record.links.push(link);
        }
    }

    Ok(records)
}
