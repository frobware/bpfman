//! Synchronous gRPC frontend over generic runtime and persistence capabilities.

mod convert;
mod service;
mod transport;

pub use grpcuds::{Status, StatusCode};
pub use service::{Images, Kernel, Recovery, Service};
pub use transport::{Execution, Handler, RpcServer, Server, ServerOptions};
mod csi;
pub use csi::{CsiService, Volumes};
