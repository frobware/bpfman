//! Privileged outside-in tests. Run with the Make targets in a private mount namespace.
//! Only this composition point selects a store; generic scenarios know no storage format.
#![allow(clippy::expect_used)]
#[path = "kernel/cli.rs"]
mod cli;
#[path = "kernel/faults.rs"]
mod faults;
#[path = "kernel/go_compatibility.rs"]
mod go_compatibility;
#[path = "kernel/lifecycle.rs"]
mod lifecycle;
#[path = "kernel/support.rs"]
mod support;

#[test]
#[ignore = "requires BPF privileges and a private mount namespace; make rust-test-kernel-load"]
fn sqlite_lifecycle_store_failures() {
    lifecycle::exercise(bpfman_store_sqlite::Backend);
}

#[test]
#[ignore = "requires BPF privileges and a private mount namespace; make rust-test-kernel-load"]
fn json_lifecycle_store_failures() {
    lifecycle::exercise(bpfman_store_json::Backend);
}

#[test]
#[ignore = "requires BPF privileges and a private mount namespace; make rust-test-kernel-load"]
fn sqlite_cli_behaviour() {
    cli::behaviour("sqlite");
}

#[test]
#[ignore = "requires BPF privileges and a private mount namespace; make rust-test-kernel-load"]
fn json_cli_behaviour() {
    cli::behaviour("json");
}

#[test]
#[ignore = "requires BPF privileges and a private mount namespace; make rust-test-kernel-load"]
fn sqlite_go_interoperability() {
    go_compatibility::exercise();
}

#[test]
#[ignore = "requires BPF privileges and a private mount namespace; make rust-test-observation"]
fn sqlite_unchanged_tracepoint_dsl() {
    cli::dsl("sqlite");
}

#[test]
#[ignore = "requires BPF privileges and a private mount namespace; make rust-test-observation"]
fn json_unchanged_tracepoint_dsl() {
    cli::dsl("json");
}
