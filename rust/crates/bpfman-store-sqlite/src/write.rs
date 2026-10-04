//! A load becomes visible in one transaction: map set and program together.

use crate::{
    Error, TracepointRecord,
    error::Failure,
    open::{require_supported, schema_version},
    queries,
};
use bpfman_fs::RuntimeWriter;
use bpfman_model::{ProgramType, StoredProgramSummary};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};

/// Atomically insert a private map set and its loaded tracepoint. Existing rows
/// are never overwritten. Failure means no successful commit was reported.
/// The caller must compensate kernel/filesystem acquisitions on failure only.
///
/// ```compile_fail
/// use bpfman_store_sqlite::{persist_tracepoint, TracepointRecord};
/// fn unlocked(runtime: &bpfman_fs::RuntimeDirectory, record: TracepointRecord<'_>) {
///     persist_tracepoint(runtime, record);
/// }
/// ```
pub fn persist_tracepoint(
    writer: &RuntimeWriter<'_>,
    record: TracepointRecord<'_>,
) -> Result<StoredProgramSummary, Error> {
    persist(writer, record).map_err(Error::from)
}

fn persist(
    writer: &RuntimeWriter<'_>,
    record: TracepointRecord<'_>,
) -> Result<StoredProgramSummary, Failure> {
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

    let mut connection =
        Connection::open_with_flags(writer.database_path(), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    require_supported(schema_version(&connection)?)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;

    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_supported(schema_version(&tx)?)?;

    queries::insert_map_set(
        &tx,
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
    queries::insert_tracepoint(
        &tx,
        queries::TracepointInsert {
            record: &record,
            object_path,
            pin_path,
            metadata: &metadata,
            gpl_compatible: gpl,
        },
    )?;

    // Construct output before commit. Nothing fallible follows a successful commit.
    let summary = StoredProgramSummary::new(
        record.id,
        record.name.as_str().into(),
        ProgramType::Tracepoint,
        record.metadata.clone(),
        Vec::new(),
    );
    tx.commit()?;

    Ok(summary)
}

fn utf8(path: &std::path::Path, id: std::num::NonZeroU32) -> Result<&str, Failure> {
    path.to_str().ok_or_else(|| Failure::InvalidRecord {
        id: i64::from(id.get()),
        reason: "runtime path is not UTF-8".into(),
    })
}
