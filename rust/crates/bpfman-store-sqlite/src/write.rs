//! A load becomes visible in one transaction: map set and program together.

use crate::{
    Error, LoadRecord,
    error::Failure,
    open::{require_supported, schema_version},
    queries,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use bpfman_fs::RuntimeWriter;
use bpfman_model::{ProgramSpec, StoredProgramSummary};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};

/// Atomically insert a private map set and its loaded program. Existing rows
/// are never overwritten. Failure means no successful commit was reported.
/// The caller must compensate kernel/filesystem acquisitions on failure only.
///
/// ```compile_fail
/// use bpfman_store_sqlite::{persist_program, LoadRecord};
/// fn unlocked(runtime: &bpfman_fs::RuntimeDirectory, record: LoadRecord<'_>) {
///     persist_program(runtime, record);
/// }
/// ```
pub fn persist_program(
    writer: &RuntimeWriter<'_>,
    record: LoadRecord<'_>,
) -> Result<StoredProgramSummary, Error> {
    let summary = StoredProgramSummary::new(
        record.id,
        record.spec.name().as_str().into(),
        record.spec.kind(),
        record.metadata.clone(),
        Vec::new(),
    );
    persist_batch(writer, &[record])?;

    Ok(summary)
}

pub(crate) fn persist_batch(
    writer: &RuntimeWriter<'_>,
    records: &[LoadRecord<'_>],
) -> Result<(), Error> {
    persist(writer, records).map_err(Error::from)
}

fn persist(writer: &RuntimeWriter<'_>, records: &[LoadRecord<'_>]) -> Result<(), Failure> {
    if records.is_empty() {
        return Ok(());
    }

    let mut connection =
        Connection::open_with_flags(writer.database_path(), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    require_supported(schema_version(&connection)?)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;

    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_supported(schema_version(&tx)?)?;

    for record in records {
        insert(&tx, writer, record)?;
    }

    tx.commit()?;

    Ok(())
}

fn insert(
    tx: &rusqlite::Transaction<'_>,
    writer: &RuntimeWriter<'_>,
    record: &LoadRecord<'_>,
) -> Result<(), Failure> {
    if !matches!(
        record.spec,
        ProgramSpec::Tracepoint(_) | ProgramSpec::Xdp(_) | ProgramSpec::Tc(_)
    ) {
        return Err(Failure::Unsupported("program type"));
    }
    let layout = writer.layout();
    let object_path = layout.bytecode_path(record.id);
    let pin_path = layout.program_pin_path(record.id);
    let map_path = layout.map_directory_path(record.id);
    let object_path = utf8(&object_path, record.id)?;
    let pin_path = utf8(&pin_path, record.id)?;
    let map_path = utf8(&map_path, record.id)?;
    let metadata = serde_json::to_string(record.metadata).map_err(|source| Failure::Metadata {
        id: i64::from(record.id.get()),
        source,
    })?;
    let globals = record
        .globals
        .iter()
        .map(|(name, bytes)| (name, STANDARD.encode(bytes)))
        .collect::<std::collections::BTreeMap<_, _>>();
    let globals = serde_json::to_string(&globals).map_err(|source| Failure::Metadata {
        id: i64::from(record.id.get()),
        source,
    })?;

    queries::insert_map_set(
        tx,
        queries::MapSetIdentity {
            id: record.id,
            pin_path: map_path,
            created_at: record.created_at,
        },
    )?;
    let gpl = matches!(
        record.license,
        "GPL"
            | "GPL v2"
            | "GPL and additional rights"
            | "Dual BSD/GPL"
            | "Dual MIT/GPL"
            | "Dual MPL/GPL"
    );
    queries::insert_program(
        tx,
        queries::ProgramInsert {
            record,
            object_path,
            pin_path,
            metadata: &metadata,
            globals: &globals,
            gpl_compatible: gpl,
        },
    )?;

    Ok(())
}

fn utf8(path: &std::path::Path, id: std::num::NonZeroU32) -> Result<&str, Failure> {
    path.to_str().ok_or_else(|| Failure::InvalidRecord {
        id: i64::from(id.get()),
        reason: "runtime path is not UTF-8".into(),
    })
}
