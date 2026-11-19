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

mod pool;
mod topology;

#[derive(Default)]
pub(super) struct Network {
    last_id: u64,
    isolated: BTreeMap<NonZeroU64, topology::IsolatedPair>,
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
            ("veth-pair", _) => Err("net veth-pair is not yet implemented".to_owned()),
            _ => Err(format!("net: unknown subcommand {subcommand:?}")),
        }
    }

    fn isolated_pair(
        &mut self,
        origin: String,
        interrupt: &Interrupt,
    ) -> Result<CommandResponse, String> {
        let id = self
            .last_id
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .ok_or("exhausted network identities")?;
        let names = topology::PairNames::isolated();
        let (lease, addresses) = pool::claim(&names, origin, interrupt)?;
        let (topology, a, b) = topology::IsolatedPair::create(names, lease, addresses, interrupt)?;
        self.isolated.insert(id, topology);
        self.last_id = id.get();
        Ok(CommandResponse::success(Value::Network(
            NetworkHandle::pair(id, a, b),
        )))
    }

    fn release(&mut self, handle: &NetworkHandle) -> Result<CommandResponse, String> {
        if handle.role() != NetworkRole::Pair {
            return Err("net release requires the pair, not an endpoint".to_owned());
        }
        if let Some(pair) = self.isolated.get_mut(&handle.id()) {
            pair.release()?;
            self.isolated.remove(&handle.id());
        } else if handle.id().get() > self.last_id {
            return Err("unknown network pair".to_owned());
        }
        Ok(CommandResponse::success(Value::Absent))
    }

    fn namespace(&self, handle: &NetworkHandle) -> Result<&str, String> {
        let topology = self
            .isolated
            .get(&handle.id())
            .ok_or("network pair has been released")?;
        match handle.role() {
            NetworkRole::Endpoint(EndpointSide::A) => Ok(&topology.names.a.namespace),
            NetworkRole::Endpoint(EndpointSide::B) => Ok(&topology.names.b.namespace),
            NetworkRole::Pair => {
                Err("netns-veth-pair has two endpoints; use the a or b field".to_owned())
            }
        }
    }
}

fn exec(arguments: &[Argument], interrupt: &Interrupt) -> Result<CommandResponse, String> {
    let mut files = Vec::new();
    let arguments = arguments
        .iter()
        .map(|argument| files::external_argument(argument, &mut files))
        .collect::<Result<Vec<_>, _>>()?;
    let (program, args) = arguments
        .split_first()
        .ok_or("net exec requires a command")?;
    let output = process::capture(Command::new(program).args(args), interrupt)?;
    Ok(process::response(output))
}
