//! Concrete Linux implementation of the kernel observation and lifecycle contracts.
//!
//! BPF ABI types and syscalls stay private. Filesystem traversal is delegated
//! to bpfman-fs, which verifies the adopted runtime and canonical pin locations.

mod backend;
mod failure;
mod function_tracing;
mod image;
mod kprobe;
mod maps;
mod netns;
mod object;
mod observe;
mod pin_syscall;
mod pinning;
mod program;
mod syscall;
mod tc;
mod tc_netlink;
mod tcx;
mod tracepoint;
mod tracing_btf;
mod uprobe;
mod verification;
mod xdp;

pub use image::{BytecodeMetadata, inspect_bytecode};
pub use netns::XdpNamespace;

use bpfman_kernel::{Error, ErrorKind};

/// Linux kernel backend selected by the application composition root.
#[derive(Default)]
pub struct Kernel {
    uprobe_opener: Option<Box<uprobe::TargetOpener>>,
}

/// Adopted program and target descriptor retained through user-probe attachment.
pub struct PreparedUprobe {
    program: bpfman_fs::PreparedUprobeAttach<AyaUprobe>,
    target: Option<std::fs::File>,
    intent: bpfman_model::UprobeLink,
}

/// Owned loaded program and maps; representation stays private to this adapter.
///
/// Concrete library objects cannot escape through the public handle:
/// ```compile_fail,E0616
/// fn expose(loaded: bpfman_kernel_aya::LoadedObject) {
///     let _aya = loaded.bpf;
/// }
/// ```
pub struct LoadedObject {
    native: Option<std::os::fd::OwnedFd>,
    bpf: aya::Ebpf,
    name: String,
    maps: Vec<String>,
}

/// Owned, loaded dispatcher. Dropping it releases only local handles.
pub struct Dispatcher(aya::Ebpf);

/// Opaque adopted tracepoint handle.
pub struct AyaTracepoint(aya::programs::TracePoint);

/// Adopted BTF tracing program with its validated entry/exit selection.
pub struct AyaTracing {
    fd: std::os::fd::OwnedFd,
    kind: bpfman_model::TracingKind,
}

/// Opaque adopted kernel probe handle.
pub struct AyaKprobe(aya::programs::KProbe);

/// Adopted native user-space probe program.
pub struct AyaUprobe(aya::programs::UProbe);

/// Opaque adopted extension handle.
pub struct AyaExtension(aya::programs::Extension);

/// Opaque owned, unpinned attachment.
#[derive(Debug)]
pub struct AyaLink(NativeLink);

#[derive(Debug)]
enum NativeLink {
    Aya(aya::programs::links::FdLink),
    Tracing(std::os::fd::OwnedFd),
}

/// Opaque owned outer-link descriptor.
pub struct AyaOuter(std::os::fd::OwnedFd);

/// Opaque retained dispatcher target for conditional switching and restoration.
pub struct AyaXdpTarget(aya::programs::ProgramFd);

/// Opaque adopted TC extension.
pub struct TcExtension(aya::programs::Extension);

/// Loaded native TC dispatcher; local handles do not own persistent attachments.
pub struct TcDispatcher(aya::Ebpf);

/// Adopted managed TC extension and retained namespace.
pub struct PreparedTc {
    direction: bpfman_model::TcDirection,
    program: bpfman_fs::TcProgram<TcExtension>,
    namespace: XdpNamespace,
}

/// Owned TC revision, including partial pin acquisitions.
pub struct TcStage {
    direction: bpfman_model::TcDirection,
    pins: bpfman_fs::TcPins,
    dispatcher: Option<TcDispatcher>,
    namespace: XdpNamespace,
    root: bpfman_fs::RuntimeIdentity,
}

/// Confined TC pins adopted while the explicit namespace selector was absent.
pub struct TcOrphan {
    pins: Box<bpfman_fs::TcPins>,
    selector: bpfman_model::NetworkNamespace,
    root: bpfman_fs::RuntimeIdentity,
}

/// Retained exact TC filter and both native targets across restoration attempts.
pub struct TcSwitch {
    direction: bpfman_model::TcDirection,
    namespace: XdpNamespace,
    root: bpfman_fs::RuntimeIdentity,
    handle: std::num::NonZeroU32,
    old_id: u32,
    new_id: u32,
    old: aya::programs::SchedClassifier,
    new: aya::programs::SchedClassifier,
}

/// Exact TC filter and clsact cleanup. Partial acquisition may own only clsact.
pub struct TcFilter {
    direction: bpfman_model::TcDirection,
    namespace: XdpNamespace,
    root: bpfman_fs::RuntimeIdentity,
    dispatcher: u32,
    handle: Option<std::num::NonZeroU32>,
    clsact_owned: bool,
}

/// Opaque native classifier with retained namespace evidence.
pub struct AyaTcx {
    program: aya::programs::SchedClassifier,
    namespace: XdpNamespace,
}
