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
    let mut statement = tx.prepare("SELECT kernel_prog_id, id FROM links ORDER BY id")?;
    let mut rows = statement.query([])?;

    while let Some(row) = rows.next()? {
        let program_id: i64 = row.get(0)?;
        let raw: i64 = row.get(1)?;
        let id = u64::try_from(raw)
            .ok()
            .and_then(NonZeroU64::new)
            .ok_or_else(|| Failure::InvalidRecord {
                id: program_id,
                reason: format!("invalid link ID {raw}"),
            })?;
        links.entry(program_id).or_default().push(id);
    }

    // The join mirrors Go's list query: programs refer to a concrete map set.
    let mut statement = tx.prepare(
        "SELECT m.program_id, m.program_name, m.program_type, m.metadata_json
             FROM managed_programs m JOIN map_sets ms ON ms.id = m.map_set_id
             ORDER BY m.program_id",
    )?;
    let mut rows = statement.query([])?;
    let mut programs = Vec::new();

    while let Some(row) = rows.next()? {
        let raw: i64 = row.get(0)?;
        let id = u32::try_from(raw)
            .ok()
            .and_then(NonZeroU32::new)
            .ok_or_else(|| Failure::InvalidRecord {
                id: raw,
                reason: "program ID must be a nonzero u32".into(),
            })?;
        let kind: String = row.get(2)?;
        let kind = kind
            .parse::<ProgramType>()
            .map_err(|_| Failure::InvalidRecord {
                id: raw,
                reason: format!("unknown program type {kind:?}"),
            })?;
        let metadata: String = row.get(3)?;
        let metadata = if metadata.is_empty() {
            BTreeMap::new()
        } else {
            serde_json::from_str::<Option<BTreeMap<String, String>>>(&metadata)
                .map_err(|source| Failure::Metadata { id: raw, source })?
                .unwrap_or_default()
        };
        programs.push(StoredProgramSummary::new(
            id,
            row.get(1)?,
            kind,
            metadata,
            links.remove(&raw).unwrap_or_default(),
        ));
    }

    Ok(programs)
}
