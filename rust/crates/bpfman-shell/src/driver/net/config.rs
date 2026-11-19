//! Validation of host-pair names and IPv4 prefix arguments.

use std::{collections::BTreeMap, fmt, net::Ipv4Addr};

use bpfman_shell_runtime::Argument;

use super::topology::{HostNames, unique_base};

pub(super) struct HostPlan {
    pub names: HostNames,
    pub addresses: Addresses,
}

pub(super) enum Addresses {
    Pooled,
    Explicit([Prefix; 2]),
}

#[derive(Clone, Copy)]
pub(super) struct Prefix {
    pub address: Ipv4Addr,
    width: u8,
}

impl Prefix {
    pub(super) fn pooled(address: Ipv4Addr) -> Self {
        Self { address, width: 30 }
    }

    fn parse(text: &str) -> Result<Self, String> {
        let (address, width) = text
            .split_once('/')
            .ok_or_else(|| format!("{text:?} is not in CIDR form"))?;
        let address = address
            .parse()
            .map_err(|_| format!("{text:?} is not an IPv4 prefix"))?;
        let width = width
            .parse::<u8>()
            .map_err(|_| format!("{text:?} is not an IPv4 prefix"))?;
        if width > 32 {
            return Err(format!("{text:?} is not an IPv4 prefix"));
        }
        Ok(Self { address, width })
    }
}

impl fmt::Display for Prefix {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}/{}", self.address, self.width)
    }
}

impl HostPlan {
    pub(super) fn parse(arguments: &[Argument]) -> Result<Self, String> {
        let mut options = BTreeMap::new();
        let mut words = arguments.iter();
        while let Some(word) = words.next() {
            let text = crate::driver::argument(word)?;
            let (name, inline) = text
                .split_once('=')
                .map_or((text.as_str(), None), |(a, b)| (a, Some(b)));
            let key = match name {
                "--ns" => "--ns",
                "--host-link" => "--host-link",
                "--peer-link" => "--peer-link",
                "--host-addr" => "--host-addr",
                "--peer-addr" => "--peer-addr",
                _ => return Err(format!("net veth-pair: unexpected argument {text:?}")),
            };
            let value = if let Some(inline) = inline {
                inline.to_owned()
            } else {
                let next = words
                    .next()
                    .ok_or_else(|| format!("{name} requires a value"))?;
                crate::driver::argument(next)?
            };
            options.insert(key, (!value.is_empty()).then_some(value));
        }
        let mut get = |name| options.remove(name).flatten();
        let names = match (get("--ns"), get("--host-link"), get("--peer-link")) {
            (None, None, None) => {
                let base = unique_base();
                HostNames {
                    host_link: format!("{base}a"),
                    peer_link: format!("{base}b"),
                    namespace: base,
                }
            }
            (Some(namespace), Some(host_link), Some(peer_link)) => {
                validate_name(&namespace, None)?;
                validate_name(&host_link, Some(15))?;
                validate_name(&peer_link, Some(15))?;
                if host_link == peer_link {
                    return Err("host and peer interfaces must have distinct names".to_owned());
                }
                HostNames {
                    namespace,
                    host_link,
                    peer_link,
                }
            }
            _ => return Err("--ns / --host-link / --peer-link must be given together".to_owned()),
        };
        let addresses = match (get("--host-addr"), get("--peer-addr")) {
            (None, None) => Addresses::Pooled,
            (Some(a), Some(b)) => Addresses::Explicit([Prefix::parse(&a)?, Prefix::parse(&b)?]),
            _ => return Err("--host-addr and --peer-addr must be given together".to_owned()),
        };
        Ok(Self { names, addresses })
    }
}

fn validate_name(name: &str, limit: Option<usize>) -> Result<(), String> {
    if matches!(name, "." | "..")
        || name.starts_with('-')
        || name
            .bytes()
            .any(|byte| byte == b'/' || byte.is_ascii_whitespace() || byte == 0)
        || limit.is_some_and(|limit| name.len() > limit)
    {
        return Err(format!("invalid network name {name:?}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_only_produce_complete_host_pairs() -> Result<(), String> {
        let parse = |words: &[&str]| {
            HostPlan::parse(
                &words
                    .iter()
                    .map(|word| Argument::Word((*word).to_owned()))
                    .collect::<Vec<_>>(),
            )
        };
        assert!(parse(&["--ns=test-only"]).is_err());
        assert!(parse(&["--host-addr=192.0.2.1/30"]).is_err());
        assert!(parse(&["--host-addr=192.0.2.1/33", "--peer-addr=192.0.2.2/30"]).is_err());
        let plan = parse(&[
            "--ns=test-net",
            "--host-link",
            "test-host",
            "--peer-link=test-peer",
            "--host-addr=192.0.2.1/32",
            "--peer-addr",
            "192.0.2.2/24",
        ])?;
        assert_eq!(plan.names.namespace, "test-net");
        let Addresses::Explicit([a, b]) = plan.addresses else {
            return Err("expected explicit addresses".to_owned());
        };
        assert_eq!(a.to_string(), "192.0.2.1/32");
        assert_eq!(b.to_string(), "192.0.2.2/24");
        Ok(())
    }
}
