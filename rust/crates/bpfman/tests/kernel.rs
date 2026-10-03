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
fn lifecycle_store_failures() {
    lifecycle::exercise(bpfman_store_sqlite::Backend);
}

#[test]
#[ignore = "requires BPF privileges and a private mount namespace; make rust-test-kernel-load"]
fn cli_behaviour() {
    cli::behaviour();
}

#[test]
#[ignore = "requires BPF privileges and a private mount namespace; make rust-test-kernel-load"]
fn sqlite_go_interoperability() {
    go_compatibility::exercise();
}

#[test]
#[ignore = "requires BPF privileges and a private mount namespace; make rust-test-observation"]
fn unchanged_tracepoint_dsl() {
    cli::dsl();
}
