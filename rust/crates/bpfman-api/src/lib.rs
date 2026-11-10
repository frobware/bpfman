//! CSI and optional bpfman compatibility adapters over a shared gRPC transport.

#[cfg(feature = "management-api")]
mod convert;
mod csi;
#[cfg(feature = "management-api")]
mod service;
mod transport;

pub use csi::{CsiService, Volumes};
pub use grpcuds::{Status, StatusCode};
#[cfg(feature = "management-api")]
pub use service::{Images, Kernel, Recovery, Server, Service};
pub use transport::{Execution, Handler, RpcServer, ServerOptions};
