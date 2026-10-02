use alloc::{collections::BTreeMap, string::String, vec::Vec};
use core::num::{NonZeroU32, NonZeroU64};

use crate::ProgramType;

/// A managed program's stored listing fields, with no claim of kernel presence.
///
/// This projection deliberately contains no wire types or variant-specific load
/// data. Full records and requests will model those using payload-bearing enums.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredProgramSummary {
    id: NonZeroU32,
    name: String,
    kind: ProgramType,
    metadata: BTreeMap<String, String>,
    links: Vec<NonZeroU64>,
}

impl StoredProgramSummary {
    /// Construct a stored summary from decoded fields and typed identities.
    pub fn new(
        id: NonZeroU32,
        name: String,
        kind: ProgramType,
        metadata: BTreeMap<String, String>,
        mut links: Vec<NonZeroU64>,
    ) -> Self {
        links.sort_unstable();
        links.dedup();
        Self {
            id,
            name,
            kind,
            metadata,
            links,
        }
    }

    /// Kernel-assigned identity recorded at load time.
    pub const fn id(&self) -> NonZeroU32 {
        self.id
    }

    /// Stored ELF entry function name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Bpfman's attach-oriented program type.
    pub const fn kind(&self) -> ProgramType {
        self.kind
    }

    /// Application label, or an empty string when unassigned.
    pub fn application(&self) -> &str {
        self.metadata
            .get("bpfman.io/application")
            .map_or("", String::as_str)
    }

    /// Sorted bpfman-managed link identities; these do not prove active attachment.
    pub fn links(&self) -> &[NonZeroU64] {
        &self.links
    }
}
