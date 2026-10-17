//! OCI inputs feed the same native lifecycle, stored observations and teardown.
#[path = "../../../../tests/support/oci.rs"]
mod registry;

use super::support::*;
use std::{fs, sync::atomic::Ordering};

pub(super) fn exercise() {
    let c = Context::new();
    let parent = c.layout.root().parent().expect("parent");
    let cache = parent.join("image-cache");
    let config = parent.join("image.toml");
    fs::write(&config, "[signing]\nverify_enabled = false\n").expect("config");
    let server = registry::Registry::new(fs::read(fixture("xdp_pass.bpf.o")).expect("ELF"), true);
    server.bearer.store(true, Ordering::Relaxed);
    let execute = |args: &[&str], success| {
        let output = c
            .command(&rust(), args)
            .arg("--image-cache-dir")
            .arg(&cache)
            .arg("--config")
            .arg(&config)
            .env_remove("BPFMAN_REGISTRY_AUTH")
            .output()
            .expect("CLI");
        assert_eq!(
            output.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    let inspected = execute(&["image", "inspect", &server.reference], true);
    let inspected: serde_json::Value =
        serde_json::from_slice(&inspected.stdout).expect("inspection");
    assert_eq!(inspected["manifests"][0]["digest"], server.digest);
    assert!(!c.layout.root().exists());
    assert!(!cache.exists());

    // A valid BPF header with a truncated ELF must never enter the cache.
    let invalid = registry::Registry::new(
        fs::read(fixture("xdp_pass.bpf.o")).expect("ELF")[..64].to_vec(),
        false,
    );
    execute(
        &[
            "program",
            "load",
            "image",
            &invalid.reference,
            "--pull-policy",
            "Always",
            "--programs",
            "xdp:pass",
        ],
        false,
    );
    assert!(!cache.exists());
    assert!(!c.layout.root().exists());
    drop(invalid);

    for policy in ["Always", "Never", "IfNotPresent"] {
        let requests = server.requests.load(Ordering::Relaxed);
        let output = execute(
            &[
                "program",
                "load",
                "image",
                &server.reference,
                "--pull-policy",
                policy,
                "--programs",
                "xdp:pass",
                "-o",
                "json",
            ],
            true,
        );
        let loaded: serde_json::Value = serde_json::from_slice(&output.stdout).expect("loaded");
        let program = &loaded["programs"][0];
        let id = program["record"]["program_id"]
            .as_u64()
            .expect("program id")
            .to_string();
        assert_eq!(
            program["record"]["load"]["image_source"]["url"],
            server.reference
        );
        assert_eq!(
            program["record"]["load"]["image_source"]["digest"],
            server.digest
        );
        assert_eq!(
            program["record"]["load"]["image_source"]["pull_policy"],
            policy
        );
        let reopened = c.json(&rust(), &["program", "get", &id, "-o", "json"]);
        assert_eq!(
            reopened["record"]["load"]["image_source"],
            program["record"]["load"]["image_source"]
        );
        // The unchanged Go reader must decode Rust's image-source row.
        let go = std::env::var_os("BPFMAN_GO_BIN")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| repository().join("bin/bpfman"));
        let observed = c.json(&go, &["program", "get", &id, "-o", "json"]);
        assert_eq!(
            observed["record"]["load"]["image_source"],
            program["record"]["load"]["image_source"]
        );
        execute(&["program", "unload", &id], true);
        if policy == "Always" {
            server.responses.lock().expect("routes").clear();
        }
        c.no_artifacts();
        if policy != "Always" {
            assert_eq!(server.requests.load(Ordering::Relaxed), requests);
        }
    }
    assert_eq!(
        c.json(&rust(), &["program", "list", "-o", "json"])["programs"],
        serde_json::json!([])
    );
    assert!(
        fs::read_dir(cache.join("images-v1"))
            .expect("cache")
            .all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with('.'))
    );
}

pub(super) fn failures() {
    use super::faults::{Faults, Point};
    use bpfman_model::{ImagePullPolicy, ProgramSource, ProgramSpec};
    use bpfman_runtime::{ActiveStore, Bpfman, Cancellation, LoadErrorKind, PreparedProgram};
    let c = Context::new();
    let store = Faults::new(bpfman_store_sqlite::Backend);
    let app = Bpfman::new(
        ActiveStore::open(store.clone(), &c.layout, TIMEOUT).expect("store"),
        bpfman_kernel_aya::Kernel::default(),
        TIMEOUT,
    );
    let request = || {
        PreparedProgram::from_image(
            &bpfman_kernel_aya::Kernel::default(),
            fs::read(fixture("xdp_pass.bpf.o")).expect("ELF"),
            ProgramSource::Image {
                url: "quay.io/example/bytecode:latest".into(),
                digest: format!("sha256:{}", "a".repeat(64)),
                pull_policy: ImagePullPolicy::Never,
            },
            ProgramSpec::Xdp("pass".try_into().expect("name")),
            Default::default(),
            &Cancellation::new(),
        )
        .expect("prepare image")
    };
    store.set(Some(Point::Commit));
    let error = app.load(request()).expect_err("commit failure");
    assert_eq!(error.unresolved(), 0);
    assert!(app.list(&Default::default()).expect("list").is_empty());
    c.no_artifacts();
    store.set(None);
    let cancellation = Cancellation::new();
    store.cancel_at(Point::Validate, &cancellation);
    let error = app
        .load_with_cancellation(request(), &cancellation)
        .expect_err("cancelled");
    assert_eq!(error.kind(), LoadErrorKind::Cancelled);
    assert_eq!(error.unresolved(), 0);
    assert!(app.list(&Default::default()).expect("list").is_empty());
    c.no_artifacts();
}

pub(super) fn build_interoperability() {
    #[path = "../../../../tests/support/publish_registry.rs"]
    mod publisher;
    let c = Context::new();
    let server = publisher::Registry::new();
    let parent = c.layout.root().parent().expect("parent");
    let config = parent.join("image-build.toml");
    let cache = parent.join("build-cache");
    fs::write(&config, "[signing]\nverify_enabled = false\n").expect("config");
    let execute = |binary: &std::path::Path, args: &[&str]| {
        let output = c
            .command(binary, args)
            .arg("--config")
            .arg(&config)
            .arg("--image-cache-dir")
            .arg(&cache)
            .env_remove("BPFMAN_REGISTRY_AUTH")
            .output()
            .expect("CLI");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    };
    execute(
        &rust(),
        &[
            "image",
            "build",
            &server.reference,
            fixture("xdp_pass.bpf.o").to_str().expect("path"),
        ],
    );
    assert!(!c.layout.root().exists());
    assert!(!cache.exists());
    let go = std::env::var_os("BPFMAN_GO_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| repository().join("bin/bpfman"));
    for binary in [&go, &rust()] {
        let loaded = execute(
            binary,
            &[
                "program",
                "load",
                "image",
                &server.reference,
                "--pull-policy",
                "Always",
                "--programs",
                "xdp:pass",
                "-o",
                "json",
            ],
        );
        let loaded: serde_json::Value =
            serde_json::from_slice(&loaded.stdout).expect("load report");
        let id = loaded["programs"][0]["record"]["program_id"]
            .as_u64()
            .expect("program id")
            .to_string();
        assert_eq!(
            loaded["programs"][0]["record"]["load"]["image_source"]["url"],
            server.reference
        );
        execute(binary, &["program", "unload", &id]);
        c.no_artifacts();
    }
    assert!(server.state.lock().expect("state").uploads.is_empty());
    assert!(!server.cancelled.load(Ordering::Relaxed));
}
