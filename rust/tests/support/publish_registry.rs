//! Mutable loopback distribution registry for publishing and consumer contracts.
#![allow(clippy::expect_used)]
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

#[derive(Default)]
pub(super) struct State {
    pub(super) requests: Vec<String>,
    pub(super) blobs: BTreeMap<String, Vec<u8>>,
    pub(super) manifests: BTreeMap<String, Vec<u8>>,
    pub(super) uploads: BTreeSet<String>,
    pub(super) faults: BTreeMap<String, u16>,
    pub(super) cancel_on: Option<String>,
    pub(super) location: Option<String>,
    pub(super) bearer: bool,
    pub(super) corrupt_digest: bool,
    pub(super) pause: Option<(
        String,
        std::sync::mpsc::SyncSender<()>,
        std::sync::mpsc::Receiver<()>,
    )>,
}

pub(super) struct Registry {
    pub(super) reference: String,
    pub(super) state: Arc<Mutex<State>>,
    pub(super) cancelled: Arc<AtomicBool>,
    address: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

pub(super) fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

impl Registry {
    pub(super) fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
        let address = listener.local_addr().expect("address");
        let state = Arc::new(Mutex::new(State::default()));
        let cancelled = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_state = state.clone();
        let worker_cancelled = cancelled.clone();
        let worker_stop = stop.clone();
        let thread = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if worker_stop.load(Ordering::Acquire) {
                    break;
                }
                let mut stream = stream.expect("connection");
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .expect("timeout");
                // Go's registry client probes HTTPS before falling back to HTTP
                // on loopback. Close the TLS probe without interpreting it as HTTP.
                let mut first = [0];
                if stream.peek(&mut first).unwrap_or(0) == 0 || first[0] == 0x16 {
                    continue;
                }
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if stream.read(&mut byte).unwrap_or(0) == 0 {
                        break;
                    }
                    header.push(byte[0]);
                    assert!(header.len() < 64 * 1024);
                }
                let header = String::from_utf8(header).expect("HTTP header");
                let mut words = header.split_whitespace();
                let method = words.next().unwrap_or_default();
                let url = words.next().unwrap_or_default();
                let path = url.split('?').next().unwrap_or_default();
                let count = header
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                    .map(|(_, value)| value.trim().parse::<usize>().expect("length"))
                    .unwrap_or(0);
                assert!(count <= 20 * 1024 * 1024);
                let mut body = vec![0; count];
                stream.read_exact(&mut body).expect("request body");
                let key = format!("{method} {path}");
                let mut state = worker_state.lock().expect("state");
                state.requests.push(format!("{method} {url}"));
                let mut headers = String::new();
                let (status, output) = if path == "/v2/" {
                    (200, Vec::new())
                } else if path == "/token" {
                    (200, b"{\"token\":\"publish-token\"}".to_vec())
                } else if state.bearer
                    && !header
                        .to_ascii_lowercase()
                        .contains("authorization: bearer publish-token")
                {
                    headers.push_str(&format!("WWW-Authenticate: Bearer realm=\"http://{address}/token\",service=\"fixture\"\r\n"));
                    (401, Vec::new())
                } else if let Some(status) = state
                    .faults
                    .iter()
                    .find(|(pattern, _)| {
                        pattern.as_str() == key
                            || pattern
                                .strip_suffix('*')
                                .is_some_and(|prefix| key.starts_with(prefix))
                    })
                    .map(|(_, status)| *status)
                {
                    (status, Vec::new())
                } else if method == "POST" && path == "/v2/test/blobs/uploads/" {
                    let location = format!("/v2/test/blobs/uploads/{}", state.requests.len());
                    state.uploads.insert(location.clone());
                    headers.push_str(&format!(
                        "Location: {}\r\n",
                        state.location.as_deref().unwrap_or(&location)
                    ));
                    (202, Vec::new())
                } else if method == "PUT" && state.uploads.contains(path) {
                    let digest = url
                        .split_once("digest=")
                        .map(|(_, value)| value)
                        .unwrap_or_default();
                    if hash(&body) != digest {
                        (400, Vec::new())
                    } else {
                        state.uploads.remove(path);
                        state.blobs.insert(digest.into(), body);
                        headers.push_str(&format!("Docker-Content-Digest: {digest}\r\n"));
                        (201, Vec::new())
                    }
                } else if method == "DELETE" && path.starts_with("/v2/test/blobs/uploads/") {
                    let existed = state.uploads.remove(path);
                    (if existed { 204 } else { 404 }, Vec::new())
                } else if let Some(selector) = path.strip_prefix("/v2/test/manifests/") {
                    if method == "PUT" {
                        let digest = hash(&body);
                        state.manifests.insert(selector.into(), body.clone());
                        state.manifests.insert(digest.clone(), body);
                        headers.push_str(&format!(
                            "Docker-Content-Digest: {}\r\n",
                            if state.corrupt_digest {
                                hash(b"wrong")
                            } else {
                                digest
                            }
                        ));
                        (201, Vec::new())
                    } else {
                        match state.manifests.get(selector) {
                            Some(body) => (200, body.clone()),
                            None => (404, Vec::new()),
                        }
                    }
                } else if let Some(digest) = path.strip_prefix("/v2/test/blobs/") {
                    match state.blobs.get(digest) {
                        Some(body) => (
                            200,
                            if method == "HEAD" {
                                Vec::new()
                            } else {
                                body.clone()
                            },
                        ),
                        None => (404, Vec::new()),
                    }
                } else {
                    (404, Vec::new())
                };
                if state.cancel_on.as_deref() == Some(key.as_str()) {
                    worker_cancelled.store(true, Ordering::Release);
                }
                if matches!(method, "GET" | "HEAD")
                    && path.starts_with("/v2/test/manifests/")
                    && status == 200
                {
                    headers.push_str(&format!("Docker-Content-Digest: {}\r\n", hash(&output)));
                    let manifest: serde_json::Value =
                        serde_json::from_slice(&output).expect("manifest");
                    headers.push_str(&format!(
                        "Content-Type: {}\r\n",
                        manifest["mediaType"].as_str().expect("media type")
                    ));
                }
                let pause = if state
                    .pause
                    .as_ref()
                    .is_some_and(|(pattern, _, _)| pattern == &key)
                {
                    state.pause.take()
                } else {
                    None
                };
                drop(state);
                if let Some((_, waiting, resume)) = pause {
                    waiting.send(()).expect("notify pause");
                    resume
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .expect("resume response");
                }
                let response = format!(
                    "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
                    output.len()
                );
                let _ = stream.write_all(response.as_bytes());
                if method != "HEAD" {
                    let _ = stream.write_all(&output);
                }
            }
        });
        Self {
            reference: format!("{address}/test:latest"),
            state,
            cancelled,
            address,
            stop,
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
