//! Session registry fixture and rewriting of the reserved OCI alias.

use std::{
    net::{IpAddr, SocketAddr},
    sync::atomic::{AtomicU64, Ordering},
};

use bpfman_shell_runtime::{Argument, Value};

mod loopback;

const ALIAS: &str = "bpfman-e2e-registry.example.com";
const REGISTRY_ENV: &str = "BPFMAN_E2E_IMAGE_REGISTRY";
static REFERENCE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
pub(crate) struct Registry {
    server: Option<loopback::Registry>,
}

impl Registry {
    pub(super) fn invoke(&mut self, arguments: &[Argument]) -> Result<Value, String> {
        let (mode, rest) = arguments
            .split_first()
            .ok_or("registry: expected ref, host or url")?;
        let mode = super::argument(mode)?;
        let value = match (mode.as_str(), rest) {
            ("ref", [name]) => reference(&super::argument(name)?)?,
            ("host", []) => self.endpoint()?.authority,
            ("url", []) => {
                let endpoint = self.endpoint()?;
                format!("{}://{}", endpoint.scheme, endpoint.authority)
            }
            ("ref", _) => return Err("registry ref requires one name".to_owned()),
            ("host" | "url", _) => return Err(format!("registry {mode} takes no arguments")),
            _ => return Err(format!("registry: unknown subcommand {mode:?}")),
        };
        Ok(Value::String(value))
    }

    pub(super) fn rewrite(&mut self, argv: &mut [String]) -> Result<(), String> {
        let position = match argv {
            [noun, operation, _, ..]
                if noun == "image" && matches!(operation.as_str(), "build" | "inspect") =>
            {
                Some(2)
            }
            [noun, operation, source, _, ..]
                if noun == "program" && operation == "load" && source == "image" =>
            {
                Some(3)
            }
            _ => None,
        };
        if let Some(entry) = position.and_then(|position| argv.get_mut(position)) {
            self.rewrite_ref(entry)?;
        }
        let mut words = argv.iter_mut();
        while let Some(word) = words.next() {
            match word.as_str() {
                "-t" | "--tag" => {
                    if let Some(value) = words.next() {
                        self.rewrite_ref(value)?;
                    }
                }
                _ => {
                    if let Some((key, value)) = word.split_once('=')
                        && matches!(key, "-t" | "--tag")
                    {
                        let mut value = value.to_owned();
                        self.rewrite_ref(&mut value)?;
                        *word = format!("{key}={value}");
                    }
                }
            }
        }
        Ok(())
    }

    fn rewrite_ref(&mut self, reference: &mut String) -> Result<(), String> {
        let Some(suffix) = reference.strip_prefix(ALIAS) else {
            return Ok(());
        };
        if suffix.is_empty() || suffix == "/" {
            return Err(format!("{ALIAS} reference requires an image name"));
        }
        if suffix.starts_with('/') {
            *reference = format!("{}{suffix}", self.endpoint()?.authority);
        }
        Ok(())
    }

    fn endpoint(&mut self) -> Result<Endpoint, String> {
        if let Ok(value) = std::env::var(REGISTRY_ENV)
            && !value.is_empty()
        {
            return Endpoint::parse(&value);
        }
        if self.server.is_none() {
            self.server = Some(
                loopback::Registry::start()
                    .map_err(|error| format!("start fixture registry: {error}"))?,
            );
        }
        let server = self
            .server
            .as_ref()
            .ok_or("fixture registry did not start")?;
        Ok(Endpoint {
            authority: server.address().to_string(),
            scheme: "http",
        })
    }
}

struct Endpoint {
    authority: String,
    scheme: &'static str,
}

impl Endpoint {
    fn parse(raw: &str) -> Result<Self, String> {
        let (rest, scheme) = if let Some(rest) = raw.strip_prefix("http://") {
            (rest, Some("http"))
        } else if let Some(rest) = raw.strip_prefix("https://") {
            (rest, Some("https"))
        } else {
            (raw, None)
        };
        let authority = rest.strip_suffix('/').unwrap_or(rest);
        if authority.is_empty()
            || authority
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'?' | b'#' | b'@'))
        {
            return Err(format!("{REGISTRY_ENV} must be a registry host"));
        }
        let host = authority
            .strip_prefix("[")
            .and_then(|rest| rest.split_once(']'))
            .map_or_else(
                || authority.split(':').next().unwrap_or_default(),
                |(host, _)| host,
            );
        let loopback = authority
            .parse::<SocketAddr>()
            .is_ok_and(|address| address.ip().is_loopback())
            || host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        Ok(Self {
            authority: authority.to_owned(),
            scheme: scheme.unwrap_or(if loopback { "http" } else { "https" }),
        })
    }
}

fn reference(name: &str) -> Result<String, String> {
    if name.trim().is_empty() {
        return Err("registry ref: name must not be empty".to_owned());
    }
    let sanitized = name
        .to_lowercase()
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let repository = if sanitized.is_empty() {
        "bytecode"
    } else {
        &sanitized
    };
    let sequence = REFERENCE_SEQUENCE
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1);
    Ok(format!(
        "{ALIAS}/bpfman-e2e/{repository}:{}-{sequence}",
        std::process::id()
    ))
}
