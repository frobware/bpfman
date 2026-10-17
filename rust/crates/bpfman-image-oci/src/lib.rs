//! Synchronous public OCI image transport and validated bytecode caching.
mod error;
mod pull;
mod reference;
mod registry;
pub use error::Error;
pub use pull::{Image, pull};
pub use reference::Reference;
pub use registry::inspect;
