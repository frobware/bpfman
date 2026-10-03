//! Conditional forward deletion of committed state. No load rollback is reused.

use crate::{
    Error, PrivateMapSet, ProgramRecord, UnloadRecord,
    error::Failure,
    open::{require_supported, schema_version},
};
use bpfman_core::EffectFailure;
use bpfman_fs::RuntimeWriter;
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use std::num::NonZeroU32;

pub(super) struct RecordEvidence {
    root: bpfman_fs::RuntimeIdentity,
    id: NonZeroU32,
    snapshot: Snapshot,
}

pub(super) struct MapSetEvidence {
    root: bpfman_fs::RuntimeIdentity,
    id: NonZeroU32,
    created_at: String,
}

#[derive(PartialEq, Eq)]
struct Snapshot {
    kind: String,
    object: String,
    pin: String,
    map_set: i64,
    map_path: String,
    created: String,
    map_created: String,
    links: i64,
    users: i64,
    shared: i64,
}

fn snapshot(connection: &Connection, id: NonZeroU32) -> Result<Option<Snapshot>, Failure> {
    require_supported(schema_version(connection)?)?;

    Ok(connection
        .query_row(
            "SELECT p.program_type, p.object_path, p.pin_path, p.map_set_id, m.pin_path,
         p.created_at, m.created_at,
         (SELECT count(*) FROM links WHERE kernel_prog_id = p.program_id),
         (SELECT count(*) FROM managed_programs WHERE map_set_id = p.map_set_id),
         (SELECT count(*) FROM shared_map_pins WHERE program_id = p.program_id)
         FROM managed_programs p LEFT JOIN map_sets m ON m.id = p.map_set_id
         WHERE p.program_id = ?1",
            [id.get()],
            |r| {
                Ok(Snapshot {
                    kind: r.get(0)?,
                    object: r.get(1)?,
                    pin: r.get(2)?,
                    map_set: r.get(3)?,
                    map_path: r.get(4)?,
                    created: r.get(5)?,
                    map_created: r.get(6)?,
                    links: r.get(7)?,
                    users: r.get(8)?,
                    shared: r.get(9)?,
                })
            },
        )
        .optional()?)
}

fn invalid(id: NonZeroU32, reason: &str) -> Failure {
    Failure::InvalidRecord {
        id: i64::from(id.get()),
        reason: reason.into(),
    }
}

fn validate(writer: &RuntimeWriter<'_>, id: NonZeroU32, row: &Snapshot) -> Result<(), Failure> {
    if row.kind != "tracepoint" {
        return Err(Failure::Unsupported("only tracepoints are implemented"));
    }

    if row.links != 0 {
        return Err(Failure::Unsupported(
            "program has links; detach is not implemented",
        ));
    }

    if row.map_set != i64::from(id.get()) || row.users != 1 || row.shared != 0 {
        return Err(Failure::Unsupported(
            "only private map sets without shared pins are implemented",
        ));
    }

    let layout = writer.layout();

    for (stored, expected) in [
        (&row.object, layout.bytecode_path(id)),
        (&row.pin, layout.program_pin_path(id)),
        (&row.map_path, layout.map_directory_path(id)),
    ] {
        if Some(stored.as_str()) != expected.to_str() {
            return Err(invalid(
                id,
                "stored artifact path differs from canonical runtime layout",
            ));
        }
    }

    Ok(())
}

/// Observe and validate the complete supported teardown scope without mutation.
/// Linked programs, shared map sets/pins, and noncanonical paths are refused.
/// Absence is distinct from invalid or unreadable state. Call under one writer
/// scope with filesystem observation and the subsequent deletion operations.
pub fn observe_unload(
    writer: &RuntimeWriter<'_>,
    id: NonZeroU32,
) -> Result<Option<UnloadRecord>, Error> {
    let Some(store) = crate::Store::inspect(&writer.database_path())? else {
        return Ok(None);
    };
    let Some(row) = store.reader.read(|connection| snapshot(connection, id))? else {
        return Ok(None);
    };
    validate(writer, id, &row)?;

    Ok(Some(UnloadRecord {
        map_set: PrivateMapSet {
            evidence: Box::new(MapSetEvidence {
                root: writer.identity().map_err(Failure::from)?,
                id,
                created_at: row.map_created.clone(),
            }),
        },
        program: ProgramRecord {
            evidence: Box::new(RecordEvidence {
                root: writer.identity().map_err(Failure::from)?,
                id,
                snapshot: row,
            }),
        },
    }))
}

impl UnloadRecord {
    /// Split independent store receipts after preflight, without duplicating them.
    pub fn into_parts(self) -> (ProgramRecord, PrivateMapSet) {
        (self.program, self.map_set)
    }
}

fn connection(writer: &RuntimeWriter<'_>) -> Result<Connection, Failure> {
    let connection =
        Connection::open_with_flags(writer.database_path(), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    require_supported(schema_version(&connection)?)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;

    Ok(connection)
}

/// Delete exactly the previously observed program, rechecking scope atomically.
/// Failure retains the receipt. No map-set row is deleted in this transaction.
///
/// ```compile_fail
/// use bpfman_store_sqlite::{delete_unloaded_program, ProgramRecord};
/// fn unlocked(runtime: &bpfman_fs::RuntimeDirectory, record: ProgramRecord) {
///     delete_unloaded_program(runtime, record);
/// }
/// ```
pub fn delete_unloaded_program(
    writer: &RuntimeWriter<'_>,
    receipt: ProgramRecord,
) -> Result<(), EffectFailure<ProgramRecord, Error>> {
    let result = (|| -> Result<(), Failure> {
        if writer.identity()? != receipt.evidence.root {
            return Err(invalid(
                receipt.evidence.id,
                "record belongs to another runtime",
            ));
        }

        let mut connection = connection(writer)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = snapshot(&tx, receipt.evidence.id)?
            .ok_or_else(|| invalid(receipt.evidence.id, "program record disappeared"))?;
        validate(writer, receipt.evidence.id, &row)?;

        if row != receipt.evidence.snapshot {
            return Err(invalid(
                receipt.evidence.id,
                "program record changed since observation",
            ));
        }

        let count = tx.execute(
            "DELETE FROM managed_programs WHERE program_id = ?1",
            [receipt.evidence.id.get()],
        )?;

        if count != 1 {
            return Err(invalid(
                receipt.evidence.id,
                "program deletion did not remove its row",
            ));
        }

        tx.commit()?;

        Ok(())
    })();
    result.map_err(|cause| EffectFailure {
        remaining: receipt,
        cause: cause.into(),
    })
}

/// Delete only the observed map set, after its last user and owned pins are gone.
/// Recheck the row identity and zero users in the same transaction as deletion.
pub fn delete_unused_map_set(
    writer: &RuntimeWriter<'_>,
    receipt: PrivateMapSet,
) -> Result<(), EffectFailure<PrivateMapSet, Error>> {
    let result = (|| -> Result<(), Failure> {
        if writer.identity()? != receipt.evidence.root {
            return Err(invalid(
                receipt.evidence.id,
                "map set belongs to another runtime",
            ));
        }

        let mut connection = connection(writer)?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        require_supported(schema_version(&tx)?)?;
        let count = tx.execute(
            "DELETE FROM map_sets WHERE id = ?1 AND created_at = ?2 AND pin_path = ?3
            AND NOT EXISTS (SELECT 1 FROM managed_programs WHERE map_set_id = ?1)",
            params![
                receipt.evidence.id.get(),
                receipt.evidence.created_at,
                writer
                    .layout()
                    .map_directory_path(receipt.evidence.id)
                    .to_str()
            ],
        )?;

        if count != 1 {
            return Err(invalid(
                receipt.evidence.id,
                "map set changed or is still in use",
            ));
        }

        tx.commit()?;

        Ok(())
    })();
    result.map_err(|cause| EffectFailure {
        remaining: receipt,
        cause: cause.into(),
    })
}
