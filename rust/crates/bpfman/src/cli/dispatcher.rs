use clap::Subcommand;
use std::num::{NonZeroU32, NonZeroU64};

#[derive(Subcommand)]
pub(crate) enum DispatcherCommand {
    /// List committed XDP and TC dispatchers (JSON only).
    List {
        #[arg(long, value_parser = ["xdp", "tc-ingress", "tc-egress"])]
        r#type: Option<String>,
        #[arg(long, default_value_t = 0)]
        nsid: u64,
        #[arg(long, default_value_t = 0)]
        ifindex: u32,
        #[arg(short, long, default_value = "json", value_parser = ["json"])]
        output: String,
    },
    /// Read the committed complete dispatcher snapshot.
    Get {
        #[command(subcommand)]
        target: DispatcherTarget,
    },
}

#[derive(Subcommand)]
pub(crate) enum DispatcherTarget {
    /// Identify a legacy TC ingress attach point.
    TcIngress {
        nsid: NonZeroU64,
        ifindex: NonZeroU32,
        #[arg(short, long, default_value = "json", value_parser = ["json"])]
        output: String,
    },
    /// Identify a legacy TC egress attach point.
    TcEgress {
        nsid: NonZeroU64,
        ifindex: NonZeroU32,
        #[arg(short, long, default_value = "json", value_parser = ["json"])]
        output: String,
    },
    /// Identify an XDP attach point by namespace inode and interface index.
    Xdp {
        nsid: NonZeroU64,
        ifindex: NonZeroU32,
        #[arg(short, long, default_value = "json", value_parser = ["json"])]
        output: String,
    },
}

impl DispatcherCommand {
    pub(crate) fn execute<S>(
        self,
        app: &bpfman_runtime::Bpfman<S, bpfman_kernel_aya::Kernel>,
    ) -> Result<(), crate::error::Error>
    where
        S: bpfman_store::OpenStore,
        S::Reader: bpfman_store::XdpDispatcherReader + bpfman_store::LinkReader,
    {
        match self {
            Self::Get {
                target:
                    DispatcherTarget::Xdp {
                        nsid,
                        ifindex,
                        output: _,
                    },
            } => {
                let snapshot = app.get_xdp_dispatcher(bpfman_model::XdpKey { nsid, ifindex })?;
                crate::output::dispatcher(&mut std::io::stdout().lock(), &snapshot)?;
            }
            Self::Get {
                target: DispatcherTarget::TcIngress { nsid, ifindex, .. },
            } => {
                let snapshot = app.get_tc_dispatcher(bpfman_model::TcKey {
                    nsid,
                    ifindex,
                    direction: bpfman_model::TcDirection::Ingress,
                })?;
                crate::output::tc_dispatcher(&mut std::io::stdout().lock(), &snapshot)?;
            }
            Self::Get {
                target: DispatcherTarget::TcEgress { nsid, ifindex, .. },
            } => {
                let snapshot = app.get_tc_dispatcher(bpfman_model::TcKey {
                    nsid,
                    ifindex,
                    direction: bpfman_model::TcDirection::Egress,
                })?;
                crate::output::tc_dispatcher(&mut std::io::stdout().lock(), &snapshot)?;
            }
            Self::List {
                nsid,
                ifindex,
                r#type,
                ..
            } => {
                let snapshots: Vec<_> = app
                    .list_xdp_dispatchers()?
                    .into_iter()
                    .filter(|s| {
                        s.members().first().is_some_and(|m| {
                            (nsid == 0 || m.details.key.nsid.get() == nsid)
                                && (ifindex == 0 || m.details.key.ifindex.get() == ifindex)
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
                                && r#type.as_deref().is_none_or(|kind| {
                                    kind == m.details.key.direction.dispatcher_type()
                                })
                        })
                    })
                    .collect();
                let snapshots = if matches!(r#type.as_deref(), Some("tc-ingress" | "tc-egress")) {
                    Vec::new()
                } else {
                    snapshots
                };
                let tc = if r#type.as_deref() == Some("xdp") {
                    Vec::new()
                } else {
                    tc
                };
                crate::output::dispatchers(&mut std::io::stdout().lock(), &snapshots, &tc)?;
            }
        }
        Ok(())
    }
}
