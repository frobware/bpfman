//! Permanent CSI daemon; independent of the bpfman management API.

use anyhow::Context;
use bpfman_api::{CsiService, RpcServer, ServerOptions, Status, Volumes};
use bpfman_fs::{RuntimeDirectory, RuntimeLayout, SocketDirectory};
use bpfman_runtime::{Cancellation, MapVolumes, VolumeError, VolumeErrorKind};
use std::sync::Arc;

pub(super) fn run(
    cli: &crate::cli::Cli,
    cancellation: &Cancellation,
) -> Result<(), crate::error::Error> {
    let app = crate::daemon::application(cli, cancellation)?;
    run_with_application(cli, app, cancellation)
}

pub(super) fn run_with_application(
    cli: &crate::cli::Cli,
    app: crate::daemon::SharedApplication,
    cancellation: &Cancellation,
) -> Result<(), crate::error::Error> {
    RuntimeDirectory::open_or_create(
        RuntimeLayout::try_from(cli.layout.csi_filesystem_path()).map_err(anyhow::Error::new)?,
    )?;
    let socket = cli.layout.csi_socket_path();
    let sockets = SocketDirectory::open(&socket)?;
    let node = std::fs::read_to_string("/proc/sys/kernel/hostname")?
        .trim_end_matches('\n')
        .to_owned();
    let volumes = Arc::new(MapVolumes::new(app));
    let service = Arc::new(CsiService::new(node, VolumePolicy(volumes.clone())));
    let (outcome, cleanup) = sockets.with_socket(
        bpfman_lock::AcquireOptions {
            timeout: cli.lock_timeout,
            cancelled: Some(cancellation.flag()),
        },
        |binding| -> Result<(), crate::error::Error> {
            let server = RpcServer::bind(binding.path(), service, ServerOptions::default())?;
            binding.publish()?;
            tracing::info!(socket = %socket.display(), "CSI server listening");
            server
                .serve(cancellation.flag())
                .context("serve CSI")
                .map_err(Into::into)
        },
    )?;
    let pending = volumes.take_recovery();
    let outcome = match (outcome, cleanup) {
        (Err(error), Err(cleanup)) => {
            Err(anyhow::anyhow!("{error:#}; CSI socket cleanup: {cleanup:#}").into())
        }
        (Err(error), _) => Err(error),
        (_, Err(error)) => Err(anyhow::Error::from(error).into()),
        _ => Ok(()),
    };
    if pending.is_empty() {
        return outcome;
    }
    tracing::error!(
        count = pending.len(),
        "CSI shutdown retains unresolved cleanup"
    );
    let message = format!(
        "CSI stopped with {} unresolved cleanup receipts",
        pending.len()
    );
    match outcome {
        Err(error) => Err(anyhow::anyhow!("{error:#}; {message}").into()),
        Ok(()) => Err(anyhow::anyhow!(message).into()),
    }
}

struct VolumePolicy<S: bpfman_store::OpenStore, K>(Arc<MapVolumes<S, K>>);

fn volume_status(error: VolumeError) -> Status {
    let kind = error.kind();
    let message = format!("{:#}", anyhow::Error::new(error));
    match kind {
        VolumeErrorKind::Cancelled => Status::cancelled(message),
        VolumeErrorKind::NotFound => Status::not_found(message),
        VolumeErrorKind::Busy => Status::aborted(message),
        VolumeErrorKind::AlreadyExists => Status::already_exists(message),
        VolumeErrorKind::FailedPrecondition => Status::failed_precondition(message),
        VolumeErrorKind::Internal => Status::internal(message),
    }
}

impl<S, K> Volumes for VolumePolicy<S, K>
where
    S: bpfman_store::OpenStore + Send + Sync + 'static,
    K: bpfman_kernel::ProgramObservations + bpfman_fs::MapReopening + Send + Sync + 'static,
{
    fn publish(
        &self,
        program: &str,
        request: &bpfman_fs::VolumeRequest,
        cancellation: &Cancellation,
    ) -> Result<(), Status> {
        self.0
            .publish(program, request, cancellation)
            .map_err(volume_status)
    }
    fn unpublish(
        &self,
        volume: &str,
        target: &std::path::Path,
        cancellation: &Cancellation,
    ) -> Result<(), Status> {
        self.0
            .unpublish(volume, target, cancellation)
            .map_err(volume_status)
    }
}
