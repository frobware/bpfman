//! Process-level evidence for filtered telemetry and reader/writer lock scopes.
#![allow(clippy::expect_used)]

use bpfman_fs::{RuntimeDirectory, RuntimeLayout};
use bpfman_lock::AcquireOptions;
use serde_json::Value;
use std::{
    path::Path,
    process::{Command, Output},
    time::Duration,
};

fn run(root: &Path, trace: &Path, filter: &str, args: &[&str]) -> Output {
    run_backend("json", root, trace, filter, args)
}

fn run_backend(backend: &str, root: &Path, trace: &Path, filter: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_bpfman"))
        .env("RUST_LOG", filter)
        .env_remove("BPFMAN_RUNTIME_DIR")
        .env_remove("BPFMAN_LOCK_TIMEOUT")
        .args(["--store", backend, "--runtime-dir"])
        .arg(root)
        .arg("--trace-file")
        .arg(trace)
        .args(args)
        .output()
        .expect("CLI")
}

fn names(path: &Path) -> Vec<String> {
    let trace: Value = serde_json::from_slice(&std::fs::read(path).expect("trace file"))
        .expect("complete trace, including after a command error");
    trace
        .as_array()
        .expect("trace events")
        .iter()
        .filter_map(|event| event["name"].as_str().map(str::to_owned))
        .collect()
}

#[test]
fn timeline_distinguishes_creation_reader_and_lock_timeout() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("runtime");
    let initial = temp.path().join("initial.json");
    let filter = "bpfman_runtime=debug,bpfman_store_json=debug,bpfman_fs=trace,bpfman_lock=trace";
    let output = run(&root, &initial, filter, &["program", "list", "-q"]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let initial_names = names(&initial);
    for required in [
        "store.open",
        "store.create",
        "snapshot.publish",
        "lock.wait",
        "lock.held",
    ] {
        assert!(
            initial_names.iter().any(|name| name == required),
            "missing {required}"
        );
    }

    let runtime =
        RuntimeDirectory::open_existing(RuntimeLayout::try_from(root.clone()).expect("layout"))
            .expect("runtime")
            .expect("existing");
    runtime
        .with_writer(
            AcquireOptions {
                timeout: Duration::from_secs(1),
                cancelled: None,
            },
            |_| {
                let read_trace = temp.path().join("reader.json");
                let output = run(
                    &root,
                    &read_trace,
                    filter,
                    &["program", "list", "-q", "--lock-timeout", "25ms"],
                );

                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert!(output.stdout.is_empty());
                let read_names = names(&read_trace);
                assert!(read_names.iter().any(|name| name == "store.read_summaries"));
                assert!(!read_names.iter().any(|name| matches!(
                    name.as_str(),
                    "lock.wait" | "lock.held" | "store.create"
                )));

                let full_trace = temp.path().join("full-reader.json");
                let output = run(
                    &root,
                    &full_trace,
                    filter,
                    &["program", "list", "-o", "json", "--lock-timeout", "25ms"],
                );

                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                let value: Value = serde_json::from_slice(&output.stdout).expect("list JSON");
                assert_eq!(value["programs"], serde_json::json!([]));
                assert!(!names(&full_trace).iter().any(|name| matches!(
                    name.as_str(),
                    "lock.wait" | "lock.held" | "store.create"
                )));

                let get_trace = temp.path().join("get-reader.json");
                let output = run(
                    &root,
                    &get_trace,
                    filter,
                    &["program", "get", "42", "--lock-timeout", "25ms"],
                );

                assert_eq!(output.status.code(), Some(1));
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains("program 42 does not exist")
                );
                assert!(!names(&get_trace).iter().any(|name| matches!(
                    name.as_str(),
                    "lock.wait" | "lock.held" | "store.create"
                )));

                let blocked_trace = temp.path().join("blocked.json");
                let output = run(
                    &root,
                    &blocked_trace,
                    filter,
                    &["program", "unload", "42", "--lock-timeout", "25ms"],
                );

                assert_eq!(output.status.code(), Some(1));
                assert!(
                    String::from_utf8_lossy(&output.stderr).contains("timed out waiting for lock")
                );
                let blocked_names = names(&blocked_trace);
                assert!(blocked_names.iter().any(|name| name == "lock.wait"));
                assert!(!blocked_names.iter().any(|name| name == "lock.held"));
            },
        )
        .expect("writer");
}

#[test]
fn rust_log_filters_components_without_changing_command_output() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("runtime");
    let trace = temp.path().join("json-only.json");
    let output = run(
        &root,
        &trace,
        "bpfman_store_json=debug",
        &["program", "list", "-o", "json"],
    );

    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("command JSON");
    assert_eq!(value["programs"], serde_json::json!([]));
    let stderr = String::from_utf8(output.stderr).expect("stderr");
    assert!(!stderr.is_empty());
    for line in stderr.lines() {
        let event: Value = serde_json::from_str(line).expect("structured stderr");
        assert!(
            event["target"]
                .as_str()
                .expect("target")
                .starts_with("bpfman_store_json")
        );
    }
    assert!(!names(&trace).iter().any(|name| name == "lock.wait"));

    let silent = run(
        &root,
        &temp.path().join("off.json"),
        "off",
        &["program", "list", "-o", "json"],
    );
    assert!(silent.status.success());
    assert_eq!(silent.stdout, output.stdout);
    assert!(silent.stderr.is_empty());
}

#[test]
fn existing_trace_destination_is_never_truncated() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("absent");
    let trace = temp.path().join("existing.json");
    std::fs::write(&trace, b"preserve").expect("existing file");
    let output = run(&root, &trace, "debug", &["program", "list"]);

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(std::fs::read(&trace).expect("existing file"), b"preserve");
    assert!(!root.exists());
}

#[test]
fn list_reuses_the_store_handle_opened_at_startup() {
    for backend in ["sqlite", "json"] {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("runtime");

        // Exercise both first-time initialization and an already-existing store.
        for stage in ["initial", "existing"] {
            let trace = temp.path().join(format!("{stage}.json"));
            let output = run_backend(backend, &root, &trace, "debug", &["program", "list", "-q"]);

            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(output.stdout.is_empty());
            let events: Vec<Value> = String::from_utf8(output.stderr)
                .expect("stderr")
                .lines()
                .map(|line| serde_json::from_str(line).expect("telemetry JSON"))
                .collect();
            let opens = |name: &str| {
                events
                    .iter()
                    .filter(|event| {
                        event["fields"]["message"] == "new" && event["span"]["name"] == name
                    })
                    .count()
            };

            assert_eq!(
                opens("store.open_reader"),
                1,
                "{backend} {stage}: no second open for list"
            );
            if backend == "sqlite" {
                assert_eq!(opens("store.connection.open"), 1, "one read connection");
                assert!(
                    events.iter().any(|event| {
                        event["fields"]["message"] == "reader acquired"
                            && event["fields"]["reused"] == true
                    }),
                    "the listing must reuse the startup connection"
                );
            }
        }
    }
}
