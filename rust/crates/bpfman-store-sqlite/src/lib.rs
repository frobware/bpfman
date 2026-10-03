//! Adapter for Go's SQLite schema, version 2.
//!
//! Creation publishes missing databases but never migrates existing ones.
//! Queries remain read-only; load persistence uses a separate atomic write API.
//! Reads return domain observations; selection policy
//! remains in bpfman-core.

mod create;
mod error;
mod open;
mod read;
mod records;
mod unload;
mod write;
pub use bpfman_store::TracepointRecord;
pub use unload::{delete_unloaded_program, delete_unused_map_set, observe_unload};
pub use write::persist_tracepoint;
mod backend;

/// SQLite backend selected by the application composition root.
/// This value performs no I/O until a store operation is called.
#[derive(Clone, Copy, Debug, Default)]
pub struct Backend;

pub use create::create_if_missing;
pub use error::{Error, ErrorKind};

/// Schema version supported by this adapter, from Go's 00002_add_lsm.sql.
pub const SCHEMA_VERSION: i64 = 2;

/// An opened read-only store; the connection never escapes the adapter.
///
/// Inspection reports the observed schema even when it is incompatible, so the
/// caller can apply opening policy. Reads independently check their own snapshot.
#[derive(Debug)]
pub struct Store {
    connection: rusqlite::Connection,
    schema_version: i64,
}

/// Validated committed record and private map-set evidence for narrow unload.
/// Fields cannot be forged; dropping evidence does not delete stored state.
pub struct UnloadRecord {
    program: ProgramRecord,
    map_set: PrivateMapSet,
}

/// Non-cloneable deletion evidence for one committed unattached tracepoint.
pub struct ProgramRecord {
    evidence: Box<unload::RecordEvidence>,
}

/// Non-cloneable deletion evidence for a map set whose last user is unloading.
pub struct PrivateMapSet {
    evidence: Box<unload::MapSetEvidence>,
}
