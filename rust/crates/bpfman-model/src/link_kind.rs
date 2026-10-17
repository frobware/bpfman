use core::{fmt, str::FromStr};

/// Managed attachment kinds, distinct from owning-program and kernel types.
/// Spellings match Go's link-kind filters.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LinkKind {
    /// Express Data Path dispatcher extension.
    Xdp,
    /// Traffic-control dispatcher extension.
    Tc,
    /// Native multi-program traffic-control attachment.
    Tcx,
    /// Kernel tracepoint attachment.
    Tracepoint,
    /// Kernel function entry probe.
    Kprobe,
    /// Kernel function return probe.
    Kretprobe,
    /// User-space function entry probe.
    Uprobe,
    /// User-space function return probe.
    Uretprobe,
    /// BTF-based function entry tracing.
    Fentry,
    /// BTF-based function exit tracing.
    Fexit,
    /// Linux Security Module hook.
    Lsm,
}

impl LinkKind {
    /// Every supported kind in Go's `AllLinkKinds` order.
    pub const ALL: [Self; 11] = [
        Self::Tracepoint,
        Self::Kprobe,
        Self::Kretprobe,
        Self::Uprobe,
        Self::Uretprobe,
        Self::Fentry,
        Self::Fexit,
        Self::Lsm,
        Self::Xdp,
        Self::Tc,
        Self::Tcx,
    ];

    /// Canonical spelling used by the CLI and persistence boundary.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Xdp => "xdp",
            Self::Tc => "tc",
            Self::Tcx => "tcx",
            Self::Tracepoint => "tracepoint",
            Self::Kprobe => "kprobe",
            Self::Kretprobe => "kretprobe",
            Self::Uprobe => "uprobe",
            Self::Uretprobe => "uretprobe",
            Self::Fentry => "fentry",
            Self::Fexit => "fexit",
            Self::Lsm => "lsm",
        }
    }
}

impl fmt::Display for LinkKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An unknown link kind; callers retain the input for boundary diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseLinkKindError;

impl fmt::Display for ParseLinkKindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown link kind")
    }
}

impl core::error::Error for ParseLinkKindError {}

impl FromStr for LinkKind {
    type Err = ParseLinkKindError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        // Go's boundary parser is case-sensitive and does not trim whitespace.
        match value {
            "xdp" => Ok(Self::Xdp),
            "tc" => Ok(Self::Tc),
            "tcx" => Ok(Self::Tcx),
            "tracepoint" => Ok(Self::Tracepoint),
            "kprobe" => Ok(Self::Kprobe),
            "kretprobe" => Ok(Self::Kretprobe),
            "uprobe" => Ok(Self::Uprobe),
            "uretprobe" => Ok(Self::Uretprobe),
            "fentry" => Ok(Self::Fentry),
            "fexit" => Ok(Self::Fexit),
            "lsm" => Ok(Self::Lsm),
            _ => Err(ParseLinkKindError),
        }
    }
}
