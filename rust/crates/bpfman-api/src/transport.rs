use crate::{Images, Kernel, Service};
use bpfman_runtime::Cancellation;
use grpcuds::{Backpressure, ServerWriter, Status};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

/// Bounded synchronous execution resources, independent of writer serialization.
#[derive(Clone, Copy, Debug)]
pub struct ServerOptions {
    /// Operation threads; reads can proceed while writers wait on the flock.
    pub workers: usize,
    /// Dedicated read threads, independent of mutators waiting on the flock.
    pub readers: usize,
    /// Queued requests per execution class; excess calls receive ResourceExhausted.
    pub pending: usize,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            workers: 8,
            readers: 2,
            pending: 128,
        }
    }
}

struct Call {
    writer: ServerWriter,
    cancellation: Cancellation,
}

struct Job {
    method: &'static str,
    bytes: Vec<u8>,
    call: Arc<Call>,
}

/// Bound Unix-socket server. The composition root owns its shutdown signal and
/// socket policy. Serving joins every operation worker before returning.
pub struct Server<S, K, I>
where
    S: bpfman_store::UnloadStore
        + bpfman_store::LinkStore
        + bpfman_store::XdpStore
        + bpfman_store::TcStore,
    K: Kernel,
{
    io: grpcuds::Server,
    jobs: Mutex<mpsc::Receiver<Job>>,
    teardown: Mutex<mpsc::Receiver<Job>>,
    reads: Mutex<mpsc::Receiver<Job>>,
    service: Arc<Service<S, K, I>>,
    active: Arc<Mutex<Vec<Arc<Call>>>>,
    options: ServerOptions,
}

const METHODS: &[&str] = &[
    "/bpfman.v1.Bpfman/Load",
    "/bpfman.v1.Bpfman/Unload",
    "/bpfman.v1.Bpfman/Attach",
    "/bpfman.v1.Bpfman/Detach",
    "/bpfman.v1.Bpfman/List",
    "/bpfman.v1.Bpfman/Get",
    "/bpfman.v1.Bpfman/PullBytecode",
];

impl<S, K, I> Server<S, K, I>
where
    S: bpfman_store::UnloadStore
        + bpfman_store::CommitLoad
        + bpfman_store::LinkStore
        + bpfman_store::XdpReplacementStore
        + bpfman_store::TcStore
        + Send
        + Sync
        + 'static,
    S::Reader: bpfman_store::LinkReader,
    K: Kernel + Send + Sync + 'static,
    K::PreparedXdp: Send,
    I: Images + Send + Sync + 'static,
{
    /// Bind a fresh socket. The synchronous transport has no async dependency.
    /// A full worker queue rejects immediately, leaving the I/O loop available.
    pub fn bind(
        path: &Path,
        service: Arc<Service<S, K, I>>,
        options: ServerOptions,
    ) -> Result<Self, std::io::Error> {
        if options.workers == 0 || options.readers == 0 || options.pending == 0 {
            return Err(std::io::Error::other(
                "gRPC worker and queue limits must be positive",
            ));
        }
        match std::fs::symlink_metadata(path) {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "gRPC socket path already exists",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let (sender, receiver) = mpsc::sync_channel::<Job>(options.pending);
        let (read_sender, read_receiver) = mpsc::sync_channel::<Job>(options.pending);
        let (teardown_sender, teardown_receiver) = mpsc::sync_channel::<Job>(options.pending);
        let active = Arc::new(Mutex::new(Vec::<Arc<Call>>::new()));
        let mut builder = grpcuds::Server::builder().bind(path);
        for &method in METHODS {
            let sender = if matches!(method, "/bpfman.v1.Bpfman/List" | "/bpfman.v1.Bpfman/Get") {
                read_sender.clone()
            } else if matches!(
                method,
                "/bpfman.v1.Bpfman/Detach" | "/bpfman.v1.Bpfman/Unload"
            ) {
                teardown_sender.clone()
            } else {
                sender.clone()
            };
            let active = active.clone();
            // Deferred single-response completion lets ordinary operation
            // threads serve unary calls without blocking the transport loop.
            builder = builder.add_server_streaming(method, move |bytes, writer| {
                if bytes.len() > 4 * 1024 * 1024 {
                    return Status::resource_exhausted("request exceeds 4 MiB");
                }
                let call = Arc::new(Call {
                    writer: writer.clone(),
                    cancellation: Cancellation::new(),
                });
                let job = Job {
                    method,
                    bytes: bytes.to_vec(),
                    call: call.clone(),
                };
                match sender.try_send(job) {
                    Ok(()) => {
                        active
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(call);
                        Status::ok()
                    }
                    Err(mpsc::TrySendError::Full(_)) => {
                        Status::resource_exhausted("gRPC worker queue is full")
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        Status::unavailable("gRPC server is stopping")
                    }
                }
            });
        }
        let io = builder.build().map_err(std::io::Error::other)?;
        Ok(Self {
            io,
            jobs: Mutex::new(receiver),
            teardown: Mutex::new(teardown_receiver),
            reads: Mutex::new(read_receiver),
            service,
            active,
            options,
        })
    }

    /// Drive HTTP/2 and join operation threads, including compensation after a
    /// cancelled call. Shutdown never abandons a writer or its owned receipts.
    pub fn serve(self, shutdown: &AtomicBool) -> Result<(), std::io::Error> {
        let stopping = AtomicBool::new(false);
        let Self {
            io,
            jobs,
            teardown,
            reads,
            service,
            active,
            options,
        } = self;
        std::thread::scope(|scope| {
            let mut threads = Vec::new();
            for index in 0..options.workers.saturating_add(options.readers) {
                let jobs = if index < options.workers {
                    &jobs
                } else {
                    &reads
                };
                let teardown = if index < options.workers {
                    Some(&teardown)
                } else {
                    None
                };
                let service = &service;
                let stopping = &stopping;
                let handle = std::thread::Builder::new()
                    .name(format!("grpc-worker-{index}"))
                    .spawn_scoped(scope, move || {
                        loop {
                            // Resource release must progress ahead of new acquisitions.
                            // FIFO admission alone can exhaust per-hook kernel limits
                            // while the corresponding detaches sit behind that burst.
                            let release = teardown.and_then(|queue| {
                                queue
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                                    .try_recv()
                                    .ok()
                            });
                            let received = release.map_or_else(
                                || {
                                    jobs.lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                                        .recv_timeout(Duration::from_millis(20))
                                },
                                Ok,
                            );
                            let job = match received {
                                Ok(job) => job,
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                                Err(mpsc::RecvTimeoutError::Timeout)
                                    if stopping.load(Ordering::Acquire) =>
                                {
                                    break;
                                }
                                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            };
                            if stopping.load(Ordering::Acquire) {
                                job.call.cancellation.cancel();
                            }
                            let outcome =
                                service.handle(job.method, &job.bytes, &job.call.cancellation);
                            let status = match outcome {
                                Ok(bytes) => {
                                    let _ = job.call.writer.write_owned(bytes);
                                    Status::ok()
                                }
                                Err(status) => status,
                            };
                            let _ = job.call.writer.finish(status);
                        }
                    });
                match handle {
                    Ok(handle) => threads.push(handle),
                    Err(error) => {
                        stopping.store(true, Ordering::Release);
                        return Err(error);
                    }
                }
            }
            let monitor = std::thread::Builder::new()
                .name("grpc-cancellation".into())
                .spawn_scoped(scope, || {
                    while !stopping.load(Ordering::Acquire) {
                        {
                            let mut calls = active
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            calls.retain(|call| {
                                if shutdown.load(Ordering::Acquire)
                                    || call.writer.is_cancelled()
                                    || call.writer.time_remaining().is_some_and(|d| d.is_zero())
                                {
                                    call.cancellation.cancel();
                                }
                                if call.writer.is_finished() {
                                    return false;
                                }
                                // grpcuds 0.1 reports a vanished stream when a queued
                                // operation bounces. Reapplying its default queue
                                // policy probes liveness without sending a message.
                                let _ = call.writer.set_backpressure(Backpressure::Unbounded);
                                true
                            });
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    for call in active
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .iter()
                    {
                        call.cancellation.cancel();
                    }
                });
            let monitor = match monitor {
                Ok(handle) => handle,
                Err(error) => {
                    stopping.store(true, Ordering::Release);
                    return Err(error);
                }
            };
            let outcome = io.serve(shutdown).map_err(std::io::Error::other);
            stopping.store(true, Ordering::Release);
            monitor
                .join()
                .map_err(|_| std::io::Error::other("gRPC cancellation worker panicked"))?;
            for thread in threads {
                thread
                    .join()
                    .map_err(|_| std::io::Error::other("gRPC operation worker panicked"))?;
            }
            outcome
        })
    }
}
