//! Versioned JSON snapshots implementing the portable store contracts.
//!
//! The runtime writer serializes read/modify/publish operations. Publication
//! atomically replaces the whole snapshot; program and map membership never
//! become separately visible. File-format types remain private to this crate.

mod backend;
mod error;
mod link;
mod state;
mod tc;
mod xdp;

use bpfman_fs::{RuntimeIdentity, RuntimeLayout, StoreSnapshot};

/// Select JSON persistence at the application composition root.
#[derive(Clone, Copy, Debug, Default)]
pub struct Backend;

/// Opened store directory and layout used for fresh, validated observations.
#[derive(Clone)]
pub struct Reader {
    file: std::sync::Arc<StoreSnapshot>,
    layout: RuntimeLayout,
}

/// Owned evidence for conditional deletion of one observed program generation.
pub struct ProgramReceipt {
    root: RuntimeIdentity,
    store: String,
    row: state::Program,
}

/// Owned evidence for conditional deletion of an unused private map generation.
pub struct MapSetReceipt {
    root: RuntimeIdentity,
    store: String,
    row: state::MapSet,
}

/// Owned evidence for conditional mutation of one unchanged standalone link.
pub struct LinkReceipt {
    root: RuntimeIdentity,
    store: String,
    row: state::Link,
}

/// Owned evidence for conditional deletion of one complete XDP snapshot.
pub struct XdpReceipt {
    root: RuntimeIdentity,
    store: String,
    rows: Vec<xdp::Row>,
    programs: Vec<state::Program>,
}

/// Conditional evidence for one unchanged TC ingress snapshot.
pub struct TcReceipt {
    root: RuntimeIdentity,
    store: String,
    row: tc::Row,
    program: state::Program,
}
