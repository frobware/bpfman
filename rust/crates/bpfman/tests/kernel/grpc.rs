//! Stock Go protobuf client against the generic synchronous Rust service host.
use crate::oci_registry as registry;

use super::support::*;
use bpfman_api::{Images, Server, ServerOptions, Service, Status};
use bpfman_fs::RuntimeLayout;
use bpfman_runtime::{ActiveStore, Bpfman, Cancellation};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::{
    fs,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

struct ImagePolicy {
    cache: RuntimeLayout,
}
impl Images for ImagePolicy {
    fn pull(
        &self,
        image: &bpfman_proto::v1::BytecodeImage,
        cancellation: &Cancellation,
        validate: &dyn Fn(&[u8]) -> Result<(), bpfman_kernel::Error>,
    ) -> Result<(Vec<u8>, bpfman_model::ProgramSource), Status> {
        let reference = bpfman_image_oci::Reference::parse(&image.url)
            .map_err(|e| Status::invalid_argument(e.to_string()))?;
        let policy = match image.image_pull_policy {
            0 => bpfman_model::ImagePullPolicy::Always,
            1 => bpfman_model::ImagePullPolicy::IfNotPresent,
            2 => bpfman_model::ImagePullPolicy::Never,
            _ => return Err(Status::invalid_argument("invalid policy")),
        };
        let auth = match (image.username.as_deref(), image.password.as_deref()) {
            (Some(u), Some(p)) if !u.is_empty() && !p.is_empty() => Some(
                bpfman_image_oci::Credentials::new(u, p)
                    .map_err(|e| Status::invalid_argument(e.to_string()))?,
            ),
            _ => None,
        };
        let pulled = bpfman_image_oci::pull(
            &reference,
            auth.as_ref(),
            &bpfman_image_oci::SigningPolicy::disabled(),
            policy,
            &self.cache,
            TIMEOUT,
            cancellation.flag(),
            |bytes| {
                bpfman_kernel_aya::inspect_bytecode(bytes, cfg!(target_endian = "little"))?;
                validate(bytes)
            },
        )
        .map_err(|e| Status::internal(e.to_string()))?;
        let source = bpfman_model::ProgramSource::Image {
            url: image.url.clone(),
            digest: pulled.digest().into(),
            pull_policy: policy,
        };
        Ok((pulled.bytes().to_vec(), source))
    }
}

pub(super) fn exercise() {
    let c = Context::new();
    let parent = c.layout.root().parent().expect("parent");
    let socket = parent.join("grpc.sock");
    let cache = RuntimeLayout::try_from(parent.join("image-cache")).expect("cache");
    let store = ActiveStore::open(bpfman_store_sqlite::Backend, &c.layout, TIMEOUT).expect("store");
    let service = Arc::new(Service::new(
        Bpfman::new(store, bpfman_kernel_aya::Kernel::default(), Duration::ZERO),
        bpfman_kernel_aya::Kernel::default(),
        ImagePolicy { cache },
        Default::default(),
    ));
    let server = Server::bind(&socket, service.clone(), ServerOptions::default()).expect("server");
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let host = std::thread::spawn(move || server.serve(&stopped));
    let registry = registry::Registry::new(
        fs::read(fixture("xdp_pass.bpf.o")).expect("bytecode"),
        false,
    );
    let client = repository().join("bin/grpc-contract");
    let network = super::xdp_attach::Interface::new();
    let interface = network.name();
    let libc = fs::read_to_string("/proc/self/maps")
        .expect("maps")
        .lines()
        .find_map(|line| {
            line.split_whitespace()
                .last()
                .filter(|s| s.ends_with("/libc.so.6"))
        })
        .expect("libc")
        .to_owned();
    let mut results = Vec::new();
    for lane in [
        "lifecycle",
        "families",
        "cancellation",
        "image",
        "persist-create",
    ] {
        let output = Command::new("timeout")
            .arg("60s")
            .arg(&client)
            .arg("-socket")
            .arg(&socket)
            .arg("-objects")
            .arg(fixture("."))
            .arg("-runtime")
            .arg(c.layout.root())
            .arg("-server-pid")
            .arg(std::process::id().to_string())
            .arg("-image")
            .arg(&registry.reference)
            .arg("-interface")
            .arg(interface.as_str())
            .arg("-uprobe-target")
            .arg(&libc)
            .arg("-lane")
            .arg(lane)
            .output()
            .expect("Go protobuf client");
        let passed = output.status.success();
        results.push((lane, output));
        if !passed {
            break;
        }
    }
    stop.store(true, Ordering::Release);
    host.join().expect("host thread").expect("host shutdown");
    for (lane, output) in results {
        assert!(
            output.status.success(),
            "{lane}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert!(
        service.take_recovery().is_empty(),
        "unexpected cleanup receipts"
    );
    drop(service);
    let reopened = Arc::new(Service::new(
        Bpfman::new(
            ActiveStore::open(bpfman_store_sqlite::Backend, &c.layout, TIMEOUT)
                .expect("reopen store"),
            bpfman_kernel_aya::Kernel::default(),
            Duration::ZERO,
        ),
        bpfman_kernel_aya::Kernel::default(),
        ImagePolicy {
            cache: RuntimeLayout::try_from(parent.join("image-cache")).expect("cache"),
        },
        Default::default(),
    ));
    let reopened_server =
        Server::bind(&socket, reopened.clone(), ServerOptions::default()).expect("reopened host");
    stop.store(false, Ordering::Release);
    let reopened_stop = stop.clone();
    let host = std::thread::spawn(move || reopened_server.serve(&reopened_stop));
    let output = Command::new("timeout")
        .arg("60s")
        .arg(&client)
        .arg("-socket")
        .arg(&socket)
        .arg("-runtime")
        .arg(c.layout.root())
        .arg("-server-pid")
        .arg(std::process::id().to_string())
        .arg("-lane")
        .arg("persist-read")
        .output()
        .expect("reopened client");
    stop.store(true, Ordering::Release);
    host.join()
        .expect("reopened thread")
        .expect("reopened shutdown");
    assert!(
        output.status.success(),
        "reopened cleanup: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(reopened.take_recovery().is_empty());
    c.no_artifacts();
    // The same client and assertions run against the unchanged Go server.
    let config = parent.join("go-grpc.toml");
    fs::write(&config, "[signing]\nverify_enabled = false\n").expect("Go config");
    let go = std::env::var_os("BPFMAN_GO_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repository().join("bin/bpfman"));
    let log = tempfile::tempfile().expect("daemon log");
    let child = Command::new(&go)
        .arg("--runtime-dir")
        .arg(c.layout.root())
        .args(["serve", "--socket-path", socket.to_str().expect("socket")])
        .arg("--config")
        .arg(&config)
        .arg("--image-cache-dir")
        .arg(parent.join("go-image-cache"))
        .stdout(Stdio::from(log.try_clone().expect("log")))
        .stderr(Stdio::from(log))
        .spawn()
        .expect("Go daemon");
    let mut daemon = Daemon(child);
    let started = Instant::now();
    while !socket.exists() {
        assert!(
            daemon.0.try_wait().expect("daemon status").is_none(),
            "Go daemon exited"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "Go socket readiness"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    for lane in [
        "lifecycle",
        "families",
        "cancellation",
        "image",
        "persist-create",
        "persist-read",
    ] {
        let output = Command::new("timeout")
            .arg("60s")
            .arg(&client)
            .arg("-socket")
            .arg(&socket)
            .arg("-objects")
            .arg(fixture("."))
            .arg("-runtime")
            .arg(c.layout.root())
            .arg("-server-pid")
            .arg(daemon.0.id().to_string())
            .arg("-interface")
            .arg(interface.as_str())
            .arg("-uprobe-target")
            .arg(&libc)
            .arg("-image")
            .arg(&registry.reference)
            .arg("-lane")
            .arg(lane)
            .output()
            .expect("Go comparison client");
        assert!(
            output.status.success(),
            "Go {lane}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let _ = Command::new("kill")
        .arg("-TERM")
        .arg(daemon.0.id().to_string())
        .status();
    let _ = daemon.0.wait();
    c.no_artifacts();
    assert!(!socket.exists(), "socket cleanup after joined shutdown");
}

struct Daemon(std::process::Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
