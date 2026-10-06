use bpfman_kernel::{Error, ErrorKind};

#[derive(Debug, thiserror::Error)]
pub(crate) enum LoadCause {
    #[error("parse local ELF")]
    Parse(#[source] Box<aya_obj::ParseError>),
    #[error("load ELF maps and relocations")]
    Kernel(#[source] Box<aya::EbpfError>),
    #[error("observe loaded map identity")]
    Map(#[source] Box<aya::maps::MapError>),
    #[error("load program")]
    Program(#[source] Box<aya::programs::ProgramError>),
    #[error("{0}")]
    Invalid(String),
    #[error("{0} execution is not implemented in this Rust load slice")]
    Unsupported(&'static str),
}

impl From<LoadCause> for Error {
    fn from(cause: LoadCause) -> Self {
        let kind = match cause {
            LoadCause::Invalid(_) | LoadCause::Parse(_) => ErrorKind::InvalidInput,
            LoadCause::Unsupported(_) => ErrorKind::Unsupported,
            _ => ErrorKind::Unavailable,
        };
        Error::new(kind, "prepare or load kernel program", cause)
    }
}

pub(crate) fn filesystem(error: bpfman_fs::Error) -> Error {
    let kind = match error.kind() {
        bpfman_fs::ErrorKind::UnsafeLayout => ErrorKind::InvalidData,
        _ => ErrorKind::Unavailable,
    };
    Error::new(kind, "managed kernel artifact", error)
}

pub(crate) fn map<T>(
    e: bpfman_core::EffectFailure<T, bpfman_fs::Error>,
) -> bpfman_core::EffectFailure<T, Error> {
    bpfman_core::EffectFailure {
        remaining: e.remaining,
        cause: filesystem(e.cause),
    }
}
