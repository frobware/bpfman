//! Load-bound BTF tracing and LSM hooks, independent of BTF and kernel-library types.
use crate::{ProgramType, Symbol};

/// Native BTF attachment selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TracingKind {
    /// Execute before the function body.
    Fentry,
    /// Execute when the function returns.
    Fexit,
    /// Execute an LSM hook bound at load time.
    Lsm,
}

impl TracingKind {
    /// Managed program family required by this attachment.
    pub fn program_type(self) -> ProgramType {
        match self {
            Self::Fentry => ProgramType::Fentry,
            Self::Fexit => ProgramType::Fexit,
            Self::Lsm => ProgramType::Lsm,
        }
    }
}

/// Function or LSM hook and selection already bound into the loaded program.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TracingLink {
    /// Requested load-time function or LSM hook spelling.
    pub function: Symbol,
    /// Tracing or LSM selection.
    pub kind: TracingKind,
}
