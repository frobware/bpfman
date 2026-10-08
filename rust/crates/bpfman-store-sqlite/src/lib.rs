//! Adapter for Go's SQLite schema, version 2.
//!
//! Creation publishes missing databases but never migrates existing ones.
//! Queries remain read-only; load persistence uses a separate atomic write API.
//! Reads return domain observations; selection policy
//! remains in bpfman-core.

mod create;
mod error;
mod link;
mod open;
mod queries;
mod read;
mod reader;
mod records;
mod tc;
mod unload;
mod write;
mod xdp;

pub use bpfman_store::LoadRecord;
pub use unload::{delete_unloaded_program, delete_unused_map_set, observe_unload};
pub use write::persist_program;

mod backend;

/// SQLite backend selected by the application composition root.
/// This value performs no I/O until a store operation is called.
#[derive(Clone, Copy, Debug, Default)]
pub struct Backend;

pub use create::create_if_missing;
pub use error::{Error, ErrorKind};

/// Schema version supported by this adapter, from Go's 00002_add_lsm.sql.
pub const SCHEMA_VERSION: i64 = 2;

/// An opened read-only store; connection reuse stays inside the adapter.
/// Clones share idle resources while concurrent reads use separate connections.
///
/// Inspection reports the observed schema even when it is incompatible, so the
/// caller can apply opening policy. Reads independently check their own snapshot.
#[derive(Clone, Debug)]
pub struct Store {
    reader: std::sync::Arc<reader::Reader>,
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

/// Non-cloneable evidence for conditional mutation of one standalone link.
pub struct LinkReceipt {
    evidence: Box<link::Evidence>,
}

/// Non-cloneable conditional evidence for one complete XDP dispatcher snapshot.
pub struct XdpReceipt {
    root: bpfman_fs::RuntimeIdentity,
    database: (u64, u64),
    rows: Vec<queries::xdp::Row>,
}

/// Conditional evidence for one unchanged singleton TC ingress snapshot.
pub struct TcReceipt {
    root: bpfman_fs::RuntimeIdentity,
    database: (u64, u64),
    row: queries::tc::Row,
}
