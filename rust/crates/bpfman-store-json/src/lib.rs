//! Versioned JSON snapshots implementing the portable store contracts.
//!
//! The runtime writer serializes read/modify/publish operations. Publication
//! atomically replaces the whole snapshot; program and map membership never
//! become separately visible. File-format types remain private to this crate.

mod backend;
mod error;
mod state;

use bpfman_fs::{RuntimeIdentity, RuntimeLayout, StoreSnapshot};

/// Select JSON persistence at the application composition root.
#[derive(Clone, Copy, Debug, Default)]
pub struct Backend;

/// Opened store directory and layout used for fresh, validated observations.
pub struct Reader {
    file: StoreSnapshot,
    layout: RuntimeLayout,
}

/// Owned evidence for conditional deletion of one observed program generation.
pub struct ProgramReceipt {
    root: RuntimeIdentity,
    store: String,
    row: state::Tracepoint,
}

/// Owned evidence for conditional deletion of an unused private map generation.
pub struct MapSetReceipt {
    root: RuntimeIdentity,
    store: String,
    row: state::MapSet,
}
