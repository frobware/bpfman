use crate::{Execution, Handler, Status};
use bpfman_fs::VolumeRequest;
use bpfman_proto::csi as pb;
use bpfman_runtime::Cancellation;
use prost::Message;
use std::path::Path;

/// Narrow map-volume operations supplied by the daemon composition root.
pub trait Volumes: Send + Sync + 'static {
    /// Publish maps belonging to a reconciled program, retaining failed cleanup.
    fn publish(
        &self,
        program: &str,
        request: &VolumeRequest,
        cancellation: &Cancellation,
    ) -> Result<(), Status>;
    /// Remove a volume, including after reopening the daemon.
    fn unpublish(
        &self,
        volume: &str,
        target: &Path,
        cancellation: &Cancellation,
    ) -> Result<(), Status>;
}

/// Synchronous CSI Identity and Node service. Unsupported operations return
/// Unimplemented; no controller or staging capability is advertised.
pub struct CsiService<V> {
    node: String,
    volumes: V,
}

impl<V> CsiService<V> {
    /// Bind the hostname and map-volume capability without performing I/O.
    pub fn new(node: String, volumes: V) -> Self {
        Self { node, volumes }
    }
}

const METHODS: &[(&str, Execution)] = &[
    ("/csi.v1.Identity/GetPluginInfo", Execution::Read),
    ("/csi.v1.Identity/GetPluginCapabilities", Execution::Read),
    ("/csi.v1.Identity/Probe", Execution::Read),
    ("/csi.v1.Node/NodeGetInfo", Execution::Read),
    ("/csi.v1.Node/NodeGetCapabilities", Execution::Read),
    ("/csi.v1.Node/NodePublishVolume", Execution::Mutation),
    ("/csi.v1.Node/NodeUnpublishVolume", Execution::Teardown),
    ("/csi.v1.Node/NodeStageVolume", Execution::Read),
    ("/csi.v1.Node/NodeUnstageVolume", Execution::Read),
    ("/csi.v1.Node/NodeGetVolumeStats", Execution::Read),
    ("/csi.v1.Node/NodeExpandVolume", Execution::Read),
];

fn decode<M: Message + Default>(bytes: &[u8]) -> Result<M, Status> {
    M::decode(bytes).map_err(|_| Status::invalid_argument("invalid protobuf request"))
}

impl<V: Volumes> Handler for CsiService<V> {
    fn methods() -> &'static [(&'static str, Execution)] {
        METHODS
    }

    fn handle(
        &self,
        method: &str,
        bytes: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Vec<u8>, Status> {
        if cancellation.is_cancelled() {
            return Err(Status::cancelled("CSI call cancelled"));
        }
        Ok(match method {
            "/csi.v1.Identity/GetPluginInfo" => {
                decode::<pb::GetPluginInfoRequest>(bytes)?;
                pb::GetPluginInfoResponse {
                    name: "csi.bpfman.io".into(),
                    vendor_version: "0.1.0".into(),
                    ..Default::default()
                }
                .encode_to_vec()
            }
            "/csi.v1.Identity/GetPluginCapabilities" => {
                decode::<pb::GetPluginCapabilitiesRequest>(bytes)?;
                pb::GetPluginCapabilitiesResponse::default().encode_to_vec()
            }
            "/csi.v1.Identity/Probe" => {
                decode::<pb::ProbeRequest>(bytes)?;
                pb::ProbeResponse { ready: Some(true) }.encode_to_vec()
            }
            "/csi.v1.Node/NodeGetInfo" => {
                decode::<pb::NodeGetInfoRequest>(bytes)?;
                pb::NodeGetInfoResponse {
                    node_id: self.node.clone(),
                    ..Default::default()
                }
                .encode_to_vec()
            }
            "/csi.v1.Node/NodeGetCapabilities" => {
                decode::<pb::NodeGetCapabilitiesRequest>(bytes)?;
                pb::NodeGetCapabilitiesResponse {
                    capabilities: vec![pb::NodeServiceCapability {
                        r#type: Some(pb::node_service_capability::Type::Rpc(
                            pb::node_service_capability::Rpc { r#type: 6 },
                        )),
                    }],
                }
                .encode_to_vec()
            }
            "/csi.v1.Node/NodePublishVolume" => {
                let req: pb::NodePublishVolumeRequest = decode(bytes)?;
                let vc = req
                    .volume_capability
                    .ok_or_else(|| Status::invalid_argument("volume capability is required"))?;
                let mount = match vc.access_type {
                    Some(pb::volume_capability::AccessType::Mount(m)) => m,
                    _ => {
                        return Err(Status::invalid_argument(
                            "volume capability must be a mount",
                        ));
                    }
                };
                if !mount.fs_type.is_empty() || !mount.mount_flags.is_empty() {
                    return Err(Status::invalid_argument(
                        "fsType and mount flags are not supported",
                    ));
                }
                if !vc.access_mode.is_some_and(|m| matches!(m.mode, 1 | 6)) {
                    return Err(Status::invalid_argument("unsupported access mode"));
                }
                let program = req
                    .volume_context
                    .get("csi.bpfman.io/program")
                    .filter(|p| !p.is_empty())
                    .ok_or_else(|| Status::invalid_argument("csi.bpfman.io/program is required"))?;
                let maps = req
                    .volume_context
                    .get("csi.bpfman.io/maps")
                    .map(String::as_str)
                    .unwrap_or_default();
                // Match Go's Atoi and chown conversion: ignore malformed/negative
                // values; positive integers are converted to the syscall's gid_t.
                let group = mount
                    .volume_mount_group
                    .parse::<i64>()
                    .ok()
                    .filter(|n| *n >= 0)
                    .map(|n| n as u32);
                let request = VolumeRequest::new(
                    &req.volume_id,
                    Path::new(&req.target_path),
                    maps,
                    req.readonly,
                    group,
                )
                .map_err(|e| Status::invalid_argument(e.to_string()))?;
                self.volumes.publish(program, &request, cancellation)?;
                pb::NodePublishVolumeResponse::default().encode_to_vec()
            }
            "/csi.v1.Node/NodeUnpublishVolume" => {
                let req: pb::NodeUnpublishVolumeRequest = decode(bytes)?;
                VolumeRequest::validate_target(&req.volume_id, Path::new(&req.target_path))
                    .map_err(|e| Status::invalid_argument(e.to_string()))?;
                self.volumes.unpublish(
                    &req.volume_id,
                    Path::new(&req.target_path),
                    cancellation,
                )?;
                pb::NodeUnpublishVolumeResponse::default().encode_to_vec()
            }
            _ => return Err(Status::unimplemented("CSI operation not supported")),
        })
    }
}
