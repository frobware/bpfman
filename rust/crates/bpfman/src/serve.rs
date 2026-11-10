//! Long-lived daemon composition; transport and runtime retain their own contracts.
use anyhow::Context;
use bpfman_api::{Images, RpcServer, Server, ServerOptions, Service, Status};
use bpfman_fs::{RuntimeDirectory, RuntimeLayout, SocketDirectory};
use bpfman_runtime::Cancellation;
use clap::builder::TypedValueParser;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

#[derive(clap::Args)]
pub(crate) struct Args {
    /// Unix socket path for gRPC server.
    #[arg(long, env = "BPFMAN_SOCKET_PATH", default_value = "/run/bpfman-sock/bpfman.sock", value_parser = clap::builder::PathBufValueParser::new().try_map(absolute_socket))]
    socket_path: PathBuf,
    /// Enable CSI driver support.
    #[arg(long, action = clap::ArgAction::Set, value_parser = crate::cli::number::boolean, num_args = 0..=1, require_equals = true, default_missing_value = "true", default_value_t = false)]
    csi_support: bool,
}

fn absolute_socket(path: PathBuf) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Ok(path);
    }
    if !path.is_absolute()
        || path.file_name().is_none()
        || path.parent() == Some(std::path::Path::new("/"))
    {
        return Err("socket path requires an absolute path with a non-root parent".into());
    }
    Ok(path)
}

struct ImagePolicy {
    cache: RuntimeLayout,
    signing: bpfman_image_oci::SigningPolicy,
    timeout: Duration,
    // Image admission may own unresolved cache/trust cleanup, independently of
    // program lifecycle receipts. Keep those errors alive until daemon shutdown.
    recovery: Arc<Mutex<Vec<bpfman_image_oci::Error>>>,
}

impl Images for ImagePolicy {
    fn pull(
        &self,
        image: &bpfman_proto::v1::BytecodeImage,
        cancellation: &Cancellation,
        validate: &dyn Fn(&[u8]) -> Result<(), bpfman_kernel::Error>,
    ) -> Result<(Vec<u8>, bpfman_model::ProgramSource), Status> {
        let reference = bpfman_image_oci::Reference::parse(&image.url)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let policy = match image.image_pull_policy {
            0 => bpfman_model::ImagePullPolicy::Always,
            1 => bpfman_model::ImagePullPolicy::IfNotPresent,
            2 => bpfman_model::ImagePullPolicy::Never,
            _ => return Err(Status::invalid_argument("invalid image pull policy")),
        };
        let auth = match (image.username.as_deref(), image.password.as_deref()) {
            (Some(u), Some(p)) if !u.is_empty() && !p.is_empty() => {
                bpfman_image_oci::Credentials::new(u, p)
                    .map_err(|e| Status::invalid_argument(e.to_string()))?
            }
            _ => bpfman_image_oci::Credentials::from_environment(&reference),
        };
        let pulled = bpfman_image_oci::pull(
            &reference,
            Some(&auth),
            &self.signing,
            policy,
            &self.cache,
            self.timeout,
            cancellation.flag(),
            |bytes| {
                bpfman_kernel_aya::inspect_bytecode(bytes, cfg!(target_endian = "little"))?;
                validate(bytes)
            },
        )
        .map_err(|e| {
            let status = if e.is_cancelled() {
                Status::cancelled("image preparation cancelled")
            } else {
                Status::internal(format!("{e:#}"))
            };
            if e.has_pending_cleanup() {
                self.recovery
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(e);
            }
            status
        })?;
        let source = bpfman_model::ProgramSource::Image {
            url: image.url.clone(),
            digest: pulled.digest().into(),
            pull_policy: policy,
        };
        Ok((pulled.bytes().to_vec(), source))
    }
}

pub(super) fn run(
    cli: &crate::cli::Cli,
    args: &Args,
    cancellation: &Cancellation,
) -> Result<(), crate::error::Error> {
    let config = Arc::new(crate::cli::config::Config::load(cli.config.as_deref())?);
    let signing = config.trust(
        config.signing()?,
        cli.trust_cache.as_ref(),
        cli.lock_timeout,
    )?;
    let socket_path = if args.socket_path.as_os_str().is_empty() {
        cli.layout.socket_path()
    } else {
        args.socket_path.clone()
    };
    let sockets = SocketDirectory::open(&socket_path)?;
    let image_recovery = Arc::new(Mutex::new(Vec::new()));
    let (outcome, cleanup) = sockets.with_socket(
        bpfman_lock::AcquireOptions {
            timeout: cli.lock_timeout,
            cancelled: Some(cancellation.flag()),
        },
        |binding| -> Result<(), crate::error::Error> {
            let app = crate::daemon::application(cli, cancellation)?;
            // Image preparation belongs only to the temporary management API.
            RuntimeDirectory::open_or_create(cli.image_cache.clone())?;
            let service = Arc::new(
                Service::from_shared(
                    app.clone(),
                    bpfman_kernel_aya::Kernel::default(),
                    ImagePolicy {
                        cache: cli.image_cache.clone(),
                        signing,
                        timeout: cli.lock_timeout,
                        recovery: image_recovery.clone(),
                    },
                    Default::default(),
                )
                .with_xdp_modes(move |interface| {
                    config
                        .xdp_mode(interface)
                        .map_err(|e| Status::invalid_argument(format!("{e:#}")))
                }),
            );
            let server = Server::bind(
                binding.path(),
                service.clone(),
                ServerOptions {
                    pending: 512,
                    ..ServerOptions::default()
                },
            )?;
            binding.publish()?;
            tracing::info!(socket = %socket_path.display(), "bpfman gRPC server listening");
            let outcome = if args.csi_support {
                serve_with_csi(cli, app, server, cancellation)
            } else {
                server
                    .serve(cancellation.flag())
                    .context("serve gRPC")
                    .map_err(Into::into)
            };
            let pending = service.take_recovery();
            if !pending.is_empty() {
                tracing::error!(
                    count = pending.len(),
                    "gRPC shutdown retains unresolved lifecycle cleanup"
                );
            }
            let image_pending = image_recovery
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len();
            if image_pending != 0 {
                tracing::error!(
                    count = image_pending,
                    "gRPC shutdown retains unresolved image cleanup"
                );
            }
            outcome?;
            if !pending.is_empty() || image_pending != 0 {
                return Err(anyhow::anyhow!(
                    "daemon stopped with {} unresolved lifecycle and {} image cleanup receipts",
                    pending.len(),
                    image_pending
                )
                .into());
            }
            Ok(())
        },
    )?;
    match (outcome, cleanup) {
        (Err(error), Err(cleanup)) => {
            Err(anyhow::anyhow!("{error:#}; socket cleanup: {cleanup:#}").into())
        }
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(anyhow::Error::from(error).into()),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn serve_with_csi<H: bpfman_api::Handler>(
    cli: &crate::cli::Cli,
    app: crate::daemon::SharedApplication,
    server: RpcServer<H>,
    cancellation: &Cancellation,
) -> Result<(), crate::error::Error> {
    std::thread::scope(|scope| -> Result<(), crate::error::Error> {
        let peer = std::thread::Builder::new()
            .name("csi-server".into())
            .spawn_scoped(scope, || {
                let outcome = crate::csi::run_with_application(cli, app, cancellation);
                cancellation.cancel();
                outcome
            })?;
        let outcome = server.serve(cancellation.flag()).context("serve gRPC");
        cancellation.cancel();
        let csi_outcome = peer
            .join()
            .map_err(|_| anyhow::anyhow!("CSI server worker panicked"))?;
        match (outcome, csi_outcome) {
            (Err(error), Err(csi)) => Err(anyhow::anyhow!("{error:#}; CSI: {csi:#}").into()),
            (Err(error), _) => Err(error.into()),
            (_, Err(error)) => Err(error),
            _ => Ok(()),
        }
    })
}
