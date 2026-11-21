//! Small anonymous OCI distribution fixture bound to loopback.

use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use sha2::{Digest, Sha256};

const MAX_LINE: u64 = 64 * 1024;
const MAX_BODY: usize = 128 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct Registry {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<io::Result<()>>>,
    failure: Option<String>,
}

impl Registry {
    pub(super) fn start() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let observe_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("bpfman-fixture-registry".to_owned())
            .spawn(move || run_listener(listener.incoming(), &observe_stop))?;
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
            failure: None,
        })
    }

    pub(super) fn address(&mut self) -> Result<SocketAddr, String> {
        if self.worker.as_ref().is_some_and(JoinHandle::is_finished) {
            self.join();
        }
        self.result().map(|()| self.address)
    }

    pub(super) fn finish(mut self) -> Result<(), String> {
        // A lookup that already joined the worker returned its failure.
        if self.worker.is_none() {
            return Ok(());
        }
        self.stop_worker();
        self.result()
    }

    fn result(&self) -> Result<(), String> {
        self.failure
            .as_ref()
            .map_or(Ok(()), |error| Err(error.clone()))
    }

    fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.failure = match worker.join() {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(format!("accept fixture registry connection: {error}")),
                Err(_) => Some("fixture registry worker panicked".to_owned()),
            };
        }
    }

    fn stop_worker(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = &self.worker
            && !worker.is_finished()
            && let Err(error) = TcpStream::connect_timeout(&self.address, TIMEOUT)
            && !worker.is_finished()
        {
            // The worker may be waiting in accept. A failed wake cannot
            // guarantee a bounded join.
            drop(self.worker.take());
            self.failure = Some(format!("wake fixture registry: {error}"));
            return;
        }
        self.join();
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        if self.worker.is_some() {
            self.stop_worker();
            super::super::report_cleanup("stop fixture registry", self.result());
        }
    }
}

fn run_listener(
    incoming: impl IntoIterator<Item = io::Result<TcpStream>>,
    stop: &AtomicBool,
) -> io::Result<()> {
    let mut state = State::default();
    for accepted in incoming {
        match accepted {
            Ok(_) if stop.load(Ordering::Acquire) => break,
            Ok(mut stream) => {
                // A malformed request or disconnected client only ends this connection.
                drop(serve(&mut stream, &mut state));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn serve(stream: &mut TcpStream, state: &mut State) -> io::Result<()> {
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let mut first = [0];
    // The Go CLI may try TLS first for local registries.
    if stream.peek(&mut first)? == 0 || first[0] == 0x16 {
        return Ok(());
    }
    let request = Request::read(stream)?;
    let head = request.method == "HEAD";
    state.respond(request).write(stream, head)
}

struct Request {
    method: String,
    target: String,
    media_type: Option<String>,
    body: Vec<u8>,
}

impl Request {
    fn read(stream: &mut TcpStream) -> io::Result<Self> {
        let mut reader = BufReader::new(stream);
        let start = line(&mut reader)?;
        let mut words = start.split_whitespace();
        let method = words.next().ok_or_else(malformed)?.to_owned();
        let target = words.next().ok_or_else(malformed)?.to_owned();
        let mut length = 0;
        let mut media_type = None;
        let mut chunked = false;
        let mut continue_body = false;
        for _ in 0..128 {
            let header = line(&mut reader)?;
            if header.is_empty() {
                if continue_body {
                    reader
                        .get_mut()
                        .write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
                }
                let body = if chunked {
                    chunks(&mut reader)?
                } else {
                    if length > MAX_BODY {
                        return Err(malformed());
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body)?;
                    body
                };
                return Ok(Self {
                    method,
                    target,
                    media_type,
                    body,
                });
            }
            let (key, value) = header.split_once(':').ok_or_else(malformed)?;
            let value = value.trim();
            if key.eq_ignore_ascii_case("content-length") {
                length = value.parse().map_err(|_| malformed())?;
            } else if key.eq_ignore_ascii_case("transfer-encoding") {
                if !value.eq_ignore_ascii_case("chunked") {
                    return Err(malformed());
                }
                chunked = true;
            } else if key.eq_ignore_ascii_case("content-type") {
                media_type = Some(value.to_owned());
            } else if key.eq_ignore_ascii_case("expect") {
                continue_body = value.eq_ignore_ascii_case("100-continue");
            }
        }
        Err(malformed())
    }
}

fn line(reader: &mut impl BufRead) -> io::Result<String> {
    let mut text = String::new();
    reader.take(MAX_LINE).read_line(&mut text)?;
    text.strip_suffix("\r\n")
        .map(str::to_owned)
        .ok_or_else(malformed)
}

fn chunks(reader: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let size = line(reader)?;
        let size = size.split(';').next().ok_or_else(malformed)?;
        let size = usize::from_str_radix(size, 16).map_err(|_| malformed())?;
        if size == 0 {
            for _ in 0..128 {
                if line(reader)?.is_empty() {
                    return Ok(body);
                }
            }
            return Err(malformed());
        }
        let length = body
            .len()
            .checked_add(size)
            .filter(|length| *length <= MAX_BODY)
            .ok_or_else(malformed)?;
        let offset = body.len();
        body.resize(length, 0);
        reader.read_exact(&mut body[offset..])?;
        if !line(reader)?.is_empty() {
            return Err(malformed());
        }
    }
}

fn malformed() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "malformed fixture registry request",
    )
}

#[derive(Default)]
struct State {
    last_upload: u64,
    uploads: BTreeMap<(String, String), Vec<u8>>,
    blobs: BTreeMap<(String, String), Arc<Artifact>>,
    manifests: BTreeMap<(String, String), Arc<Artifact>>,
}

struct Artifact {
    digest: String,
    media_type: String,
    bytes: Arc<[u8]>,
}

impl State {
    fn respond(&mut self, request: Request) -> Reply {
        let (path, query) = request
            .target
            .split_once('?')
            .unwrap_or((&request.target, ""));
        let method = request.method.as_str();
        if path == "/v2/" || path == "/v2" {
            return Reply::new("200 OK");
        }
        let Some(path) = path.strip_prefix("/v2/") else {
            return Reply::missing("NAME_UNKNOWN");
        };
        if let Some((repository, upload)) = path.rsplit_once("/blobs/uploads/") {
            return self.upload(method, repository, upload, query, request.body);
        }
        if let Some((repository, digest)) = path.rsplit_once("/blobs/")
            && matches!(method, "HEAD" | "GET")
        {
            return get(&self.blobs, repository, digest, "BLOB_UNKNOWN");
        }
        if let Some((repository, selector)) = path.rsplit_once("/manifests/") {
            return match method {
                "HEAD" | "GET" => get(&self.manifests, repository, selector, "MANIFEST_UNKNOWN"),
                "PUT" => {
                    let digest = hash(&request.body);
                    if selector.starts_with("sha256:") && selector != digest {
                        return Reply::new("400 Bad Request");
                    }
                    let artifact = Arc::new(Artifact {
                        digest: digest.clone(),
                        media_type: request.media_type.unwrap_or_else(|| {
                            "application/vnd.oci.image.manifest.v1+json".to_owned()
                        }),
                        bytes: request.body.into(),
                    });
                    self.manifests.insert(
                        (repository.to_owned(), selector.to_owned()),
                        Arc::clone(&artifact),
                    );
                    self.manifests
                        .insert((repository.to_owned(), digest.clone()), artifact);
                    Reply::new("201 Created")
                        .header("Location", format!("/v2/{repository}/manifests/{digest}"))
                        .header("Docker-Content-Digest", digest)
                }
                _ => Reply::new("405 Method Not Allowed"),
            };
        }
        Reply::missing("NAME_UNKNOWN")
    }

    fn upload(
        &mut self,
        method: &str,
        repository: &str,
        upload: &str,
        query: &str,
        body: Vec<u8>,
    ) -> Reply {
        if method == "POST" && upload.is_empty() {
            let Some(id) = self.last_upload.checked_add(1) else {
                return Reply::new("503 Service Unavailable");
            };
            self.last_upload = id;
            let upload = id.to_string();
            let length = body.len();
            self.uploads
                .insert((repository.to_owned(), upload.clone()), body);
            return upload_reply("202 Accepted", repository, &upload, length);
        }
        let key = (repository.to_owned(), upload.to_owned());
        let Some(bytes) = self.uploads.get_mut(&key) else {
            return Reply::missing("BLOB_UPLOAD_UNKNOWN");
        };
        let combined = bytes.len().checked_add(body.len());
        if combined.is_none_or(|length| length > MAX_BODY) {
            return Reply::new("413 Payload Too Large");
        }
        match method {
            "PATCH" => {
                bytes.extend(body);
                upload_reply("202 Accepted", repository, upload, bytes.len())
            }
            "PUT" => {
                let digest = format!(
                    "sha256:{:x}",
                    Sha256::new()
                        .chain_update(&*bytes)
                        .chain_update(&body)
                        .finalize()
                );
                let expected = query
                    .split('&')
                    .find_map(|field| field.strip_prefix("digest="))
                    .map(|text| text.replace("%3A", ":").replace("%3a", ":"));
                if expected.as_deref() != Some(digest.as_str()) {
                    return Reply::new("400 Bad Request");
                }
                let Some(mut bytes) = self.uploads.remove(&key) else {
                    return Reply::missing("BLOB_UPLOAD_UNKNOWN");
                };
                bytes.extend(body);
                let artifact = Arc::new(Artifact {
                    digest: digest.clone(),
                    media_type: "application/octet-stream".to_owned(),
                    bytes: bytes.into(),
                });
                self.blobs
                    .insert((repository.to_owned(), digest.clone()), artifact);
                Reply::new("201 Created")
                    .header("Location", format!("/v2/{repository}/blobs/{digest}"))
                    .header("Docker-Content-Digest", digest)
            }
            "DELETE" => {
                self.uploads.remove(&key);
                Reply::new("204 No Content")
            }
            "GET" => upload_reply("204 No Content", repository, upload, bytes.len()),
            _ => Reply::new("405 Method Not Allowed"),
        }
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn upload_reply(status: &'static str, repository: &str, upload: &str, length: usize) -> Reply {
    Reply::new(status)
        .header(
            "Location",
            format!("/v2/{repository}/blobs/uploads/{upload}"),
        )
        .header("Docker-Upload-UUID", upload.to_owned())
        .header("Range", format!("0-{}", length.saturating_sub(1)))
}

fn get(
    artifacts: &BTreeMap<(String, String), Arc<Artifact>>,
    repository: &str,
    selector: &str,
    missing: &str,
) -> Reply {
    let Some(artifact) = artifacts.get(&(repository.to_owned(), selector.to_owned())) else {
        return Reply::missing(missing);
    };
    let mut reply = Reply::new("200 OK")
        .header("Docker-Content-Digest", artifact.digest.clone())
        .header("Content-Type", artifact.media_type.clone());
    reply.body = Some(Arc::clone(&artifact.bytes));
    reply
}

struct Reply {
    status: &'static str,
    headers: Vec<(&'static str, String)>,
    body: Option<Arc<[u8]>>,
}

impl Reply {
    fn new(status: &'static str) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: None,
        }
    }

    fn missing(code: &str) -> Self {
        let mut reply =
            Self::new("404 Not Found").header("Content-Type", "application/json".to_owned());
        reply.body = Some(
            format!(r#"{{"errors":[{{"code":"{code}"}}]}}"#)
                .into_bytes()
                .into(),
        );
        reply
    }

    fn header(mut self, key: &'static str, value: String) -> Self {
        self.headers.push((key, value));
        self
    }

    fn write(self, stream: &mut TcpStream, head: bool) -> io::Result<()> {
        let body = self.body.as_deref().unwrap_or_default();
        write!(
            stream,
            "HTTP/1.1 {}\r\nConnection: close\r\nDistribution-API-Version: registry/2.0\r\nContent-Length: {}\r\n",
            self.status,
            body.len()
        )?;
        for (key, value) in self.headers {
            write!(stream, "{key}: {value}\r\n")?;
        }
        stream.write_all(b"\r\n")?;
        if !head {
            stream.write_all(body)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listener_failure_survives_join_and_prevents_reusing_its_address() -> Result<(), String> {
        let address = "127.0.0.1:1234"
            .parse()
            .map_err(|error: std::net::AddrParseError| error.to_string())?;
        let failed_registry = || Registry {
            address,
            stop: Arc::new(AtomicBool::new(false)),
            worker: Some(thread::spawn(|| {
                run_listener(
                    [
                        Err(io::Error::from(io::ErrorKind::Interrupted)),
                        Err(io::Error::from_raw_os_error(
                            rustix::io::Errno::MFILE.raw_os_error(),
                        )),
                    ],
                    &AtomicBool::new(false),
                )
            })),
            failure: None,
        };
        let mut registry = failed_registry();
        registry.join();
        let error = registry.address().expect_err("listener failure");
        assert!(
            error.contains("accept fixture registry connection"),
            "{error}"
        );
        assert!(error.contains("Too many open files"), "{error}");
        assert_eq!(registry.address(), Err(error.clone()));
        assert_eq!(registry.finish(), Ok(()));
        let registry = failed_registry();
        while !registry
            .worker
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
        {
            thread::yield_now();
        }
        assert_eq!(registry.finish(), Err(error));
        Ok(())
    }

    #[test]
    fn digest_mismatch_leaves_the_upload_available_for_retry() -> Result<(), &'static str> {
        let mut state = State::default();
        let request = |method: &str, target: &str, body: &[u8]| Request {
            method: method.to_owned(),
            target: target.to_owned(),
            media_type: None,
            body: body.to_vec(),
        };
        let started = state.respond(request("POST", "/v2/test/bytes/blobs/uploads/", b""));
        assert_eq!(started.status, "202 Accepted");
        let upload = started
            .headers
            .iter()
            .find_map(|(name, value)| (*name == "Location").then_some(value))
            .ok_or("missing upload location")?;
        assert_eq!(
            state.respond(request("PATCH", upload, b"first")).status,
            "202 Accepted"
        );
        assert_eq!(
            state
                .respond(request(
                    "PUT",
                    &format!("{upload}?digest=sha256:bad"),
                    b"second"
                ))
                .status,
            "400 Bad Request"
        );
        let digest = hash(b"firstsecond");
        assert_eq!(
            state
                .respond(request(
                    "PUT",
                    &format!("{upload}?digest={digest}"),
                    b"second"
                ))
                .status,
            "201 Created"
        );
        let stored = state.respond(request(
            "GET",
            &format!("/v2/test/bytes/blobs/{digest}"),
            b"",
        ));
        assert_eq!(stored.status, "200 OK");
        assert_eq!(stored.body.as_deref(), Some(b"firstsecond".as_slice()));
        Ok(())
    }
}
