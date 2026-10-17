//! Explicit caller credentials; no ambient credential-store lookup.
use crate::Error;
use base64::{Engine, engine::general_purpose::STANDARD};

/// Registry credentials retained only in memory, including pending upload cleanup.
/// Debug output is redacted; credentials are never part of cache keys or records.
#[derive(Clone)]
pub struct Credentials {
    basic: String,
}

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
            basic: format!(
                "Basic {}",
                STANDARD.encode(format!("{username}:{password}"))
            ),
        })
    }

    pub(super) fn basic(&self) -> &str {
        &self.basic
    }
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Credentials { .. }")
    }
}
