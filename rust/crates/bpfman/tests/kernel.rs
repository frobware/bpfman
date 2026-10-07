//! Privileged outside-in tests. Run with the Make targets in a private mount namespace.
//! Only this composition point selects a store; generic scenarios know no storage format.
#![allow(clippy::expect_used)]
#[path = "kernel/attached.rs"]
mod attached;
#[path = "kernel/batch.rs"]
mod batch;
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

#[test]
fn sqlite_batch_lifecycle() {
    batch::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn json_batch_lifecycle() {
    batch::exercise(bpfman_store_json::Backend);
}

#[test]
fn sqlite_unchanged_tracepoint_dsl_batch() {
    cli::dsl("sqlite", "TestMultiProgTracepoint_LoadAttachDetachUnload");
}

#[test]
fn json_unchanged_tracepoint_dsl_batch() {
    cli::dsl("json", "TestMultiProgTracepoint_LoadAttachDetachUnload");
}

#[test]
fn sqlite_batch_cli() {
    batch::cli("sqlite");
}

#[test]
fn json_batch_cli() {
    batch::cli("json");
}

#[path = "kernel/xdp.rs"]
mod xdp;

#[test]
fn sqlite_xdp_load_lifecycle() {
    xdp::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn json_xdp_load_lifecycle() {
    xdp::exercise(bpfman_store_json::Backend);
}

#[test]
fn sqlite_unchanged_xdp_load_dsl() {
    cli::dsl("sqlite", "TestXDP_LoadAndGet");
    cli::dsl("sqlite", "TestLoad_NamedProgramSkipsBrokenSibling");
}

#[test]
fn json_unchanged_xdp_load_dsl() {
    cli::dsl("json", "TestXDP_LoadAndGet");
    cli::dsl("json", "TestLoad_NamedProgramSkipsBrokenSibling");
}

#[test]
fn sqlite_unchanged_xdp_attach_dsl() {
    cli::dsl("sqlite", "TestXDP_LinkRoundTrip");
    cli::dsl("sqlite", "TestDispatcher_LifecycleAfterLastDetachXDP");
}

#[test]
fn json_unchanged_xdp_attach_dsl() {
    cli::dsl("json", "TestXDP_LinkRoundTrip");
    cli::dsl("json", "TestDispatcher_LifecycleAfterLastDetachXDP");
}

#[path = "kernel/xdp_attach.rs"]
mod xdp_attach;

#[test]
fn sqlite_xdp_attachment_failures() {
    xdp_attach::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn json_xdp_attachment_failures() {
    xdp_attach::exercise(bpfman_store_json::Backend);
}

#[path = "kernel/xdp_switch.rs"]
mod xdp_switch;

#[test]
fn sqlite_xdp_switch_and_restoration() {
    xdp_switch::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn json_xdp_switch_and_restoration() {
    xdp_switch::exercise(bpfman_store_json::Backend);
}

#[path = "kernel/xdp_runtime.rs"]
mod xdp_runtime;

#[test]
fn sqlite_xdp_runtime_replacement() {
    xdp_runtime::exercise(bpfman_store_sqlite::Backend, "sqlite");
}

#[test]
fn json_xdp_runtime_replacement() {
    xdp_runtime::exercise(bpfman_store_json::Backend, "json");
}

fn xdp_replacement_dsl(store: &'static str) {
    for script in [
        "TestDispatcher_PriorityOrderingXDP",
        "TestDispatcher_SlotReusedAfterDetachXDP",
        "TestDispatcher_AttachExceedsMaxProgramsXDP",
        "TestXDP_DefaultProceedOnRebuild",
        "TestXDP_DispatcherConfigAfterDetach",
    ] {
        cli::dsl(store, script);
    }
}

#[test]
fn sqlite_unchanged_xdp_replacement_dsl() {
    xdp_replacement_dsl("sqlite");
}

#[test]
fn json_unchanged_xdp_replacement_dsl() {
    xdp_replacement_dsl("json");
}

#[path = "kernel/xdp_unload.rs"]
mod xdp_unload;

#[test]
fn sqlite_xdp_unload_lifecycle() {
    xdp_unload::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn json_xdp_unload_lifecycle() {
    xdp_unload::exercise(bpfman_store_json::Backend);
}

#[test]
fn sqlite_unchanged_xdp_unload_dsl() {
    cli::dsl("sqlite", "TestXDP_UnloadDispatcherMemberRebuildsSurvivor");
}

#[test]
fn json_unchanged_xdp_unload_dsl() {
    cli::dsl("json", "TestXDP_UnloadDispatcherMemberRebuildsSurvivor");
}

#[path = "kernel/xdp_corpus.rs"]
mod xdp_corpus;

#[test]
fn sqlite_xdp_netns_round_trip() {
    cli::dsl("sqlite", "TestXDP_NetnsVethPairLinkRoundTrip");
}

#[test]
fn json_xdp_netns_round_trip() {
    cli::dsl("json", "TestXDP_NetnsVethPairLinkRoundTrip");
}

#[test]
fn sqlite_xdp_netns_rebuild() {
    cli::dsl("sqlite", "TestXDP_NetnsDispatcherRebuild");
}

#[test]
fn json_xdp_netns_rebuild() {
    cli::dsl("json", "TestXDP_NetnsDispatcherRebuild");
}

#[path = "kernel/xdp_netns.rs"]
mod xdp_netns;

#[test]
fn sqlite_xdp_netns_recovery() {
    xdp_netns::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn json_xdp_netns_recovery() {
    xdp_netns::exercise(bpfman_store_json::Backend);
}

#[path = "kernel/xdp_frags.rs"]
mod xdp_frags;

#[test]
fn sqlite_xdp_frags_multibuffer() {
    xdp_frags::exercise(bpfman_store_sqlite::Backend, bpfman_model::XdpMode::Drv);
}

#[test]
fn json_xdp_frags_multibuffer() {
    xdp_frags::exercise(bpfman_store_json::Backend, bpfman_model::XdpMode::Drv);
}

#[test]
fn sqlite_xdp_frags_multibuffer_skb() {
    xdp_frags::exercise(bpfman_store_sqlite::Backend, bpfman_model::XdpMode::Skb);
}

#[test]
fn json_xdp_frags_multibuffer_skb() {
    xdp_frags::exercise(bpfman_store_json::Backend, bpfman_model::XdpMode::Skb);
}

#[path = "kernel/xdp_delivery.rs"]
mod xdp_delivery;

#[test]
fn sqlite_xdp_packet_delivery() {
    xdp_delivery::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn json_xdp_packet_delivery() {
    xdp_delivery::exercise(
        bpfman_store_json::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn sqlite_xdp_packet_delivery_skb() {
    xdp_delivery::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn json_xdp_packet_delivery_skb() {
    xdp_delivery::exercise(
        bpfman_store_json::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::Linear,
    );
}

#[path = "kernel/xdp_devmap.rs"]
mod xdp_devmap;

#[test]
fn sqlite_xdp_devmap_delivery() {
    xdp_devmap::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn json_xdp_devmap_delivery() {
    xdp_devmap::exercise(
        bpfman_store_json::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn sqlite_xdp_devmap_delivery_skb() {
    xdp_devmap::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn json_xdp_devmap_delivery_skb() {
    xdp_devmap::exercise(
        bpfman_store_json::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn sqlite_xdp_multibuffer_forwarding() {
    xdp_delivery::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::MultiBuffer,
    );
    xdp_devmap::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[test]
fn sqlite_xdp_multibuffer_forwarding_skb() {
    xdp_delivery::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::MultiBuffer,
    );
    xdp_devmap::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[test]
fn json_xdp_multibuffer_forwarding() {
    xdp_delivery::exercise(
        bpfman_store_json::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::MultiBuffer,
    );
    xdp_devmap::exercise(
        bpfman_store_json::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[test]
fn json_xdp_multibuffer_forwarding_skb() {
    xdp_delivery::exercise(
        bpfman_store_json::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::MultiBuffer,
    );
    xdp_devmap::exercise(
        bpfman_store_json::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[path = "kernel/xdp_broadcast.rs"]
mod xdp_broadcast;

#[test]
fn sqlite_xdp_devmap_broadcast() {
    xdp_broadcast::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn sqlite_xdp_devmap_broadcast_skb() {
    xdp_broadcast::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn json_xdp_devmap_broadcast() {
    xdp_broadcast::exercise(
        bpfman_store_json::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn json_xdp_devmap_broadcast_skb() {
    xdp_broadcast::exercise(
        bpfman_store_json::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn sqlite_xdp_devmap_broadcast_frags() {
    xdp_broadcast::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[test]
fn sqlite_xdp_devmap_broadcast_frags_skb() {
    xdp_broadcast::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[test]
fn json_xdp_devmap_broadcast_frags() {
    xdp_broadcast::exercise(
        bpfman_store_json::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[test]
fn json_xdp_devmap_broadcast_frags_skb() {
    xdp_broadcast::exercise(
        bpfman_store_json::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::MultiBuffer,
    );
}
