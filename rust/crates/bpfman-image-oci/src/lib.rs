//! Synchronous OCI image transport and validated bytecode caching.
//!
//! Collection policy belongs to the embedding application. Cap the `ureq` and
//! `ureq_proto` logging targets below TRACE: their wire diagnostics bypass header
//! redaction and can contain credentials, tokens, upload URLs and bytecode. The
//! bpfman CLI enforces this ceiling for stderr and timeline output, independently
//! of caller-supplied logging directives. This adapter installs no subscriber.
mod credentials;
mod error;
mod publish;
mod pull;
mod reference;
mod registry;
mod signing;
mod transport;
mod trust;
mod upload;
mod verification;
pub use credentials::Credentials;
pub use error::Error;
pub use publish::{Bytecode, Publication, publish};
pub use pull::{Image, pull};
pub use reference::Reference;
pub use registry::inspect;
pub use signing::{SigningPolicy, TrustedIdentity};
pub use verification::{VerificationStatus, verify};

#[cfg(test)]
#[path = "../../../tests/support/oci.rs"]
mod fixture;
#[cfg(test)]
#[path = "../../../tests/support/signed_image.rs"]
mod signed_image;
