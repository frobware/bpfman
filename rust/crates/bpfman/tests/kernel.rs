//! Privileged outside-in tests. Run with the Make targets in a private mount namespace.
//! Only this composition point selects a store; generic scenarios know no storage format.
#![allow(clippy::expect_used)]
#[path = "kernel/attached.rs"]
mod attached;
#[path = "kernel/cancellation.rs"]
mod cancellation;
#[path = "kernel/cli.rs"]
mod cli;
#[path = "kernel/faults.rs"]
mod faults;
#[path = "kernel/go_compatibility.rs"]
mod go_compatibility;
#[path = "kernel/lifecycle.rs"]
mod lifecycle;
#[path = "kernel/links.rs"]
mod links;
#[path = "kernel/pending.rs"]
mod pending;
#[path = "kernel/support.rs"]
mod support;

#[test]
fn sqlite_lifecycle_store_failures() {
    lifecycle::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn json_lifecycle_store_failures() {
    lifecycle::exercise(bpfman_store_json::Backend);
}

#[test]
fn sqlite_cli_behaviour() {
    cli::behaviour("sqlite");
}

#[test]
fn json_cli_behaviour() {
    cli::behaviour("json");
}

#[test]
fn sqlite_go_interoperability() {
    go_compatibility::exercise();
}

#[test]
fn sqlite_unchanged_tracepoint_dsl() {
    cli::dsl("sqlite", "TestTracepoint_LoadAndGet");
}

#[test]
fn json_unchanged_tracepoint_dsl() {
    cli::dsl("json", "TestTracepoint_LoadAndGet");
}

#[test]
fn sqlite_cancellation_boundaries() {
    cancellation::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn json_cancellation_boundaries() {
    cancellation::exercise(bpfman_store_json::Backend);
}

#[test]
fn sqlite_tracepoint_attachment_lifecycle() {
    links::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn json_tracepoint_attachment_lifecycle() {
    links::exercise(bpfman_store_json::Backend);
}

#[test]
fn sqlite_unchanged_tracepoint_dsl_link_round_trip() {
    cli::dsl("sqlite", "TestTracepoint_LinkRoundTrip");
}

#[test]
fn json_unchanged_tracepoint_dsl_link_round_trip() {
    cli::dsl("json", "TestTracepoint_LinkRoundTrip");
}

#[test]
fn sqlite_attached_unload_failures_and_cancellation() {
    attached::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn json_attached_unload_failures_and_cancellation() {
    attached::exercise(bpfman_store_json::Backend);
}

#[test]
fn sqlite_unchanged_tracepoint_dsl_unload_attached() {
    cli::dsl("sqlite", "TestTracepoint_UnloadAttached");
}

#[test]
fn json_unchanged_tracepoint_dsl_unload_attached() {
    cli::dsl("json", "TestTracepoint_UnloadAttached");
}

#[test]
fn go_tracepoint_dsl_unload_attached() {
    cli::go_dsl("TestTracepoint_UnloadAttached");
}

#[test]
fn sqlite_unload_pending_pin_failures_and_retries() {
    pending::pinned(bpfman_store_sqlite::Backend);
}

#[test]
fn json_unload_pending_pin_failures_and_retries() {
    pending::pinned(bpfman_store_json::Backend);
}

#[test]
fn sqlite_unload_pending_intent_without_pin() {
    pending::unpinned(bpfman_store_sqlite::Backend);
}

#[test]
fn json_unload_pending_intent_without_pin() {
    pending::unpinned(bpfman_store_json::Backend);
}
