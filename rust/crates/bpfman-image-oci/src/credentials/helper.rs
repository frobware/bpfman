//! Synchronous Docker credential-helper protocol with bounded process custody.
use super::Credentials;
use crate::{Error, registry::check};
use serde::Deserialize;
use std::{
    io::{self, Read, Write},
    net::Shutdown,
    os::{fd::OwnedFd, unix::net::UnixStream},
    process::{Child, Command, Stdio},
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);
const NOT_FOUND: &[u8] = b"credentials not found in native keychain";

pub(super) fn get(
    suffix: &str,
    registry: &str,
    flag: &AtomicBool,
) -> Result<Option<Credentials>, Error> {
    // Config supplies a suffix, never an executable path or shell command.
    if suffix.is_empty()
        || suffix.len() > 128
        || !suffix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    {
        return Err(Error::invalid("invalid registry credential helper suffix"));
    }
    let mut command = Command::new(format!("docker-credential-{suffix}"));
    command.arg("get");
    let output = execute(command, registry, flag, TIMEOUT)?;
    decode(&output)
}

fn decode(output: &[u8]) -> Result<Option<Credentials>, Error> {
    #[derive(Deserialize)]
    struct Response {
        #[serde(default, rename = "Username")]
        username: String,
        #[serde(default, rename = "Secret")]
        secret: String,
    }

    // Parser diagnostics may contain the secret, including on type errors.
    let object: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(output)
        .map_err(|_| Error::invalid("invalid registry credential helper response"))?;
    let response: Response = serde_json::from_value(serde_json::Value::Object(object))
        .map_err(|_| Error::invalid("invalid registry credential helper response"))?;
    if response.username == "<token>" {
        return Err(Error::invalid(
            "registry identity-token exchange is unsupported; use BPFMAN_REGISTRY_AUTH",
        ));
    }
    if response.username.is_empty() && response.secret.is_empty() {
        return Ok(None);
    }
    Credentials::new(&response.username, &response.secret).map(Some)
}

fn channel() -> Result<(UnixStream, Stdio), Error> {
    let (parent, child) = UnixStream::pair()
        .map_err(|_| Error::invalid("create registry credential helper channel"))?;
    parent
        .set_nonblocking(true)
        .map_err(|_| Error::invalid("configure registry credential helper channel"))?;
    // Separate socket endpoints keep the child's standard I/O blocking while
    // allowing the caller to service all three streams without reader threads,
    // unsafe code or a new syscall dependency. OwnedFd preserves CLOEXEC custody.
    Ok((parent, Stdio::from(OwnedFd::from(child))))
}

fn execute(
    mut command: Command,
    registry: &str,
    flag: &AtomicBool,
    timeout: Duration,
) -> Result<Vec<u8>, Error> {
    check(flag)?;
    if registry.is_empty() || registry.len() > 2048 || registry.contains(['\n', '\r', '\0']) {
        return Err(Error::invalid("invalid registry credential helper input"));
    }
    let started = Instant::now();
    let (mut input, stdin) = channel()?;
    let (mut output, stdout) = channel()?;
    let (mut errors, stderr) = channel()?;
    let mut child = Reap {
        child: command
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .map_err(|_| Error::invalid("start registry credential helper"))?,
        reaped: false,
    };
    // Command owns copies of the child endpoints until dropped. Close them now
    // so EOF reflects the helper's descriptors rather than parent-held copies.
    drop(command);
    let mut sent = 0;
    let mut bytes = Vec::new();
    let mut error_bytes = 0;
    let mut output_eof = false;
    let mut errors_eof = false;
    let mut status = None;
    loop {
        check(flag)?;
        if started.elapsed() >= timeout {
            return Err(Error::invalid("registry credential helper timed out"));
        }
        let mut progress = false;
        if sent < registry.len() {
            match input.write(&registry.as_bytes()[sent..]) {
                Ok(0) => return Err(Error::invalid("write registry credential helper input")),
                Ok(count) => {
                    sent += count;
                    progress = true;
                    if sent == registry.len() {
                        input.shutdown(Shutdown::Write).map_err(|_| {
                            Error::invalid("close registry credential helper input")
                        })?;
                    }
                }
                Err(e) if transient(&e) => {}
                Err(_) => return Err(Error::invalid("write registry credential helper input")),
            }
        }
        progress |= read(&mut output, &mut output_eof, bytes.len(), |chunk| {
            bytes.extend_from_slice(chunk);
        })?;
        // Drain stderr concurrently, but never retain, inherit or log it.
        progress |= read(&mut errors, &mut errors_eof, error_bytes, |chunk| {
            error_bytes += chunk.len();
        })?;
        if status.is_none() {
            status = child
                .child
                .try_wait()
                .map_err(|_| Error::invalid("wait for registry credential helper"))?;
            child.reaped = status.is_some();
        }
        if let Some(status) = status.filter(|_| output_eof && errors_eof) {
            check(flag)?;
            if status.success() {
                return Ok(bytes);
            }
            // Docker's protocol uses this exact stdout message on nonzero exit.
            // A selected helper never falls back to inline or another store.
            if bytes.trim_ascii() == NOT_FOUND {
                return Ok(b"{}".to_vec());
            }
            return Err(Error::invalid("registry credential helper failed"));
        }
        if !progress {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

fn read(
    stream: &mut UnixStream,
    eof: &mut bool,
    received: usize,
    mut consume: impl FnMut(&[u8]),
) -> Result<bool, Error> {
    if *eof {
        return Ok(false);
    }
    let mut chunk = [0; 8192];
    match stream.read(&mut chunk) {
        Ok(0) => {
            *eof = true;
            Ok(true)
        }
        Ok(count) => {
            if received.saturating_add(count) > OUTPUT_LIMIT {
                return Err(Error::invalid(
                    "registry credential helper output exceeds 64 KiB",
                ));
            }
            consume(&chunk[..count]);
            Ok(true)
        }
        Err(e) if transient(&e) => Ok(false),
        Err(_) => Err(Error::invalid("read registry credential helper output")),
    }
}

fn transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

struct Reap {
    child: Child,
    reaped: bool,
}

impl Drop for Reap {
    fn drop(&mut self) {
        if !self.reaped {
            // Cancellation and every failure close channels and kill/wait the
            // directly spawned child. No detached reader threads survive EOF
            // held open by descendants. Never expose helper stderr in errors.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests;
