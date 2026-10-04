//! SQLite connection and row decoding stay private behind the crate facade.

use std::{
    collections::BTreeMap,
    num::{NonZeroU32, NonZeroU64},
};

use bpfman_model::{ProgramType, StoredProgramSummary};
use rusqlite::Connection;

use crate::{
    Error, Store,
    error::Failure,
    open::{require_supported, schema_version},
    queries,
};

impl Store {
    /// Read one consistent snapshot using this handle, without reopening by path.
    ///
    /// Schema compatibility is checked again within the read transaction so a
    /// later migration cannot silently invalidate the earlier opening observation.
    pub fn read_programs(&mut self) -> Result<Vec<StoredProgramSummary>, Error> {
        self.reader.read(read).map_err(Error::from)
    }
}

fn read(connection: &mut Connection) -> Result<Vec<StoredProgramSummary>, Failure> {
    let tx = connection.transaction()?;
    require_supported(schema_version(&tx)?)?;

    let mut links = BTreeMap::<i64, Vec<NonZeroU64>>::new();

    for row in queries::links(&tx)? {
        let program_id = row.program_id;
        let raw = row.link_id;
        let id = u64::try_from(raw)
            .ok()
            .and_then(NonZeroU64::new)
            .ok_or_else(|| Failure::InvalidRecord {
                id: program_id,
                reason: format!("invalid link ID {raw}"),
            })?;
        links.entry(program_id).or_default().push(id);
    }

    let mut programs = Vec::new();

    for row in queries::summaries(&tx)? {
        let raw = row.id;
        let id = u32::try_from(raw)
            .ok()
            .and_then(NonZeroU32::new)
            .ok_or_else(|| Failure::InvalidRecord {
                id: raw,
                reason: "program ID must be a nonzero u32".into(),
            })?;
        let kind = row.kind;
        let kind = kind
            .parse::<ProgramType>()
            .map_err(|_| Failure::InvalidRecord {
                id: raw,
                reason: format!("unknown program type {kind:?}"),
            })?;
        let metadata = row.metadata;
        let metadata = if metadata.is_empty() {
            BTreeMap::new()
        } else {
            serde_json::from_str::<Option<BTreeMap<String, String>>>(&metadata)
                .map_err(|source| Failure::Metadata { id: raw, source })?
                .unwrap_or_default()
        };
        programs.push(StoredProgramSummary::new(
            id,
            row.name,
            kind,
            metadata,
            links.remove(&raw).unwrap_or_default(),
        ));
    }

    Ok(programs)
}
