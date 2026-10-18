use clap::{Args, Subcommand, ValueEnum};
use std::num::{NonZeroU32, NonZeroU64};

use super::OutputFormat;

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct DispatcherArgs {
    #[command(subcommand)]
    command: Option<DispatcherCommand>,
    #[command(flatten)]
    list: ListArgs,
}

impl DispatcherArgs {
    pub(super) fn into_command(self) -> DispatcherCommand {
        self.command.unwrap_or(DispatcherCommand::List(self.list))
    }
}

#[derive(Subcommand)]
pub(crate) enum DispatcherCommand {
    /// List committed XDP and TC dispatchers.
    List(ListArgs),
    /// Delete stored dispatcher and extension-link records.
    #[command(hide = true)]
    Delete {
        /// Dispatcher type.
        #[arg(value_enum)]
        kind: DispatcherKind,
        /// Network namespace inode.
        nsid: NonZeroU64,
        /// Interface index.
        ifindex: NonZeroU32,
    },
    /// Read the committed complete dispatcher snapshot.
    Get {
        /// Dispatcher type.
        #[arg(value_enum)]
        kind: DispatcherKind,
        /// Network namespace inode.
        nsid: NonZeroU64,
        /// Interface index.
        ifindex: NonZeroU32,
        /// Output format.
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
}

#[derive(Args)]
pub(crate) struct ListArgs {
    /// Filter by dispatcher type.
    #[arg(long = "type", value_enum)]
    kind: Option<DispatcherKind>,
    /// Filter by network namespace ID; zero selects all namespaces.
    #[arg(long, default_value_t = 0)]
    nsid: u64,
    /// Filter by interface index; zero selects all interfaces.
    #[arg(long, default_value_t = 0)]
    ifindex: u32,
    /// Output format.
    #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
    output: OutputFormat,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum DispatcherKind {
    Xdp,
    TcIngress,
    TcEgress,
}

impl DispatcherKind {
    fn direction(self) -> Option<bpfman_model::TcDirection> {
        match self {
            Self::Xdp => None,
            Self::TcIngress => Some(bpfman_model::TcDirection::Ingress),
            Self::TcEgress => Some(bpfman_model::TcDirection::Egress),
        }
    }
}

impl DispatcherCommand {
    pub(crate) fn execute<S, K>(
        self,
        app: &bpfman_runtime::Bpfman<S, K>,
        cancellation: &bpfman_runtime::Cancellation,
    ) -> Result<(), crate::error::Error>
    where
        S: bpfman_store::DispatcherStore,
        S::Reader: bpfman_store::XdpDispatcherReader + bpfman_store::LinkReader,
    {
        match self {
            Self::Delete {
                kind,
                nsid,
                ifindex,
            } => {
                let key = match kind.direction() {
                    None => {
                        bpfman_model::DispatcherKey::Xdp(bpfman_model::XdpKey { nsid, ifindex })
                    }
                    Some(direction) => bpfman_model::DispatcherKey::Tc(bpfman_model::TcKey {
                        nsid,
                        ifindex,
                        direction,
                    }),
                };
                if !app.delete_dispatcher_with_cancellation(key, cancellation)? {
                    let name = match kind {
                        DispatcherKind::Xdp => "xdp",
                        DispatcherKind::TcIngress => "tc-ingress",
                        DispatcherKind::TcEgress => "tc-egress",
                    };
                    return Err(anyhow::anyhow!(
                        "dispatcher ({name}, {nsid}, {ifindex}): record not found"
                    )
                    .into());
                }
            }
            Self::Get {
                kind,
                nsid,
                ifindex,
                output,
            } => {
                if let Some(direction) = kind.direction() {
                    let snapshot = app.get_tc_dispatcher(bpfman_model::TcKey {
                        nsid,
                        ifindex,
                        direction,
                    })?;
                    crate::output::tc_dispatcher(&mut std::io::stdout().lock(), &snapshot, output)?;
                } else {
                    let snapshot =
                        app.get_xdp_dispatcher(bpfman_model::XdpKey { nsid, ifindex })?;
                    crate::output::dispatcher(&mut std::io::stdout().lock(), &snapshot, output)?;
                }
            }
            Self::List(ListArgs {
                nsid,
                ifindex,
                kind,
                output,
            }) => {
                let snapshots: Vec<_> = app
                    .list_xdp_dispatchers()?
                    .into_iter()
                    .filter(|s| {
                        s.members().first().is_some_and(|m| {
                            (nsid == 0 || m.details.key.nsid.get() == nsid)
                                && (ifindex == 0 || m.details.key.ifindex.get() == ifindex)
                                && kind.is_none_or(|kind| kind == DispatcherKind::Xdp)
                        })
                    })
                    .collect();
                let tc: Vec<_> = app
                    .list_tc_dispatchers()?
                    .into_iter()
                    .filter(|s| {
                        s.members().first().is_some_and(|m| {
                            (nsid == 0 || m.details.key.nsid.get() == nsid)
                                && (ifindex == 0 || m.details.key.ifindex.get() == ifindex)
                                && kind.is_none_or(|kind| {
                                    kind.direction() == Some(m.details.key.direction)
                                })
                        })
                    })
                    .collect();
                crate::output::dispatchers(&mut std::io::stdout().lock(), &snapshots, &tc, output)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;

    #[test]
    fn default_listing_and_explicit_commands_share_typed_flags() -> Result<(), clap::Error> {
        for args in [vec![], vec!["list"]] {
            let cli = Cli::try_parse_from(["bpfman", "dispatcher"].into_iter().chain(args))?;
            let Command::Dispatcher(args) = cli.command else {
                unreachable!()
            };
            assert!(matches!(
                args.into_command(),
                DispatcherCommand::List(ListArgs {
                    output: OutputFormat::Text,
                    kind: None,
                    nsid: 0,
                    ifindex: 0
                })
            ));
        }
        for prefix in [vec![], vec!["list"]] {
            let cli =
                Cli::try_parse_from(["bpfman", "dispatcher"].into_iter().chain(prefix).chain([
                    "--type",
                    "tc-egress",
                    "--nsid",
                    "123",
                    "--ifindex",
                    "9",
                    "-o",
                    "json",
                ]))?;
            let Command::Dispatcher(args) = cli.command else {
                unreachable!()
            };
            assert!(matches!(
                args.into_command(),
                DispatcherCommand::List(ListArgs {
                    output: OutputFormat::Json,
                    kind: Some(DispatcherKind::TcEgress),
                    nsid: 123,
                    ifindex: 9
                })
            ));
        }
        for (name, kind) in [
            ("xdp", DispatcherKind::Xdp),
            ("tc-ingress", DispatcherKind::TcIngress),
            ("tc-egress", DispatcherKind::TcEgress),
        ] {
            for flags in [vec![name, "123", "9"], vec!["-o", "text", name, "123", "9"]] {
                let cli =
                    Cli::try_parse_from(["bpfman", "dispatcher", "get"].into_iter().chain(flags))?;
                let Command::Dispatcher(args) = cli.command else {
                    unreachable!()
                };
                assert!(
                    matches!(args.into_command(), DispatcherCommand::Get { kind: actual, output: OutputFormat::Text, nsid, ifindex } if actual == kind && nsid.get() == 123 && ifindex.get() == 9)
                );
            }
        }
        Ok(())
    }
}
