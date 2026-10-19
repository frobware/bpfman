//! Anonymous loopback registry fixture. Bodies and descriptors are independently
//! mutable to exercise content verification without an external registry.
#![allow(clippy::expect_used)]
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
};

type Responses = Arc<Mutex<BTreeMap<String, (u16, Vec<u8>)>>>;

pub(super) struct Registry {
    pub(super) reference: String,
    pub(super) bearer: Arc<AtomicBool>,
    pub(super) digest: String,
    pub(super) requests: Arc<AtomicUsize>,
    pub(super) responses: Responses,
    stop: Arc<AtomicBool>,
    address: std::net::SocketAddr,
    thread: Option<JoinHandle<()>>,
}

pub(super) fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

impl Registry {
    pub(super) fn new(bytes: Vec<u8>, index: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("registry listener");
        let address = listener.local_addr().expect("registry address");
        let layer_digest = hash(&bytes);
        let config = serde_json::to_vec(&json!({"config":{"Labels": {
            "io.ebpf.programs":"{\"pass\":\"xdp\"}", "io.ebpf.maps":"{}"
        }}}))
        .expect("config");
        let config_digest = hash(&config);
        let manifest = serde_json::to_vec(&json!({"schemaVersion":2, "mediaType":"application/vnd.oci.image.manifest.v1+json",
            "config":{"digest":config_digest,"size":config.len(),"mediaType":"application/vnd.oci.image.config.v1+json"},
            "layers":[{"digest":layer_digest,"size":bytes.len(),"mediaType":"application/octet-stream"}]})).expect("manifest");
        let child_digest = hash(&manifest);
        let arch = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            other => other,
        };
        let top = if index {
            serde_json::to_vec(&json!({"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json",
            "manifests":[{"digest":child_digest,"size":manifest.len(),"mediaType":"application/vnd.oci.image.manifest.v1+json","platform":{"os":"linux","architecture":arch}}]})).expect("index")
        } else {
            manifest.clone()
        };
        let top_digest = hash(&top);
        let mut routes = BTreeMap::new();
        for (path, bytes) in [
            ("/manifests/latest".into(), top.clone()),
            (format!("/manifests/{top_digest}"), top),
            (format!("/manifests/{child_digest}"), manifest),
            (format!("/blobs/{config_digest}"), config),
            (format!("/blobs/{layer_digest}"), bytes),
        ] {
            routes.insert(format!("/v2/test{path}"), (200, bytes));
        }
        let responses = Arc::new(Mutex::new(routes));
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(AtomicUsize::new(0));
        let bearer = Arc::new(AtomicBool::new(false));
        let server_bearer = bearer.clone();
        let server_stop = stop.clone();
        let server_responses = responses.clone();
        let server_requests = requests.clone();
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if server_stop.load(Ordering::Acquire) {
                    break;
                }
                let mut stream = stream.expect("registry connection");
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .expect("timeout");
                // Stock registry clients may probe TLS before HTTP on loopback.
                let mut first = [0];
                if stream.peek(&mut first).unwrap_or(0) == 0 || first[0] == 0x16 {
                    continue;
                }
                let mut request = Vec::new();
                let mut byte = [0];
                while !request.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap_or(0) == 0 {
                        break;
                    }
                    request.push(byte[0]);
                    assert!(request.len() < 64 * 1024);
                }
                let request = String::from_utf8(request).expect("HTTP request");
                let path = request.split_whitespace().nth(1).unwrap_or_default();
                server_requests.fetch_add(1, Ordering::Relaxed);
                let (status, body) = if path.starts_with("/token?") {
                    (200, b"{\"token\":\"fixture-token\"}".to_vec())
                } else if server_bearer.load(Ordering::Relaxed)
                    && !request
                        .to_ascii_lowercase()
                        .contains("authorization: bearer fixture-token")
                {
                    (401, Vec::new())
                } else {
                    server_responses
                        .lock()
                        .expect("routes")
                        .get(path)
                        .cloned()
                        .unwrap_or((404, Vec::new()))
                };
                let challenge = if status == 401 {
                    format!(
                        "WWW-Authenticate: Bearer realm=\"http://{address}/token\",service=\"fixture\",scope=\"repository:test:pull\"\r\n"
                    )
                } else {
                    String::new()
                };
                let manifest: Option<serde_json::Value> = path
                    .contains("/manifests/")
                    .then(|| serde_json::from_slice(&body).ok())
                    .flatten();
                let media_type = manifest
                    .as_ref()
                    .and_then(|manifest| manifest["mediaType"].as_str())
                    .unwrap_or("application/octet-stream");
                let header = format!(
                    "HTTP/1.1 {status} result\r\n{challenge}Content-Length: {}\r\nContent-Type: {media_type}\r\nDocker-Content-Digest: {}\r\nConnection: close\r\n\r\n",
                    body.len(),
                    hash(&body)
                );
                let _ = stream.write_all(header.as_bytes());
                if !request.starts_with("HEAD ") {
                    let _ = stream.write_all(&body);
                }
            }
        });
        Self {
            bearer,
            reference: format!("{address}/test"),
            digest: child_digest,
            requests,
            responses,
            stop,
            address,
            thread: Some(thread),
        }
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("registry thread");
        }
    }
}
