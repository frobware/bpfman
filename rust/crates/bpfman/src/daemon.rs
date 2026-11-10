//! Application construction shared by independent daemon frontends.

use bpfman_runtime::{ActiveStore, Bpfman, Cancellation};
use std::{sync::Arc, time::Duration};

type Application = Bpfman<bpfman_store_sqlite::Backend, bpfman_kernel_aya::Kernel>;

pub(super) fn application(
    cli: &crate::cli::Cli,
    cancellation: &Cancellation,
) -> Result<Arc<Application>, crate::error::Error> {
    let store = ActiveStore::open_with_cancellation(
        bpfman_store_sqlite::Backend,
        &cli.layout,
        cli.lock_timeout,
        cancellation,
    )?;
    let launcher = crate::ns_helper::Launcher::new(std::env::current_exe()?, cancellation.clone());
    let kernel = bpfman_kernel_aya::Kernel::with_cancellable_uprobe_target_opener(
        move |writer, target, cancelled| launcher.open_with_cancellation(writer, target, cancelled),
    );
    Ok(Arc::new(Bpfman::new(store, kernel, Duration::ZERO)))
}

pub(super) type SharedApplication = Arc<Application>;
