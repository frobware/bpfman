//! Correlation across the real socket, admission, operation and monitor threads.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use bpfman_api::{Execution, Handler, RpcServer, ServerOptions, Status, StatusCode};
use bpfman_runtime::Cancellation;
use serde_json::Value;
use std::{
    io::Write,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tracing_subscriber::fmt::format::FmtSpan;

#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logs {
    fn events(&self) -> Vec<Value> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).expect("JSON event"))
            .collect()
    }

    fn wait_for(&self, predicate: impl Fn(&[Value]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !predicate(&self.events()) {
            assert!(
                Instant::now() < deadline,
                "missing event: {:?}",
                self.events()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

struct TestHandler {
    started: mpsc::Sender<u8>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl Handler for TestHandler {
    fn methods() -> &'static [(&'static str, Execution)] {
        &[
            ("/test.Service/Work", Execution::Mutation),
            ("/test.Service/Read", Execution::Read),
            ("/test.Service/Release", Execution::Teardown),
        ]
    }

    fn handle(&self, _: &str, bytes: &[u8], token: &Cancellation) -> Result<Vec<u8>, Status> {
        let action = bytes[0];
        let _span = tracing::debug_span!("test.runtime", action).entered();
        tracing::debug!(action, "handler entered");
        self.started.send(action).unwrap();
        match action {
            1 | 5 => self
                .release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap(),
            2 => return Err(Status::internal("private-status-secret")),
            3 => return Err(Status::not_found("private-status-secret")),
            4 => {
                let deadline = Instant::now() + Duration::from_secs(5);
                while !token.is_cancelled() {
                    assert!(Instant::now() < deadline, "cancellation not delivered");
                    std::thread::sleep(Duration::from_millis(2));
                }
                tracing::debug!("handler observed cancellation");
                return Err(Status::cancelled("private-status-secret"));
            }
            _ => {}
        }
        Ok(Vec::new())
    }
}

struct Fixture {
    _temporary: tempfile::TempDir,
    socket: std::path::PathBuf,
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    started: mpsc::Receiver<u8>,
    release: mpsc::Sender<()>,
    logs: Logs,
}

impl Fixture {
    fn new(workers: usize, pending: usize, level: tracing::Level) -> Self {
        Self::with_handler(workers, pending, level, |started, release| TestHandler {
            started,
            release: Mutex::new(release),
        })
    }

    fn with_handler<H: Handler>(
        workers: usize,
        pending: usize,
        level: tracing::Level,
        build: impl FnOnce(mpsc::Sender<u8>, mpsc::Receiver<()>) -> H,
    ) -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("rpc.sock");
        let logs = Logs::default();
        let output = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .with_max_level(level)
            .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
            .with_writer(move || output.clone())
            .finish();
        let dispatch = tracing::Dispatch::new(subscriber);
        let (started_tx, started) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        // Only bind has the caller's subscriber. Server/worker/monitor threads
        // must explicitly inherit it; a global subscriber would hide the bug.
        let server = tracing::dispatcher::with_default(&dispatch, || {
            RpcServer::bind(
                &socket,
                Arc::new(build(started_tx, release_rx)),
                ServerOptions {
                    workers,
                    readers: 1,
                    pending,
                },
            )
            .unwrap()
        });
        let shutdown = Arc::new(AtomicBool::new(false));
        let signal = shutdown.clone();
        let thread = std::thread::spawn(move || {
            server.serve(&signal).unwrap();
            tracing::dispatcher::with_default(&dispatch, || tracing::debug!("outside request"));
        });
        Self {
            _temporary: temporary,
            socket,
            shutdown,
            thread: Some(thread),
            started,
            release,
            logs,
        }
    }

    fn client(&self) -> grpcuds::Client {
        let mut client =
            grpcuds::Client::connect_wait(&self.socket, Duration::from_secs(5)).unwrap();
        client.set_timeout(Some(Duration::from_secs(5)));
        client
    }

    fn call(
        &self,
        method: &'static str,
        bytes: Vec<u8>,
    ) -> std::thread::JoinHandle<Result<Vec<u8>, Status>> {
        let mut client = self.client();
        std::thread::spawn(move || client.unary(method, &bytes))
    }

    fn stop(mut self) -> Vec<Value> {
        self.shutdown.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
        self.logs.events()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        // Release any blocked handler even if an assertion fails.
        let _ = self.release.send(());
        let _ = self.release.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn request(event: &Value) -> &Value {
    event["spans"]
        .as_array()
        .unwrap()
        .iter()
        .find(|span| span["name"] == "rpc.request")
        .expect("request ancestor")
}

fn message(event: &Value, text: &str) -> bool {
    event["fields"]["message"] == text
}

#[test]
fn concurrent_workers_keep_distinct_request_ancestry_and_timings() {
    let f = Fixture::new(2, 4, tracing::Level::DEBUG);
    let first = f.call("/test.Service/Work", vec![1]);
    let second = f.call("/test.Service/Release", vec![5]);
    let a = f.started.recv_timeout(Duration::from_secs(5)).unwrap();
    let b = f.started.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_ne!(a, b, "both handlers overlap before release");
    f.release.send(()).unwrap();
    f.release.send(()).unwrap();
    first.join().unwrap().unwrap();
    second.join().unwrap().unwrap();
    let events = f.stop();
    let entered: Vec<_> = events
        .iter()
        .filter(|e| message(e, "handler entered"))
        .collect();
    assert_eq!(entered.len(), 2);
    assert_ne!(
        request(entered[0])["request_id"],
        request(entered[1])["request_id"]
    );
    for event in entered {
        let parent = request(event);
        let action = event["fields"]["action"].as_u64().unwrap();
        assert_eq!(
            parent["rpc_method"],
            if action == 1 {
                "/test.Service/Work"
            } else {
                "/test.Service/Release"
            }
        );
        let completed = events
            .iter()
            .find(|e| {
                message(e, "RPC completed") && e["fields"]["request_id"] == parent["request_id"]
            })
            .unwrap();
        assert_eq!(completed["fields"]["grpc_status"], "Ok");
        assert_eq!(completed["fields"]["response_queued"], true);
        assert_eq!(completed["level"], "INFO");
        let fields = &completed["fields"];
        let queue = fields["queue_ms"].as_f64().unwrap();
        let execution = fields["execution_ms"].as_f64().unwrap();
        let total = fields["total_ms"].as_f64().unwrap();
        assert!(queue >= 0.0 && execution >= 0.0 && (queue + execution - total).abs() < 0.001);
    }
    let outside = events
        .iter()
        .find(|e| message(e, "outside request"))
        .unwrap();
    assert!(
        outside.get("spans").is_none(),
        "no context leaks outside a call"
    );
}

#[test]
fn admission_rejections_and_failures_are_correlated_and_redacted() {
    let f = Fixture::new(1, 1, tracing::Level::DEBUG);
    let first = f.call("/test.Service/Work", vec![1]);
    f.started.recv_timeout(Duration::from_secs(5)).unwrap();
    let queued = f.call("/test.Service/Work", vec![0]);
    f.logs
        .wait_for(|events| events.iter().filter(|e| message(e, "RPC admitted")).count() == 2);
    let rejected = f
        .client()
        .unary("/test.Service/Work", b"private-payload-secret")
        .unwrap_err();
    assert_eq!(rejected.code(), StatusCode::ResourceExhausted);
    // Independent read workers keep progressing while the mutation queue is full.
    for (action, code) in [(2, StatusCode::Internal), (3, StatusCode::NotFound)] {
        let status = f
            .client()
            .unary("/test.Service/Read", &[action])
            .unwrap_err();
        assert_eq!(status.code(), code);
        assert_eq!(status.message(), Some("private-status-secret"));
    }
    let oversized = f
        .client()
        .unary("/test.Service/Read", &vec![0; 4 * 1024 * 1024 + 1])
        .unwrap_err();
    assert_eq!(oversized.code(), StatusCode::ResourceExhausted);
    f.release.send(()).unwrap();
    first.join().unwrap().unwrap();
    queued.join().unwrap().unwrap();
    let events = f.stop();
    let rejected: Vec<_> = events
        .iter()
        .filter(|e| message(e, "RPC rejected"))
        .collect();
    assert_eq!(rejected.len(), 2);
    for event in rejected {
        assert_eq!(event["fields"]["request_id"], request(event)["request_id"]);
        assert_eq!(event["fields"]["grpc_status"], "ResourceExhausted");
    }
    for (status, level) in [("Internal", "ERROR"), ("NotFound", "DEBUG")] {
        let event = events
            .iter()
            .find(|e| message(e, "RPC completed") && e["fields"]["grpc_status"] == status)
            .unwrap();
        assert_eq!(event["level"], level);
        assert_eq!(event["fields"]["request_id"], request(event)["request_id"]);
    }
    let serialized = serde_json::to_string(&events).unwrap();
    assert!(!serialized.contains("private-payload-secret"));
    assert!(!serialized.contains("private-status-secret"));
}

#[test]
fn deadline_and_shutdown_cancellation_keep_the_request_context() {
    let f = Fixture::new(1, 4, tracing::Level::DEBUG);
    let mut client = f.client();
    client.set_timeout(Some(Duration::from_millis(100)));
    let deadline = std::thread::spawn(move || client.unary("/test.Service/Work", &[4]));
    f.started.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(deadline.join().unwrap().is_err());
    f.logs
        .wait_for(|events| events.iter().any(|e| message(e, "RPC completed")));
    let shutdown = f.call("/test.Service/Work", vec![4]);
    f.started.recv_timeout(Duration::from_secs(5)).unwrap();
    let events = f.stop();
    let _ = shutdown.join().unwrap();
    let cancellations: Vec<_> = events
        .iter()
        .filter(|e| message(e, "RPC cancellation requested"))
        .collect();
    assert_eq!(cancellations.len(), 2, "one observation per request");
    assert_eq!(cancellations[0]["fields"]["reason"], "deadline");
    assert_eq!(cancellations[1]["fields"]["reason"], "shutdown");
    for event in cancellations {
        let id = &event["fields"]["request_id"];
        assert_eq!(*id, request(event)["request_id"]);
        let completed = events
            .iter()
            .find(|e| message(e, "RPC completed") && e["fields"]["request_id"] == *id)
            .unwrap();
        assert_eq!(completed["fields"]["grpc_status"], "Cancelled");
        assert_eq!(completed["fields"]["cancellation_requested"], true);
        assert!(events.iter().any(
            |e| message(e, "handler observed cancellation") && request(e)["request_id"] == *id
        ));
    }
}

#[test]
fn read_server_errors_keep_identifiers_when_debug_spans_are_filtered() {
    let f = Fixture::new(1, 4, tracing::Level::INFO);
    assert_eq!(
        f.client()
            .unary("/test.Service/Read", &[2])
            .unwrap_err()
            .code(),
        StatusCode::Internal
    );
    let events = f.stop();
    let completed = events.iter().find(|e| message(e, "RPC completed")).unwrap();
    assert_eq!(completed["level"], "ERROR");
    assert_eq!(completed["fields"]["rpc_method"], "/test.Service/Read");
    assert!(completed["fields"]["request_id"].as_u64().is_some());
}

#[test]
fn disconnected_client_cancellation_is_logged_once() {
    let f = Fixture::new(1, 4, tracing::Level::DEBUG);
    let mut client = f.client();
    client.set_timeout(None);
    {
        let _stream = client.server_streaming("/test.Service/Work", &[4]).unwrap();
        f.started.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    drop(client);
    f.logs
        .wait_for(|events| events.iter().any(|e| message(e, "RPC completed")));
    let events = f.stop();
    let cancellations: Vec<_> = events
        .iter()
        .filter(|e| message(e, "RPC cancellation requested"))
        .collect();
    assert_eq!(cancellations.len(), 1);
    assert_eq!(cancellations[0]["fields"]["reason"], "client_disconnect");
    let completed = events.iter().find(|e| message(e, "RPC completed")).unwrap();
    assert_eq!(
        completed["fields"]["request_id"],
        request(cancellations[0])["request_id"]
    );
    assert_eq!(completed["fields"]["response_queued"], false);
}

#[test]
fn csi_volume_identity_is_correlated_without_logging_context_or_secrets() {
    use bpfman_api::{CsiService, Volumes};
    use bpfman_fs::VolumeRequest;
    use bpfman_proto::csi as pb;
    use prost::Message;
    struct Backend;
    impl Volumes for Backend {
        fn publish(&self, _: &str, _: &VolumeRequest, _: &Cancellation) -> Result<(), Status> {
            let _span = tracing::debug_span!("test.volume").entered();
            tracing::debug!("volume backend called");
            Ok(())
        }
        fn unpublish(&self, _: &str, _: &std::path::Path, _: &Cancellation) -> Result<(), Status> {
            tracing::debug!("volume backend called");
            Ok(())
        }
    }
    let f = Fixture::with_handler(1, 4, tracing::Level::DEBUG, |_, _| {
        CsiService::new("node".into(), Backend)
    });
    let publish = pb::NodePublishVolumeRequest {
        volume_id: "test-volume".into(),
        target_path: "/private-target-secret".into(),
        volume_context: [
            (
                "csi.bpfman.io/program".into(),
                "private-selector-secret".into(),
            ),
            ("csi.bpfman.io/maps".into(), "test_map".into()),
        ]
        .into(),
        secrets: [("password".into(), "private-csi-secret".into())].into(),
        volume_capability: Some(pb::VolumeCapability {
            access_type: Some(pb::volume_capability::AccessType::Mount(Default::default())),
            access_mode: Some(pb::volume_capability::AccessMode { mode: 1 }),
        }),
        ..Default::default()
    };
    f.client()
        .unary("/csi.v1.Node/NodePublishVolume", &publish.encode_to_vec())
        .unwrap();
    f.client()
        .unary(
            "/csi.v1.Node/NodeUnpublishVolume",
            &pb::NodeUnpublishVolumeRequest {
                volume_id: publish.volume_id,
                target_path: publish.target_path,
            }
            .encode_to_vec(),
        )
        .unwrap();
    let events = f.stop();
    let backend: Vec<_> = events
        .iter()
        .filter(|e| message(e, "volume backend called"))
        .collect();
    assert_eq!(backend.len(), 2);
    assert_ne!(
        request(backend[0])["request_id"],
        request(backend[1])["request_id"]
    );
    for event in backend {
        assert_eq!(request(event)["volume_id"], "test-volume");
    }
    let serialized = serde_json::to_string(&events).unwrap();
    for secret in [
        "private-target-secret",
        "private-selector-secret",
        "private-csi-secret",
    ] {
        assert!(!serialized.contains(secret));
    }
}
