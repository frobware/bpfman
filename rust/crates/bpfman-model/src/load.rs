use crate::{InvalidSymbol, ProgramSpec, ProgramType, Symbol};

impl TryFrom<&str> for Symbol {
    type Error = InvalidSymbol;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.is_empty() || value.trim() != value || value.contains([':', '\0']) {
            return Err(InvalidSymbol);
        }
        Ok(Self(value.into()))
    }
}

impl Symbol {
    /// The validated symbol spelling, without normalisation or truncation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl ProgramSpec {
    /// Entry function selected from the ELF, not the truncated kernel name.
    pub fn name(&self) -> &Symbol {
        match self {
            Self::Xdp(name)
            | Self::Tc(name)
            | Self::Tcx(name)
            | Self::Tracepoint(name)
            | Self::Kprobe(name)
            | Self::Kretprobe(name)
            | Self::Uprobe(name)
            | Self::Uretprobe(name)
            | Self::Fentry { name, .. }
            | Self::Fexit { name, .. }
            | Self::Lsm { name, .. } => name,
        }
    }

    /// Bpfman's load/attach taxonomy, derived from the variant.
    pub const fn kind(&self) -> ProgramType {
        match self {
            Self::Xdp(_) => ProgramType::Xdp,
            Self::Tc(_) => ProgramType::Tc,
            Self::Tcx(_) => ProgramType::Tcx,
            Self::Tracepoint(_) => ProgramType::Tracepoint,
            Self::Kprobe(_) => ProgramType::Kprobe,
            Self::Kretprobe(_) => ProgramType::Kretprobe,
            Self::Uprobe(_) => ProgramType::Uprobe,
            Self::Uretprobe(_) => ProgramType::Uretprobe,
            Self::Fentry { .. } => ProgramType::Fentry,
            Self::Fexit { .. } => ProgramType::Fexit,
            Self::Lsm { .. } => ProgramType::Lsm,
        }
    }
}

// Keep the pure dependency closure empty: Aya enables thiserror's std feature
// in the application graph, which Cargo would unify into this crate too.
impl core::fmt::Display for crate::InvalidSymbol {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("names and targets must be nonempty, have no surrounding whitespace, and contain no colon or NUL")
    }
}
impl core::error::Error for crate::InvalidSymbol {}
