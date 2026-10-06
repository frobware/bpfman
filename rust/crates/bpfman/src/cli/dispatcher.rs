use clap::Subcommand;
use std::num::{NonZeroU32, NonZeroU64};

#[derive(Subcommand)]
pub(crate) enum DispatcherCommand {
    /// List committed XDP dispatchers (JSON only).
    List {
        #[arg(long, value_parser = ["xdp"])]
        r#type: Option<String>,
        #[arg(long, default_value_t = 0)]
        nsid: u64,
        #[arg(long, default_value_t = 0)]
        ifindex: u32,
        #[arg(short, long, default_value = "json", value_parser = ["json"])]
        output: String,
    },
    /// Read the committed complete XDP dispatcher snapshot.
    Get {
        #[command(subcommand)]
        target: DispatcherTarget,
    },
}

#[derive(Subcommand)]
pub(crate) enum DispatcherTarget {
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
            Self::List { nsid, ifindex, .. } => {
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
                crate::output::dispatchers(&mut std::io::stdout().lock(), &snapshots)?;
            }
        }
        Ok(())
    }
}
