//! Explicit credentials and deferred, registry-bound credential-file lookup.
use crate::Error;
use base64::{Engine, engine::general_purpose::STANDARD};

/// Registry credentials retained only in memory, including pending upload cleanup.
/// Debug output is redacted; credentials are never part of cache keys or records.
#[derive(Clone)]
pub struct Credentials {
    source: Source,
}

#[derive(Clone)]
enum Source {
    Basic(String),
    Bearer(String),
    Refresh {
        token: String,
        basic: Option<String>,
    },
    Files {
        registry: String,
        paths: Vec<std::path::PathBuf>,
    },
}

mod files;
mod helper;

impl Credentials {
    /// Capture a nonempty username and password. Passwords may contain colons.
    pub fn new(username: &str, password: &str) -> Result<Self, Error> {
        if username.is_empty()
            || password.is_empty()
            || username.contains(':')
            || username.len().saturating_add(password.len()) > 48 * 1024
        {
            return Err(Error::invalid(
                "registry auth requires a nonempty username and password within the size limit",
            ));
        }
        Ok(Self {
            source: Source::Basic(format!(
                "Basic {}",
                STANDARD.encode(format!("{username}:{password}"))
            )),
        })
    }

    /// Capture credential-file locations from the current process environment.
    /// No file is opened until the selected registry challenges authentication.
    /// Explicit callers can keep passing `None` for anonymous-only operation.
    /// The first existing file wins. Configured helpers run only when selected.
    /// Identity tokens are exchanged at the challenged bearer-token service.
    pub fn from_environment(reference: &crate::Reference) -> Self {
        Self {
            source: Source::Files {
                registry: reference.registry.clone(),
                paths: files::paths(|key| std::env::var_os(key)),
            },
        }
    }

    pub(super) fn resolve(
        &self,
        reference: &crate::Reference,
        flag: &std::sync::atomic::AtomicBool,
    ) -> Result<Option<Self>, Error> {
        crate::registry::check(flag)?;
        match &self.source {
            Source::Files { registry, paths } => {
                if registry != &reference.registry {
                    return Err(Error::invalid(
                        "credential lookup belongs to a different registry",
                    ));
                }
                files::resolve(registry, paths, flag)
            }
            _ => Ok(Some(self.clone())),
        }
    }

    pub(super) fn basic(&self) -> Result<&str, Error> {
        match &self.source {
            Source::Basic(value) => Ok(value),
            Source::Refresh {
                basic: Some(value), ..
            } => Ok(value),
            _ => Err(Error::invalid(
                "registry Basic authentication requires username and password",
            )),
        }
    }

    pub(super) fn bearer(&self) -> Option<&str> {
        match &self.source {
            Source::Bearer(value) => Some(value),
            _ => None,
        }
    }

    fn identity(token: String, basic: Option<String>) -> Result<Self, Error> {
        // OAuth refresh tokens are opaque printable ASCII. Form encoding, rather
        // than string interpolation, preserves spaces and reserved characters.
        if token.is_empty()
            || token.len() > 64 * 1024
            || !token.bytes().all(|b| b.is_ascii_graphic() || b == b' ')
        {
            return Err(Error::invalid("invalid registry identity token"));
        }
        Ok(Self {
            source: Source::Refresh { token, basic },
        })
    }

    pub(super) fn refresh(&self) -> Option<&str> {
        match &self.source {
            Source::Refresh { token, .. } => Some(token),
            _ => None,
        }
    }
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials { .. }")
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod identity_tests;

#[cfg(test)]
#[path = "../../../tests/support/publish_registry.rs"]
mod test_registry;
