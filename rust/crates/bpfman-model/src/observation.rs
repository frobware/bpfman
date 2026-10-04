//! Backend-independent stored and observed values. Wire nullability is a CLI concern.

use crate::ProgramSpec;
use alloc::{collections::BTreeMap, string::String, vec::Vec};
use core::num::{NonZeroU32, NonZeroU64};

/// Provenance of committed bytecode, separate from its stored copy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProgramSource {
    /// Original local operand; older records may not retain it.
    File(Option<String>),
    /// Image provenance contains no registry credentials.
    Image {
        /// Image reference supplied by the caller.
        url: String,
        /// Resolved image digest.
        digest: String,
        /// Requested pull behavior.
        pull_policy: ImagePullPolicy,
    },
}

/// Stored image pull behavior.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImagePullPolicy {
    /// Always consult the registry.
    Always,
    /// Use a cached image if available.
    IfNotPresent,
    /// Require an already cached image.
    Never,
}

/// Committed program observation; paths are data, never deletion authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredProgram {
    /// Managed kernel identity.
    pub id: NonZeroU32,
    /// Stored ELF selection with type-specific load target.
    pub spec: ProgramSpec,
    /// Load provenance.
    pub source: ProgramSource,
    /// Stored bytecode path.
    pub object_path: String,
    /// Stored program pin.
    pub pin_path: String,
    /// Stored map-set directory.
    pub map_path: String,
    /// Map-set identity; differs from the program ID for consumers.
    pub map_set: NonZeroU32,
    /// Global overrides, retaining nil versus empty byte slices.
    pub globals: BTreeMap<String, Option<Vec<u8>>>,
    /// ELF license.
    pub license: String,
    /// Whether that license is GPL-compatible.
    pub gpl_compatible: bool,
    /// Operator owner label.
    pub owner: String,
    /// Operator description.
    pub description: String,
    /// Selection metadata.
    pub metadata: BTreeMap<String, String>,
    /// Validated RFC3339 creation time.
    pub created_at: String,
    /// Validated update time, absent if never updated.
    pub updated_at: Option<String>,
    /// Sorted stored link handles, without kernel presence claims.
    pub links: Vec<NonZeroU64>,
}

/// A live kernel observation; optional fields distinguish unavailable values from zero.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KernelProgram {
    /// Kernel identity.
    pub id: NonZeroU32,
    /// Kernel-truncated name.
    pub name: String,
    /// Kernel type taxonomy.
    pub kind: String,
    /// Instruction tag.
    pub tag: String,
    /// UTC load time at second precision.
    pub loaded_at: Option<String>,
    /// Loader UID when reported.
    pub uid: Option<u32>,
    /// Associated BTF when populated.
    pub btf_id: Option<u32>,
    /// Reported map IDs; an empty collection means no maps.
    pub map_ids: Option<Vec<u32>>,
    /// JIT byte length.
    pub jited_size: u32,
    /// Translated byte length.
    pub xlated_size: u32,
    /// Verifier instruction count.
    pub verified_insns: u32,
    /// Observed locked memory accounting.
    pub memlock: Option<u64>,
    /// Kernel withheld translated instructions.
    pub restricted: bool,
}

/// Kernel map attributes, independent of pin observations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KernelMap {
    /// Kernel map ID.
    pub id: u32,
    /// Kernel-truncated map name.
    pub name: String,
    /// Kernel map taxonomy.
    pub kind: String,
    /// Key size in bytes.
    pub key_size: u32,
    /// Value size in bytes.
    pub value_size: u32,
    /// Maximum entry count.
    pub max_entries: u32,
    /// Creation flags.
    pub flags: u32,
    /// Associated BTF when populated.
    pub btf_id: Option<u32>,
    /// Populated type-specific creation parameter.
    pub map_extra: Option<u64>,
    /// Locked memory accounting.
    pub memlock: Option<u64>,
    /// Observed userspace write protection.
    pub frozen: bool,
}

/// Kernel execution counters; zero is distinct from an unavailable observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgramStats {
    /// Accumulated execution time in nanoseconds.
    pub runtime_ns: u64,
    /// Execution count.
    pub run_count: u64,
    /// Skipped recursive executions.
    pub recursion_misses: u64,
}

/// Kernel map information plus optional filesystem correlation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedMap {
    /// Kernel attributes.
    pub kernel: KernelMap,
    /// Correlated or expected pin path; absent for uncorrelated load observations and internal maps.
    pub pin_path: Option<String>,
    /// Whether the correlated pin was observed.
    pub present: bool,
}

/// A managed program with a successful kernel observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedProgram {
    /// Stored identity and intent.
    pub record: StoredProgram,
    /// Live kernel attributes.
    pub kernel: KernelProgram,
    /// Statistics when collected successfully.
    pub stats: Option<ProgramStats>,
    /// Canonical program pin location.
    pub prog_pin: String,
    /// Canonical map-set location.
    pub map_dir: String,
    /// Canonical bytecode location.
    pub bytecode: String,
    /// Stored links and independently observed kernel/pin presence.
    pub links: Vec<crate::ObservedLink>,
    /// Observed maps.
    pub maps: Vec<ObservedMap>,
    /// Managed programs belonging to this map set.
    pub map_used_by: Vec<NonZeroU32>,
}

/// Managed listing row with a potentially absent kernel object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgramEntry {
    /// Committed record.
    pub record: StoredProgram,
    /// Absent only when the kernel object does not exist.
    pub kernel: Option<KernelProgram>,
}
