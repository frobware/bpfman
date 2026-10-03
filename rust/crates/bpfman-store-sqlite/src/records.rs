//! Full record decoding is separate from the cheap stored-summary projection.
use crate::{
    Error, Store,
    error::Failure,
    open::{require_supported, schema_version},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use bpfman_model::{
    ImagePullPolicy, ProgramSource, ProgramSpec, ProgramType, StoredProgram, Symbol,
};
use rusqlite::Row;
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
fn timestamp(raw: String, program: i64) -> Result<String, Failure> {
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

fn decode(row: &Row<'_>) -> Result<StoredProgram, Failure> {
    let raw: i64 = row.get(0)?;
    let name: String = row.get(1)?;
    let symbol =
        Symbol::try_from(name.as_str()).map_err(|_| invalid(raw, "invalid program name"))?;
    let kind: String = row.get(2)?;
    let kind = kind
        .parse::<ProgramType>()
        .map_err(|_| invalid(raw, "unknown program type"))?;
    let target: Option<String> = row.get(6)?;
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
    let source_path: Option<String> = row.get(4)?;
    let image: Option<String> = row.get(8)?;
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
    let global_json: Option<String> = row.get(7)?;
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
        object_path: row.get(3)?,
        pin_path: row.get(5)?,
        globals,
        owner: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
        description: row.get::<_, Option<String>>(10)?.unwrap_or_default(),
        license: row.get::<_, Option<String>>(11)?.unwrap_or_default(),
        gpl_compatible: row.get::<_, i64>(12)? != 0,
        metadata: labels(row.get(13)?, raw)?,
        created_at: timestamp(row.get(14)?, raw)?,
        updated_at: row
            .get::<_, Option<String>>(15)?
            .map(|s| timestamp(s, raw))
            .transpose()?,
        map_set: id(row.get(16)?)?,
        map_path: row.get(17)?,
        links: Vec::new(),
    })
}
impl Store {
    /// Read full committed records and link identities in one read-only snapshot.
    /// Invalid values fail decoding; missing map-set references are never omitted.
    pub fn read_records(&mut self) -> Result<Vec<StoredProgram>, Error> {
        let tx = self.connection.transaction().map_err(Failure::from)?;
        require_supported(schema_version(&tx)?)?;
        let mut statement=tx.prepare("SELECT p.program_id,p.program_name,p.program_type,p.object_path,p.source_path,p.pin_path,p.attach_func,p.global_data,p.image_source,p.owner,p.description,p.license,p.gpl_compatible,p.metadata_json,p.created_at,p.updated_at,p.map_set_id,m.pin_path FROM managed_programs p LEFT JOIN map_sets m ON m.id=p.map_set_id ORDER BY p.program_id").map_err(Failure::from)?;
        let mut rows = statement.query([]).map_err(Failure::from)?;
        let mut records = Vec::new();
        while let Some(row) = rows.next().map_err(Failure::from)? {
            records.push(decode(row)?);
        }
        let mut links = tx
            .prepare("SELECT kernel_prog_id,id FROM links ORDER BY id")
            .map_err(Failure::from)?;
        let mut rows = links.query([]).map_err(Failure::from)?;
        while let Some(row) = rows.next().map_err(Failure::from)? {
            let program: i64 = row.get(0).map_err(Failure::from)?;
            let raw: i64 = row.get(1).map_err(Failure::from)?;
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
}
