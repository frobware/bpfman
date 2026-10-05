//! Pure domain vocabulary shared by lifecycle policy and I/O adapters.
//!
//! Wire formats and kernel-library types belong at the application boundary.

#![no_std]

extern crate alloc;

use alloc::string::String;

mod xdp;
pub use xdp::{InterfaceName, InvalidXdp, XdpKey, XdpLink, XdpProceedOn, XdpSnapshot, xdp_config};

mod link;
mod load;
mod observation;
mod program_type;
mod summary;

pub use observation::{
    ImagePullPolicy, KernelMap, KernelProgram, ObservedMap, ObservedProgram, ProgramEntry,
    ProgramSource, ProgramStats, StoredProgram,
};

pub use link::{
    InvalidTracepoint, KernelLink, KernelLinkDetails, LinkDetails, LinkState, ObservedLink,
    StoredLink, Tracepoint,
};
pub use program_type::{ParseProgramTypeError, ProgramType};
pub use summary::StoredProgramSummary;

/// A nonempty ELF symbol or load-time target, without a colon or NUL.
///
/// Whitespace at either end is rejected; input boundaries may trim first.
/// This is not a filesystem component or proof the symbol exists in an ELF.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Symbol(String);

/// Input cannot name an ELF symbol or load-time target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidSymbol;

/// A selected ELF program with the load-time data required by its kind.
///
/// The variant determines the kind; there is no independent discriminator or
/// optional target that can contradict it. Targets describe loading, not an
/// attachment (in particular a tracepoint selection needs no category/event).
///
/// ```compile_fail
/// use bpfman_model::{ProgramSpec, Symbol};
/// let spec = ProgramSpec::Fentry { name: Symbol::try_from("enter").unwrap() };
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProgramSpec {
    /// Express Data Path entry function.
    Xdp(Symbol),
    /// Traffic-control dispatcher entry function.
    Tc(Symbol),
    /// Native multi-program traffic-control entry function.
    Tcx(Symbol),
    /// Tracepoint entry function; attachment is a separate operation.
    Tracepoint(Symbol),
    /// Kernel function entry probe.
    Kprobe(Symbol),
    /// Kernel function return probe.
    Kretprobe(Symbol),
    /// User-space function entry probe.
    Uprobe(Symbol),
    /// User-space function return probe.
    Uretprobe(Symbol),
    /// BTF function entry tracing.
    Fentry {
        /// ELF entry function.
        name: Symbol,
        /// Kernel function required when loading.
        target: Symbol,
    },
    /// BTF function exit tracing.
    Fexit {
        /// ELF entry function.
        name: Symbol,
        /// Kernel function required when loading.
        target: Symbol,
    },
    /// Linux Security Module program.
    Lsm {
        /// ELF entry function.
        name: Symbol,
        /// Security hook required when loading.
        hook: Symbol,
    },
}
