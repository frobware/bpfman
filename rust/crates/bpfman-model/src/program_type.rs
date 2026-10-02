use core::{fmt, str::FromStr};

/// Bpfman's attach-oriented program taxonomy, distinct from kernel types.
///
/// For example, TC and TCX both load as `schedcls`, and all four probe
/// variants load as `kprobe`. Keeping the finer vocabulary prevents losing
/// the attachment distinction. Spellings match Go's `program.go`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ProgramType {
    /// Express Data Path program.
    Xdp,
    /// Traffic-control dispatcher extension.
    Tc,
    /// Native multi-program traffic-control attachment.
    Tcx,
    /// Kernel tracepoint program.
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

impl ProgramType {
    /// Every supported type in the same order as Go's `AllProgramTypes`.
    pub const ALL: [Self; 11] = [
        Self::Xdp,
        Self::Tc,
        Self::Tcx,
        Self::Tracepoint,
        Self::Kprobe,
        Self::Kretprobe,
        Self::Uprobe,
        Self::Uretprobe,
        Self::Fentry,
        Self::Fexit,
        Self::Lsm,
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

impl fmt::Display for ProgramType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An unknown program type; callers retain the input for boundary diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseProgramTypeError;

impl fmt::Display for ParseProgramTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("unknown program type")
    }
}

impl core::error::Error for ParseProgramTypeError {}

impl FromStr for ProgramType {
    type Err = ParseProgramTypeError;

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
            _ => Err(ParseProgramTypeError),
        }
    }
}
