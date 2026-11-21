//! Typed network fixtures and namespace-scoped command execution.

use std::{collections::BTreeMap, num::NonZeroU64, process::Command};

use bpfman_shell_runtime::{
    Argument, CommandResponse, EndpointSide, NetworkHandle, NetworkRole, Value,
};

use super::{
    files,
    jobs::Jobs,
    process::{self, Interrupt},
};

mod config;
mod pool;
mod route;
mod topology;

enum Topology {
    Host(topology::HostPair),
    Isolated(topology::IsolatedPair),
}

#[derive(Default)]
pub(super) struct Network {
    last_id: u64,
    pairs: BTreeMap<NonZeroU64, Topology>,
}

impl Network {
    pub(super) fn invoke(
        &mut self,
        arguments: &[Argument],
        origin: String,
        interrupt: &Interrupt,
        jobs: &mut Jobs,
    ) -> Result<CommandResponse, String> {
        let (subcommand, rest) = arguments.split_first().ok_or("net: subcommand required")?;
        let subcommand = super::argument(subcommand)?;
        match (subcommand.as_str(), rest) {
            ("netns-veth-pair", []) => self.isolated_pair(origin, interrupt),
            ("netns-veth-pair", _) => Err("net netns-veth-pair takes no arguments".to_owned()),
            ("release", [Argument::Value(Value::Network(handle))]) => self.release(handle),
            ("release", _) => Err("net release requires one network pair".to_owned()),
            ("exec" | "start", [Argument::Value(Value::Network(handle)), command @ ..])
                if !command.is_empty() =>
            {
                let namespace = self.namespace(handle)?;
                let argv = ["ip", "netns", "exec", namespace]
                    .into_iter()
                    .map(|word| Argument::Word(word.to_owned()))
                    .chain(command.iter().cloned())
                    .collect::<Vec<_>>();
                if subcommand == "start" {
                    jobs.start_in_namespace(&argv, origin)
                } else {
                    exec(&argv, interrupt)
                }
            }
            ("exec" | "start", _) => Err(format!(
                "net {subcommand} requires an endpoint and a command"
            )),
            ("veth-pair", arguments) => self.host_pair(arguments, origin, interrupt),
            _ => Err(format!("net: unknown subcommand {subcommand:?}")),
        }
    }

    fn isolated_pair(
        &mut self,
        origin: String,
        interrupt: &Interrupt,
    ) -> Result<CommandResponse, String> {
        let id = self.next_id()?;
        let names = topology::PairNames::isolated();
        let claim = pool::ClaimNames {
            namespace: &names.a.namespace,
            second_namespace: Some(&names.b.namespace),
            root_link: &names.a.link,
        };
        let (lease, addresses) = pool::claim(claim, origin, interrupt)?;
        let (topology, a, b) = topology::IsolatedPair::create(names, lease, addresses, interrupt)?;
        self.pairs.insert(id, Topology::Isolated(topology));
        self.last_id = id.get();
        Ok(CommandResponse::success(Value::Network(
            NetworkHandle::pair(id, a, b),
        )))
    }

    fn host_pair(
        &mut self,
        arguments: &[Argument],
        origin: String,
        interrupt: &Interrupt,
    ) -> Result<CommandResponse, String> {
        let id = self.next_id()?;
        let plan = config::HostPlan::parse(arguments)?;
        route::ensure(interrupt)?;
        let (lease, prefixes) = match plan.addresses {
            config::Addresses::Pooled => {
                let claim = pool::ClaimNames {
                    namespace: &plan.names.namespace,
                    second_namespace: None,
                    root_link: &plan.names.host_link,
                };
                let (lease, addresses) = pool::claim(claim, origin, interrupt)?;
                (Some(lease), addresses.map(config::Prefix::pooled))
            }
            config::Addresses::Explicit(prefixes) => (None, prefixes),
        };
        let (topology, observed) =
            topology::HostPair::create(plan.names, lease, prefixes, interrupt)?;
        self.pairs.insert(id, Topology::Host(topology));
        self.last_id = id.get();
        Ok(CommandResponse::success(Value::Network(
            NetworkHandle::host_pair(id, observed),
        )))
    }

    fn next_id(&self) -> Result<NonZeroU64, String> {
        self.last_id
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or_else(|| "exhausted network identities".to_owned())
    }

    fn release(&mut self, handle: &NetworkHandle) -> Result<CommandResponse, String> {
        if matches!(handle.role(), NetworkRole::Endpoint(_)) {
            return Err("net release requires the pair, not an endpoint".to_owned());
        }
        if let Some(pair) = self.pairs.get_mut(&handle.id()) {
            match pair {
                Topology::Host(pair) => pair.release()?,
                Topology::Isolated(pair) => pair.release()?,
            }
            self.pairs.remove(&handle.id());
        } else if handle.id().get() > self.last_id {
            return Err("unknown network pair".to_owned());
        }
        Ok(CommandResponse::success(Value::Absent))
    }

    fn namespace(&self, handle: &NetworkHandle) -> Result<&str, String> {
        let topology = self
            .pairs
            .get(&handle.id())
            .ok_or("network pair has been released")?;
        match (topology, handle.role()) {
            (Topology::Host(pair), NetworkRole::HostPair) => Ok(&pair.names.namespace),
            (Topology::Isolated(pair), NetworkRole::Endpoint(EndpointSide::A)) => {
                Ok(&pair.names.a.namespace)
            }
            (Topology::Isolated(pair), NetworkRole::Endpoint(EndpointSide::B)) => {
                Ok(&pair.names.b.namespace)
            }
            (Topology::Isolated(_), NetworkRole::Pair) => {
                Err("netns-veth-pair has two endpoints; use the a or b field".to_owned())
            }
            _ => Err("network handle does not match its topology".to_owned()),
        }
    }
}

fn exec(arguments: &[Argument], interrupt: &Interrupt) -> Result<CommandResponse, String> {
    let mut files = files::AdapterFiles::default();
    let arguments = arguments
        .iter()
        .map(|argument| files.argument(argument))
        .collect::<Result<Vec<_>, _>>()?;
    let (program, args) = arguments
        .split_first()
        .ok_or("net exec requires a command")?;
    let output = process::capture(Command::new(program).args(args), interrupt)?;
    Ok(process::response(output))
}
