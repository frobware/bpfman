//! Frontend fault/cancellation contracts with the stateful kernel and real store.
#![allow(clippy::expect_used)]
#[allow(dead_code)]
#[path = "support/kernel.rs"]
mod kernel;

use bpfman_api::{Images, Service, Status, StatusCode};
use bpfman_fs::{RuntimeDirectory, RuntimeLayout};
use bpfman_proto::v1 as pb;
use bpfman_runtime::{ActiveStore, Bpfman, Cancellation};
use kernel::{FakeKernel, Phase, Point};
use prost::Message;
use std::time::Duration;

struct NoImages;
impl Images for NoImages {
    fn pull(
        &self,
        _: &pb::BytecodeImage,
        _: &Cancellation,
        _: &dyn Fn(&[u8]) -> Result<(), bpfman_kernel::Error>,
    ) -> Result<(Vec<u8>, bpfman_model::ProgramSource), Status> {
        Err(Status::unimplemented("image pulling not configured"))
    }
}
struct Fixture {
    _temporary: tempfile::TempDir,
    object: String,
    kernel: FakeKernel,
    service: Service<bpfman_store_sqlite::Backend, FakeKernel, NoImages>,
}
impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("fixture");
        let layout = RuntimeLayout::try_from(temporary.path().join("runtime")).expect("layout");
        let runtime = RuntimeDirectory::open_or_create(layout.clone()).expect("runtime");
        let kernel = FakeKernel::new(&runtime);
        let store = ActiveStore::open(
            bpfman_store_sqlite::Backend,
            &layout,
            Duration::from_secs(1),
        )
        .expect("store");
        let app = Bpfman::new(store, kernel.clone(), Duration::from_secs(1));
        let service = Service::new(app, kernel.clone(), NoImages, Default::default());
        let path = temporary.path().join("fixture.o");
        std::fs::write(&path, b"fake object").expect("object");
        Self {
            object: path.to_str().expect("UTF8").into(),
            _temporary: temporary,
            kernel,
            service,
        }
    }
    fn request(&self) -> pb::LoadRequest {
        pb::LoadRequest {
            bytecode: Some(pb::BytecodeLocation {
                location: Some(pb::bytecode_location::Location::File(self.object.clone())),
            }),
            info: vec![pb::LoadInfo {
                name: "counter".into(),
                program_type: 2,
                ..Default::default()
            }],
            ..Default::default()
        }
    }
    fn call<Q: Message, R: Message + Default>(
        &self,
        name: &str,
        request: Q,
        token: &Cancellation,
    ) -> Result<R, Status> {
        let bytes = self.service.handle(
            &format!("/bpfman.v1.Bpfman/{name}"),
            &request.encode_to_vec(),
            token,
        )?;
        R::decode(bytes.as_slice()).map_err(|e| Status::internal(e.to_string()))
    }
    fn load(&self) -> u32 {
        let loaded: pb::LoadResponse = self
            .call("Load", self.request(), &Cancellation::new())
            .expect("load");
        loaded.programs[0].kernel_info.as_ref().expect("kernel").id
    }
    fn attach(&self, id: u32, token: &Cancellation) -> Result<pb::AttachResponse, Status> {
        self.call(
            "Attach",
            pb::AttachRequest {
                id,
                attach: Some(pb::AttachInfo {
                    info: Some(pb::attach_info::Info::TracepointAttachInfo(
                        pb::TracepointAttachInfo {
                            tracepoint: "syscalls/sys_enter_kill".into(),
                            ..Default::default()
                        },
                    )),
                }),
            },
            token,
        )
    }
    fn unload(&self, id: u32) {
        let _: pb::UnloadResponse = self
            .call("Unload", pb::UnloadRequest { id }, &Cancellation::new())
            .expect("unload");
        self.kernel.assert_empty();
    }
}

#[test]
fn load_cleanup_receipts_survive_status_conversion_and_cancelled_retry() {
    let f = Fixture::new();
    f.kernel.fail(Point::PinProgram, Phase::After);
    f.kernel.fail(Point::RemoveProgram, Phase::Before);
    let outcome: Result<pb::LoadResponse, _> = f.call("Load", f.request(), &Cancellation::new());
    assert_eq!(
        outcome.expect_err("injected failure").code(),
        StatusCode::Internal
    );
    f.kernel.clear();
    let cancelled = Cancellation::new();
    cancelled.cancel();
    assert_eq!(
        f.service.retry_cleanup(&cancelled),
        1,
        "cancelled admission retains ownership"
    );
    assert_eq!(f.service.retry_cleanup(&Cancellation::new()), 0);
    f.kernel.assert_empty();
}

#[test]
fn attach_cleanup_receipts_survive_wire_error() {
    let f = Fixture::new();
    let id = f.load();
    f.kernel.fail(Point::PinLink, Phase::After);
    f.kernel.fail(Point::RemoveLink, Phase::Before);
    assert_eq!(
        f.attach(id, &Cancellation::new())
            .expect_err("injected failure")
            .code(),
        StatusCode::Internal
    );
    f.kernel.clear();
    assert_eq!(f.service.retry_cleanup(&Cancellation::new()), 0);
    let got: pb::GetResponse = f
        .call("Get", pb::GetRequest { id }, &Cancellation::new())
        .expect("Get");
    assert!(got.info.expect("info").links.is_empty());
    f.unload(id);
}

#[test]
fn cancellation_during_forward_attach_compensates_before_return() {
    let f = Fixture::new();
    let id = f.load();
    let token = Cancellation::new();
    f.kernel.cancel_at(Point::PinLink, &token);
    assert_eq!(
        f.attach(id, &token).expect_err("cancelled").code(),
        StatusCode::Cancelled
    );
    assert!(f.service.take_recovery().is_empty());
    let got: pb::GetResponse = f
        .call("Get", pb::GetRequest { id }, &Cancellation::new())
        .expect("Get");
    assert!(got.info.expect("info").links.is_empty());
    f.unload(id);
}

#[test]
fn request_errors_and_cancelled_reads_have_no_effects() {
    let f = Fixture::new();
    let token = Cancellation::new();
    token.cancel();
    let result: Result<pb::ListResponse, _> = f.call("List", pb::ListRequest::default(), &token);
    assert_eq!(
        result.expect_err("cancelled read").code(),
        StatusCode::Cancelled
    );
    let result: Result<pb::LoadResponse, _> =
        f.call("Load", pb::LoadRequest::default(), &Cancellation::new());
    assert_eq!(
        result.expect_err("invalid request").code(),
        StatusCode::InvalidArgument
    );
    assert!(f.service.take_recovery().is_empty());
    f.kernel.assert_empty();
}

#[test]
fn socket_admission_preserves_existing_files() {
    let f = Fixture::new();
    let path = f._temporary.path().join("existing");
    std::fs::write(&path, b"operator data").expect("existing file");
    let service = std::sync::Arc::new(f.service);
    let error = bpfman_api::Server::bind(&path, service.clone(), Default::default())
        .err()
        .expect("existing socket path refused");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&path).expect("preserved"), b"operator data");
    let missing = f._temporary.path().join("new.sock");
    let options = bpfman_api::ServerOptions {
        workers: 0,
        ..Default::default()
    };
    assert!(bpfman_api::Server::bind(&missing, service, options).is_err());
    assert!(!missing.exists());
}
