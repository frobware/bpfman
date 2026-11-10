use super::Execution;
use grpcuds::{Status, StatusCode};
use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tracing::{Dispatch, Span};

// Shared by management and CSI listeners, unique for this process lifetime.
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

pub(super) struct Request {
    pub(super) span: Span,
    pub(super) dispatch: Dispatch,
    admitted: Instant,
    id: u64,
    method: &'static str,
    execution: Execution,
    cancelled: AtomicBool,
}

impl Request {
    pub(super) fn new(method: &'static str, execution: Execution) -> Self {
        let admitted = Instant::now();
        let id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
        let class = match execution {
            Execution::Read => "read",
            Execution::Mutation => "mutation",
            Execution::Teardown => "teardown",
        };
        macro_rules! span {
            ($level:expr) => {
                tracing::span!(parent: None, $level, "rpc.request",
                    request_id = id, rpc_method = method, execution = class,
                    program_id = tracing::field::Empty,
                    link_id = tracing::field::Empty,
                    volume_id = tracing::field::Empty)
            };
        }
        let span = match execution {
            Execution::Read => span!(tracing::Level::DEBUG),
            _ => span!(tracing::Level::INFO),
        };
        Self {
            span,
            dispatch: tracing::dispatcher::get_default(Clone::clone),
            admitted,
            id,
            method,
            execution,
            cancelled: AtomicBool::new(false),
        }
    }

    pub(super) fn start(&self) -> Duration {
        let queued = self.admitted.elapsed();
        tracing::debug!(
            queue_ms = queued.as_secs_f64() * 1000.0,
            "RPC execution started"
        );
        queued
    }

    pub(super) fn cancel(&self, reason: &'static str) {
        if !self.cancelled.swap(true, Ordering::Relaxed) {
            tracing::debug!(
                request_id = self.id,
                rpc_method = self.method,
                reason,
                "RPC cancellation requested"
            );
        }
    }

    pub(super) fn reject(&self, status: Status, reason: &'static str) -> Status {
        tracing::debug!(
            request_id = self.id,
            rpc_method = self.method,
            grpc_status = ?status.code(),
            reason,
            total_ms = self.admitted.elapsed().as_secs_f64() * 1000.0,
            "RPC rejected"
        );
        status
    }

    pub(super) fn finish(&self, code: StatusCode, queued: Duration, response_queued: bool) {
        let total = self.admitted.elapsed();
        // Never render Status's free-form message: it can contain caller input
        // or an adapter's credential-bearing error. Codes are stable diagnostics.
        macro_rules! complete {
            ($level:expr) => {
                tracing::event!($level,
                    request_id = self.id,
                    rpc_method = self.method,
                    grpc_status = ?code,
                    queue_ms = queued.as_secs_f64() * 1000.0,
                    execution_ms = total.saturating_sub(queued).as_secs_f64() * 1000.0,
                    total_ms = total.as_secs_f64() * 1000.0,
                    response_queued,
                    cancellation_requested = self.cancelled.load(Ordering::Relaxed),
                    "RPC completed")
            };
        }
        match code {
            StatusCode::Internal | StatusCode::Unknown | StatusCode::DataLoss => {
                complete!(tracing::Level::ERROR);
            }
            StatusCode::Ok if !matches!(self.execution, Execution::Read) => {
                complete!(tracing::Level::INFO);
            }
            _ => complete!(tracing::Level::DEBUG),
        }
    }
}
