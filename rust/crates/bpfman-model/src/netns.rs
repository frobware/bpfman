use alloc::string::String;
use core::{fmt, str::FromStr};

/// Network namespace selection: empty means the caller's namespace; otherwise an
/// absolute namespace path. Parsing performs no filesystem or kernel I/O.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NetworkNamespace(String);

/// A namespace path must be absolute and contain no NUL bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidNetworkNamespace;

impl fmt::Display for InvalidNetworkNamespace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("expected an absolute network namespace path")
    }
}

impl core::error::Error for InvalidNetworkNamespace {}

impl FromStr for NetworkNamespace {
    type Err = InvalidNetworkNamespace;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if (!raw.is_empty() && !raw.starts_with('/')) || raw.contains('\0') {
            return Err(InvalidNetworkNamespace);
        }
        Ok(Self(raw.into()))
    }
}

impl NetworkNamespace {
    /// Stored path, empty for the caller's namespace.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
