//! Adapter for Go's SQLite schema, version 2.
//!
//! Initialisation creates missing databases but never migrates existing ones.
//! Queries remain read-only and return domain observations; selection policy
//! remains in bpfman-core.

mod error;
mod initialise;
mod open;
mod read;

pub use error::{Error, ErrorKind};
pub use initialise::initialise_if_missing;

/// Schema version supported by this adapter, from Go's 00002_add_lsm.sql.
pub const SCHEMA_VERSION: i64 = 2;

/// An opened read-only store; the connection never escapes the adapter.
///
/// Inspection reports the observed schema even when it is incompatible, so the
/// caller can apply setup policy. Reads independently check their own snapshot.
#[derive(Debug)]
pub struct Store {
    connection: rusqlite::Connection,
    schema_version: i64,
}
