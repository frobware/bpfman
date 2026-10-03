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
mod write;
pub use write::{TracepointRecord, persist_tracepoint};

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
