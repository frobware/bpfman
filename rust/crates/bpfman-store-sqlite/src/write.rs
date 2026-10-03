//! A load becomes visible in one transaction: map set and program together.

use crate::{
    Error, TracepointRecord,
    error::Failure,
    open::{require_supported, schema_version},
};
use bpfman_fs::RuntimeWriter;
use bpfman_model::{ProgramType, StoredProgramSummary};
use rusqlite::{Connection, OpenFlags, TransactionBehavior, params};

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
    let paths = [
        layout.bytecode_path(record.id),
        layout.program_pin_path(record.id),
        layout.map_directory_path(record.id),
    ];
    let paths = paths
        .iter()
        .map(|path| {
            path.to_str().ok_or_else(|| Failure::InvalidRecord {
                id: i64::from(record.id.get()),
                reason: "runtime path is not UTF-8".into(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let metadata = serde_json::to_string(record.metadata).map_err(|source| Failure::Metadata {
        id: i64::from(record.id.get()),
        source,
    })?;
    let mut connection =
        Connection::open_with_flags(writer.database_path(), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_supported(schema_version(&tx)?)?;
    tx.execute(
        "INSERT INTO map_sets (id, pin_path, created_at) VALUES (?1, ?2, ?3)",
        params![record.id.get(), paths[2], record.created_at],
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
    tx.execute(
        "INSERT INTO managed_programs
        (program_id, program_name, program_type, object_path, source_path, pin_path,
         map_set_id, license, gpl_compatible, metadata_json, created_at)
        VALUES (?1, ?2, 'tracepoint', ?3, ?4, ?5, ?1, ?6, ?7, ?8, ?9)",
        params![
            record.id.get(),
            record.name.as_str(),
            paths[0],
            record.source,
            paths[1],
            record.license,
            gpl,
            metadata,
            record.created_at
        ],
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
