//! Executable dependency boundaries for the independent Rust workspace.

// Metadata failures should identify the broken contract directly in tests.
#![allow(clippy::expect_used)]

use std::{collections::HashSet, path::Path, process::Command, sync::OnceLock};

use serde_json::Value;

const TIERS: &[(&str, u64)] = &[
    ("bpfman-model", 0),
    ("bpfman-core", 1),
    ("bpfman-lock", 1),
    ("bpfman-fs", 2),
    ("bpfman-kernel", 2),
    ("bpfman-store", 3),
    ("bpfman-store-sqlite", 4),
    ("bpfman-store-json", 4),
    ("bpfman-runtime", 4),
    ("bpfman", 5),
];
const PURE: &[&str] = &["bpfman-model", "bpfman-core"];

// No external normal dependencies are admitted to the pure crates. In
// particular Aya's std-enabled thiserror must not unify into their closure.
const PURE_EXTERNAL: &[&str] = &[];

fn metadata() -> &'static Value {
    static METADATA: OnceLock<Value> = OnceLock::new();
    METADATA.get_or_init(|| {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
        let output = Command::new(env!("CARGO"))
            .args([
                "metadata",
                "--format-version",
                "1",
                "--locked",
                "--all-features",
                "--manifest-path",
            ])
            .arg(manifest)
            .output()
            .expect("run metadata for rust/Cargo.toml");

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("decode Cargo metadata")
    })
}

fn array(value: &Value) -> &[Value] {
    value.as_array().expect("metadata array")
}

fn string(value: &Value) -> &str {
    value.as_str().expect("metadata string")
}

fn package<'a>(meta: &'a Value, id: &Value) -> &'a Value {
    array(&meta["packages"])
        .iter()
        .find(|p| p["id"] == *id)
        .expect("resolved package")
}

fn normal_dependencies<'a>(meta: &'a Value, id: &Value) -> Vec<&'a Value> {
    let node = array(&meta["resolve"]["nodes"])
        .iter()
        .find(|n| n["id"] == *id)
        .expect("resolved node");
    array(&node["deps"])
        .iter()
        .filter(|d| array(&d["dep_kinds"]).iter().any(|k| k["kind"].is_null()))
        .map(|d| &d["pkg"])
        .collect()
}

fn tier(name: &str) -> u64 {
    TIERS
        .iter()
        .find(|(n, _)| *n == name)
        .expect("assign every workspace member a tier")
        .1
}

#[test]
fn workspace_members_are_registered_and_documented() {
    let meta = metadata();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root");

    assert_eq!(Path::new(string(&meta["workspace_root"])), root);

    let names: HashSet<_> = array(&meta["workspace_members"])
        .iter()
        .map(|id| string(&package(meta, id)["name"]))
        .collect();

    assert_eq!(names, TIERS.iter().map(|(name, _)| *name).collect());

    let readme = std::fs::read_to_string(root.join("README.md")).expect("workspace README");
    let documented: HashSet<_> = readme
        .lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|line| line.split('`').next())
        .collect();

    assert_eq!(
        names, documented,
        "document each implemented crate exactly once in the registry"
    );

    for &(name, tier) in TIERS {
        assert!(
            readme.contains(&format!("| `{name}` | {tier} |")),
            "README tier for {name} must match the enforced tier {tier}"
        );
    }
}

#[test]
fn no_local_dependency_can_reach_legacy_rust() {
    let meta = metadata();
    let root = Path::new(string(&meta["workspace_root"]))
        .canonicalize()
        .expect("workspace root");

    for p in array(&meta["packages"]) {
        if p["source"].is_null() {
            let manifest = Path::new(string(&p["manifest_path"]))
                .canonicalize()
                .expect("local manifest");

            assert!(
                manifest.starts_with(&root),
                "local package outside rust/: {}",
                manifest.display()
            );
        }

        // Also inspect declared paths: this includes optional, target-specific,
        // build and development dependencies, even if not compiled here.
        for dep in array(&p["dependencies"]) {
            if let Some(path) = dep["path"].as_str() {
                let path = Path::new(path).canonicalize().expect("dependency path");

                assert!(
                    path.starts_with(&root),
                    "dependency outside rust/: {}",
                    path.display()
                );
            }
        }
    }
}

#[test]
fn normal_workspace_edges_point_strictly_downward() {
    let meta = metadata();
    let members = array(&meta["workspace_members"]);

    for id in members {
        let from = string(&package(meta, id)["name"]);

        for dep in normal_dependencies(meta, id) {
            if members.contains(dep) {
                let to = string(&package(meta, dep)["name"]);

                assert!(tier(from) > tier(to), "{from} must not depend on {to}");
            }
        }
    }
}

#[test]
fn runtime_and_store_contract_cannot_reach_a_persistence_backend() {
    let meta = metadata();

    for name in ["bpfman-runtime", "bpfman-store"] {
        let start = array(&meta["workspace_members"])
            .iter()
            .find(|id| package(meta, id)["name"] == name)
            .expect("member");
        let mut pending = vec![start];
        let mut seen = HashSet::new();

        while let Some(id) = pending.pop() {
            if !seen.insert(string(id)) {
                continue;
            }

            let dependency = string(&package(meta, id)["name"]);

            assert!(
                !matches!(
                    dependency,
                    "bpfman-store-sqlite" | "bpfman-store-json" | "rusqlite" | "libsqlite3-sys"
                ),
                "{name} reaches concrete persistence backend {dependency}"
            );
            pending.extend(normal_dependencies(meta, id));
        }
    }
}

#[test]
fn backend_and_frontend_dependencies_stay_at_their_boundaries() {
    let meta = metadata();

    for id in array(&meta["workspace_members"]) {
        let name = string(&package(meta, id)["name"]);

        for dep in normal_dependencies(meta, id) {
            let dependency = string(&package(meta, dep)["name"]);

            match dependency {
                "rustix" => assert!(
                    matches!(name, "bpfman-lock" | "bpfman-fs"),
                    "filesystem and lock syscalls belong in their adapters"
                ),
                "aya" => assert!(
                    matches!(name, "bpfman-fs" | "bpfman-runtime"),
                    "Aya is confined to pin I/O and the private kernel adapter"
                ),
                "aya-obj" => assert!(
                    matches!(name, "bpfman-runtime" | "bpfman-kernel"),
                    "ELF parsing and generated BPF ABI layouts belong in kernel adapters"
                ),
                "rusqlite" | "libsqlite3-sys" => assert_eq!(
                    name, "bpfman-store-sqlite",
                    "SQL belongs inside the store adapter"
                ),
                "bpfman-store-sqlite" | "bpfman-store-json" => assert_eq!(
                    name, "bpfman",
                    "only the composition root selects a persistence backend"
                ),
                "clap" | "anyhow" | "tracing-subscriber" | "tracing-chrome" => assert_eq!(
                    name, "bpfman",
                    "CLI parsing, reports, and telemetry collection belong in the binary"
                ),
                _ => {}
            }
        }
    }
}

#[test]
fn pure_closures_have_only_reviewed_dependencies_and_features() {
    let meta = metadata();

    for name in PURE {
        let start = array(&meta["workspace_members"])
            .iter()
            .find(|id| package(meta, id)["name"] == *name)
            .expect("pure workspace member");
        let features = package(meta, start)["features"]
            .as_object()
            .expect("declared features");

        assert!(
            features.is_empty(),
            "{name}: review new features before admitting them"
        );

        let mut pending = vec![start];
        let mut seen = HashSet::new();

        while let Some(id) = pending.pop() {
            if !seen.insert(string(id)) {
                continue;
            }

            let dep = string(&package(meta, id)["name"]);

            assert!(
                PURE.contains(&dep) || PURE_EXTERNAL.contains(&dep),
                "{name} reaches unreviewed {dep}"
            );

            if dep == "thiserror" {
                let node = array(&meta["resolve"]["nodes"])
                    .iter()
                    .find(|node| node["id"] == *id)
                    .expect("resolved thiserror node");

                assert!(
                    !array(&node["features"])
                        .iter()
                        .any(|feature| feature == "std"),
                    "pure errors must not enable thiserror's std feature"
                );
            }

            pending.extend(normal_dependencies(meta, id));
        }
    }
}

#[test]
fn syscall_exception_keeps_all_other_workspace_lint_gates() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workspace = std::fs::read_to_string(root.join("Cargo.toml")).expect("workspace");
    let kernel = std::fs::read_to_string(root.join("crates/bpfman-kernel/Cargo.toml"))
        .expect("kernel manifest");
    let section = |text: &str, header: &str| -> Vec<String> {
        text.split_once(header)
            .expect("lint section")
            .1
            .lines()
            .take_while(|line| !line.starts_with('['))
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_owned)
            .collect()
    };

    let filesystem = std::fs::read_to_string(root.join("crates/bpfman-fs/Cargo.toml"))
        .expect("filesystem manifest");
    for manifest in [&kernel, &filesystem] {
        for category in ["rust", "clippy"] {
            let expected = section(&workspace, &format!("[workspace.lints.{category}]"));
            let actual = section(manifest, &format!("[lints.{category}]"));
            let expected: Vec<_> = expected
                .into_iter()
                .map(|line| {
                    if line == "unsafe_code = \"forbid\"" {
                        "unsafe_code = \"deny\"".into()
                    } else {
                        line
                    }
                })
                .collect();

            assert_eq!(
                actual, expected,
                "syscall adapters must retain all other workspace gates"
            );
        }
    }

    let syscall = std::fs::read_to_string(root.join("crates/bpfman-kernel/src/syscall.rs"))
        .expect("syscall boundary");

    assert!(syscall.contains("#![allow(unsafe_code)]"));

    for module in ["lib.rs", "observe.rs"] {
        let source = std::fs::read_to_string(root.join("crates/bpfman-kernel/src").join(module))
            .expect("safe observation module");

        assert!(!source.contains("allow(unsafe_code)"));
    }
}

#[test]
fn filesystem_unsafe_is_confined_to_xdp_syscall_boundary() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../bpfman-fs/src");
    let boundary = std::fs::read_to_string(root.join("xdp/syscall.rs")).expect("syscalls");
    assert!(boundary.contains("#![allow(unsafe_code)]"));
    for file in [
        "lib.rs",
        "artifacts.rs",
        "directory.rs",
        "error.rs",
        "layout.rs",
        "link.rs",
        "observe.rs",
        "removal.rs",
        "snapshot.rs",
        "xdp.rs",
    ] {
        let source = std::fs::read_to_string(root.join(file)).expect("safe filesystem module");
        assert!(!source.contains("allow(unsafe_code)"), "{file}");
    }
}
