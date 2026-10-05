use clap::Subcommand;
use std::num::{NonZeroU32, NonZeroU64};

#[derive(Subcommand)]
pub(crate) enum DispatcherCommand {
    /// Read the committed single-member XDP dispatcher snapshot.
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
        app: &bpfman_runtime::Bpfman<S>,
    ) -> Result<(), crate::error::Error>
    where
        S: bpfman_store::OpenStore,
        S::Reader: bpfman_store::XdpReader + bpfman_store::LinkReader,
    {
        let Self::Get {
            target:
                DispatcherTarget::Xdp {
                    nsid,
                    ifindex,
                    output: _,
                },
        } = self;
        let snapshot = app.get_xdp_dispatcher(bpfman_model::XdpKey { nsid, ifindex })?;
        crate::output::dispatcher(&mut std::io::stdout().lock(), &snapshot)?;
        Ok(())
    }
}
