//! Read-only Docker/Podman credential files. No helper processes or file writes.
use super::{Credentials, Source};
use crate::{Error, registry::check};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use std::{
    collections::BTreeMap, ffi::OsString, io::Read, path::PathBuf, sync::atomic::AtomicBool,
};

const FILE_LIMIT: usize = 4 * 1024 * 1024;

pub(super) fn paths(mut env: impl FnMut(&str) -> Option<OsString>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut add = |path: PathBuf| {
        if !paths.contains(&path) {
            paths.push(path);
        }
    };
    let mut get = |key: &str| env(key).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(path) = get("REGISTRY_AUTH_FILE") {
        add(path);
    }
    if let Some(path) = get("XDG_RUNTIME_DIR") {
        add(path.join("containers/auth.json"));
    }
    if let Some(home) = get("HOME") {
        add(home.join(".config/containers/auth.json"));
        add(get("DOCKER_CONFIG")
            .unwrap_or_else(|| home.join(".docker"))
            .join("config.json"));
    }
    paths
}

pub(super) fn resolve(
    registry: &str,
    paths: &[PathBuf],
    flag: &AtomicBool,
) -> Result<Option<Credentials>, Error> {
    for path in paths {
        check(flag)?;
        let Some(file) = bpfman_fs::open_registry_credentials(path)
            .map_err(|e| Error::caused("open registry credential store", e))?
        else {
            continue;
        };
        let mut reader = file.take((FILE_LIMIT + 1) as u64);
        let mut bytes = Vec::new();
        let mut chunk = [0; 16 * 1024];
        loop {
            check(flag)?;
            let count = match reader.read(&mut chunk) {
                Ok(count) => count,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(Error::caused("read registry credential file", e)),
            };
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..count]);
            if bytes.len() > FILE_LIMIT {
                return Err(Error::invalid("registry credential file exceeds 4 MiB"));
            }
        }
        check(flag)?;
        // JSON/base64 errors can contain offending secret values. Never retain
        // those diagnostic sources, or include file contents in tracing.
        let object: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&bytes)
            .map_err(|_| Error::invalid("invalid registry credential file JSON"))?;
        let config: Config = serde_json::from_value(serde_json::Value::Object(object))
            .map_err(|_| Error::invalid("invalid registry credential file JSON"))?;
        let result = config.credentials(registry)?;
        check(flag)?;
        // Go selects one store, rather than merging/falling through when the
        // chosen store has no credentials for this registry.
        return Ok(result);
    }
    check(flag)?;
    Ok(None)
}

#[derive(Deserialize)]
struct Config {
    #[serde(default)]
    auths: BTreeMap<String, serde_json::Value>,
    #[serde(default, rename = "credHelpers")]
    helpers: BTreeMap<String, String>,
    #[serde(default, rename = "credsStore")]
    store: String,
}

impl Config {
    fn credentials(&self, registry: &str) -> Result<Option<Credentials>, Error> {
        if !self.store.is_empty() || select(&self.helpers, registry)?.is_some_and(|s| !s.is_empty())
        {
            return Err(Error::invalid(
                "registry credential helpers are unsupported; use a credential file with inline auth or BPFMAN_REGISTRY_AUTH",
            ));
        }
        let Some(value) = select(&self.auths, registry)? else {
            return Ok(None);
        };
        if !value.is_object() {
            return Err(Error::invalid("invalid registry credential entry"));
        }
        let auth: Auth = serde_json::from_value(value.clone())
            .map_err(|_| Error::invalid("invalid registry credential entry"))?;
        if !auth.token.is_empty() {
            if auth.token.len() > 64 * 1024 || !auth.token.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(Error::invalid("invalid registry credential token"));
            }
            return Ok(Some(Credentials {
                source: Source::Bearer(format!("Bearer {}", auth.token)),
            }));
        }
        if !auth.identity_token.is_empty() {
            return Err(Error::invalid(
                "registry identity-token exchange is unsupported; use BPFMAN_REGISTRY_AUTH",
            ));
        }
        let (username, password) = if auth.auth.is_empty() {
            (auth.username, auth.password)
        } else {
            if auth.auth.len() > 64 * 1024 {
                return Err(Error::invalid(
                    "registry credential auth exceeds size limit",
                ));
            }
            let bytes = STANDARD
                .decode(&auth.auth)
                .map_err(|_| Error::invalid("invalid registry credential auth encoding"))?;
            let decoded = String::from_utf8(bytes)
                .map_err(|_| Error::invalid("registry credential auth must be UTF-8"))?;
            let (username, password) = decoded.split_once(':').ok_or_else(|| {
                Error::invalid("registry credential auth requires username:password")
            })?;
            (username.to_owned(), password.to_owned())
        };
        if username.is_empty() && password.is_empty() {
            return Ok(None);
        }
        Credentials::new(&username, &password).map(Some)
    }
}

#[derive(Deserialize)]
struct Auth {
    #[serde(default)]
    auth: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
    #[serde(default, rename = "registrytoken")]
    token: String,
    #[serde(default, rename = "identitytoken")]
    identity_token: String,
}

fn select<'a, T>(entries: &'a BTreeMap<String, T>, registry: &str) -> Result<Option<&'a T>, Error> {
    if let Some(value) = entries.get(registry) {
        return Ok(Some(value));
    }
    let mut matches = entries.iter().filter(|(key, _)| {
        let authority = key
            .strip_prefix("https://")
            .or_else(|| key.strip_prefix("http://"))
            .unwrap_or(key)
            .split('/')
            .next()
            .unwrap_or("");
        authority.eq_ignore_ascii_case(registry) || (docker_hub(authority) && docker_hub(registry))
    });
    let selected = matches.next().map(|(_, value)| value);
    if matches.next().is_some() {
        return Err(Error::invalid("ambiguous registry credential aliases"));
    }
    Ok(selected)
}

fn docker_hub(authority: &str) -> bool {
    ["docker.io", "index.docker.io", "registry-1.docker.io"]
        .iter()
        .any(|host| authority.eq_ignore_ascii_case(host))
}

#[cfg(test)]
mod tests;
