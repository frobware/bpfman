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
    ("bpfman-store-sqlite", 3),
    ("bpfman-runtime", 4),
    ("bpfman", 5),
];
const PURE: &[&str] = &["bpfman-model", "bpfman-core"];
// thiserror's no_std runtime support plus its host-side derive dependency
// closure. These expand core::fmt/core::error implementations, not effects.
const PURE_EXTERNAL: &[&str] = &[
    "thiserror",
    "thiserror-impl",
    "proc-macro2",
    "quote",
    "syn",
    "unicode-ident",
];

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
                "rusqlite" | "libsqlite3-sys" => assert_eq!(
                    name, "bpfman-store-sqlite",
                    "SQL belongs inside the store adapter"
                ),
                "clap" | "anyhow" => assert_eq!(
                    name, "bpfman",
                    "CLI parsing and general reports belong in the binary"
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
