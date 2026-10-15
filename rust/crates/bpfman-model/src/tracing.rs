//! Load-bound function tracing, independent of BTF and kernel-library types.
use crate::{ProgramType, Symbol};

/// Native function tracing selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TracingKind {
    /// Execute before the function body.
    Fentry,
    /// Execute when the function returns.
    Fexit,
}

impl TracingKind {
    /// Managed program family required by this attachment.
    pub fn program_type(self) -> ProgramType {
        match self {
            Self::Fentry => ProgramType::Fentry,
            Self::Fexit => ProgramType::Fexit,
        }
    }
}

/// Function and selection already bound into the loaded program.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TracingLink {
    /// Requested load-time function spelling.
    pub function: Symbol,
    /// Entry or exit selection.
    pub kind: TracingKind,
}
