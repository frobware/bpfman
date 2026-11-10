//! Generated CSI messages and optional bpfman management messages.
//! Domain conversion belongs to the API frontend, never to this crate.

/// Existing bpfman.v1 protobuf wire vocabulary.
#[cfg(feature = "management-api")]
#[allow(missing_docs)]
pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/bpfman.v1.rs"));
}

/// CSI node-plugin message vocabulary from the shared specification.
#[allow(missing_docs)]
pub mod csi {
    include!(concat!(env!("OUT_DIR"), "/csi.v1.rs"));
}
