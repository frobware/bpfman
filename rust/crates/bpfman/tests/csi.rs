//! CSI wire validation is independent of the kernel, store and mount interpreter.
#![allow(clippy::expect_used)]
use bpfman_api::{CsiService, Handler, Status, StatusCode, Volumes};
use bpfman_fs::VolumeRequest;
use bpfman_proto::csi as pb;
use bpfman_runtime::Cancellation;
use prost::Message;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
struct Backend(Calls);
type Calls = Arc<Mutex<Vec<(String, Vec<String>)>>>;
impl Volumes for Backend {
    fn publish(
        &self,
        program: &str,
        request: &VolumeRequest,
        _: &Cancellation,
    ) -> Result<(), Status> {
        self.0
            .lock()
            .expect("calls")
            .push((program.into(), request.maps().to_vec()));
        Ok(())
    }
    fn unpublish(&self, _: &str, _: &Path, _: &Cancellation) -> Result<(), Status> {
        Ok(())
    }
}

fn request() -> pb::NodePublishVolumeRequest {
    pb::NodePublishVolumeRequest {
        volume_id: "volume".into(),
        target_path: "/tmp/volume".into(),
        volume_context: [
            ("csi.bpfman.io/program".into(), "program".into()),
            ("csi.bpfman.io/maps".into(), " map_a,map_b,map_a, ,".into()),
        ]
        .into(),
        volume_capability: Some(pb::VolumeCapability {
            access_type: Some(pb::volume_capability::AccessType::Mount(Default::default())),
            access_mode: Some(pb::volume_capability::AccessMode { mode: 1 }),
        }),
        ..Default::default()
    }
}

#[test]
fn malformed_and_cancelled_calls_never_reach_volume_effects() {
    let backend = Backend::default();
    let service = CsiService::new("node".into(), backend.clone());
    let token = Cancellation::new();
    let path = "/csi.v1.Node/NodePublishVolume";
    let mut requests = Vec::new();
    for volume in ["", "..", "../escape", "a/b", "."] {
        let mut r = request();
        r.volume_id = volume.into();
        requests.push(r);
    }
    for target in ["", "/", "relative", "/tmp/.."] {
        let mut r = request();
        r.target_path = target.into();
        requests.push(r);
    }
    for name in ["", " , ,", "..", "../map", "/map", "a/b", "."] {
        let mut r = request();
        r.volume_context
            .insert("csi.bpfman.io/maps".into(), name.into());
        requests.push(r);
    }
    let mut r = request();
    r.volume_capability = None;
    requests.push(r);
    let mut r = request();
    r.volume_capability
        .as_mut()
        .expect("capability")
        .access_mode = None;
    requests.push(r);
    for r in requests {
        assert_eq!(
            service
                .handle(path, &r.encode_to_vec(), &token)
                .expect_err("reject")
                .code(),
            StatusCode::InvalidArgument
        );
    }
    assert_eq!(
        service
            .handle(path, &[0xff], &token)
            .expect_err("malformed")
            .code(),
        StatusCode::InvalidArgument
    );
    token.cancel();
    assert_eq!(
        service
            .handle(path, &request().encode_to_vec(), &token)
            .expect_err("cancelled")
            .code(),
        StatusCode::Cancelled
    );
    assert!(backend.0.lock().expect("calls").is_empty());
    service
        .handle(path, &request().encode_to_vec(), &Cancellation::new())
        .expect("valid");
    assert_eq!(
        *backend.0.lock().expect("calls"),
        vec![("program".into(), vec!["map_a".into(), "map_b".into()])]
    );
}
