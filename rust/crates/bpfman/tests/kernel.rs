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
    let _timing = support::TestTiming::start("sqlite_lifecycle_store_failures");
    lifecycle::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn sqlite_cli_effect_boundaries() {
    let _timing = support::TestTiming::start("sqlite_cli_effect_boundaries");
    cli::effect_boundaries();
}

#[test]
fn sqlite_go_interoperability() {
    let _timing = support::TestTiming::start("sqlite_go_interoperability");
    go_compatibility::exercise();
}

#[test]
fn sqlite_cancellation_boundaries() {
    let _timing = support::TestTiming::start("sqlite_cancellation_boundaries");
    cancellation::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn sqlite_tracepoint_attachment_lifecycle() {
    let _timing = support::TestTiming::start("sqlite_tracepoint_attachment_lifecycle");
    links::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn sqlite_attached_unload_failures_and_cancellation() {
    let _timing = support::TestTiming::start("sqlite_attached_unload_failures_and_cancellation");
    attached::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn go_tracepoint_dsl_unload_attached() {
    let _timing = support::TestTiming::start("go_tracepoint_dsl_unload_attached");
    cli::go_dsl("TestTracepoint_UnloadAttached");
}

#[test]
fn sqlite_unload_pending_pin_failures_and_retries() {
    let _timing = support::TestTiming::start("sqlite_unload_pending_pin_failures_and_retries");
    pending::pinned(bpfman_store_sqlite::Backend);
}

#[test]
fn sqlite_unload_pending_intent_without_pin() {
    let _timing = support::TestTiming::start("sqlite_unload_pending_intent_without_pin");
    pending::unpinned(bpfman_store_sqlite::Backend);
}

#[test]
fn sqlite_batch_lifecycle() {
    let _timing = support::TestTiming::start("sqlite_batch_lifecycle");
    batch::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn sqlite_batch_cli() {
    let _timing = support::TestTiming::start("sqlite_batch_cli");
    batch::cli();
}

#[path = "kernel/xdp.rs"]
mod xdp;

#[test]
fn sqlite_xdp_load_lifecycle() {
    let _timing = support::TestTiming::start("sqlite_xdp_load_lifecycle");
    xdp::exercise(bpfman_store_sqlite::Backend);
}

#[path = "kernel/xdp_attach.rs"]
mod xdp_attach;

#[test]
fn sqlite_xdp_attachment_failures() {
    let _timing = support::TestTiming::start("sqlite_xdp_attachment_failures");
    xdp_attach::exercise(bpfman_store_sqlite::Backend);
}

#[path = "kernel/xdp_switch.rs"]
mod xdp_switch;

#[test]
fn sqlite_xdp_switch_and_restoration() {
    let _timing = support::TestTiming::start("sqlite_xdp_switch_and_restoration");
    xdp_switch::exercise(bpfman_store_sqlite::Backend);
}

#[path = "kernel/xdp_runtime.rs"]
mod xdp_runtime;

#[test]
fn sqlite_xdp_runtime_replacement() {
    let _timing = support::TestTiming::start("sqlite_xdp_runtime_replacement");
    xdp_runtime::exercise(bpfman_store_sqlite::Backend);
}

#[path = "kernel/xdp_unload.rs"]
mod xdp_unload;

#[test]
fn sqlite_xdp_unload_lifecycle() {
    let _timing = support::TestTiming::start("sqlite_xdp_unload_lifecycle");
    xdp_unload::exercise(bpfman_store_sqlite::Backend);
}

#[path = "kernel/xdp_netns.rs"]
mod xdp_netns;

#[test]
fn sqlite_xdp_netns_recovery() {
    let _timing = support::TestTiming::start("sqlite_xdp_netns_recovery");
    xdp_netns::exercise(bpfman_store_sqlite::Backend);
}

#[path = "kernel/xdp_frags.rs"]
mod xdp_frags;

#[test]
fn sqlite_xdp_frags_multibuffer() {
    let _timing = support::TestTiming::start("sqlite_xdp_frags_multibuffer");
    xdp_frags::exercise(bpfman_store_sqlite::Backend, bpfman_model::XdpMode::Drv);
}

#[test]
fn sqlite_xdp_frags_multibuffer_skb() {
    let _timing = support::TestTiming::start("sqlite_xdp_frags_multibuffer_skb");
    xdp_frags::exercise(bpfman_store_sqlite::Backend, bpfman_model::XdpMode::Skb);
}

#[path = "kernel/xdp_delivery.rs"]
mod xdp_delivery;

#[test]
fn sqlite_xdp_packet_delivery() {
    let _timing = support::TestTiming::start("sqlite_xdp_packet_delivery");
    xdp_delivery::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn sqlite_xdp_packet_delivery_skb() {
    let _timing = support::TestTiming::start("sqlite_xdp_packet_delivery_skb");
    xdp_delivery::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::Linear,
    );
}

#[path = "kernel/xdp_devmap.rs"]
mod xdp_devmap;

#[test]
fn sqlite_xdp_devmap_delivery() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_delivery");
    xdp_devmap::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn sqlite_xdp_devmap_delivery_skb() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_delivery_skb");
    xdp_devmap::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::Linear,
    );
}

// Delivery and DEVMAP own separate fixtures. Keep them separately scheduled so
// idle workers can take either jumbo fault matrix without waiting for the other.
#[test]
fn sqlite_xdp_multibuffer_forwarding_delivery() {
    let _timing = support::TestTiming::start("sqlite_xdp_multibuffer_forwarding_delivery");
    xdp_delivery::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[test]
fn sqlite_xdp_multibuffer_forwarding_devmap() {
    let _timing = support::TestTiming::start("sqlite_xdp_multibuffer_forwarding_devmap");
    xdp_devmap::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[test]
fn sqlite_xdp_multibuffer_forwarding_delivery_skb() {
    let _timing = support::TestTiming::start("sqlite_xdp_multibuffer_forwarding_delivery_skb");
    xdp_delivery::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[test]
fn sqlite_xdp_multibuffer_forwarding_devmap_skb() {
    let _timing = support::TestTiming::start("sqlite_xdp_multibuffer_forwarding_devmap_skb");
    xdp_devmap::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[path = "kernel/xdp_broadcast.rs"]
mod xdp_broadcast;

#[test]
fn sqlite_xdp_devmap_broadcast() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_broadcast");
    xdp_broadcast::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn sqlite_xdp_devmap_broadcast_skb() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_broadcast_skb");
    xdp_broadcast::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn sqlite_xdp_devmap_broadcast_frags() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_broadcast_frags");
    xdp_broadcast::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[test]
fn sqlite_xdp_devmap_broadcast_frags_skb() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_broadcast_frags_skb");
    xdp_broadcast::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[test]
fn sqlite_xdp_devmap_broadcast_hash() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_broadcast_hash");
    xdp_broadcast::exercise_hash(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn sqlite_xdp_devmap_broadcast_hash_skb() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_broadcast_hash_skb");
    xdp_broadcast::exercise_hash(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn sqlite_xdp_devmap_broadcast_hash_frags() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_broadcast_hash_frags");
    xdp_broadcast::exercise_hash(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[test]
fn sqlite_xdp_devmap_broadcast_hash_frags_skb() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_broadcast_hash_frags_skb");
    xdp_broadcast::exercise_hash(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[path = "kernel/xdp_egress.rs"]
mod xdp_egress;

#[test]
fn sqlite_xdp_devmap_egress() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_egress");
    xdp_egress::exercise(bpfman_store_sqlite::Backend, bpfman_model::XdpMode::Drv);
}

#[test]
fn sqlite_xdp_devmap_egress_skb() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_egress_skb");
    xdp_egress::exercise(bpfman_store_sqlite::Backend, bpfman_model::XdpMode::Skb);
}

#[test]
fn sqlite_xdp_devmap_egress_frags_boundary() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_egress_frags_boundary");
    xdp_egress::fragments_boundary(bpfman_store_sqlite::Backend, bpfman_model::XdpMode::Drv);
}

#[test]
fn sqlite_xdp_devmap_egress_frags_boundary_skb() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_egress_frags_boundary_skb");
    xdp_egress::fragments_boundary(bpfman_store_sqlite::Backend, bpfman_model::XdpMode::Skb);
}

#[test]
fn sqlite_xdp_devmap_egress_hash() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_egress_hash");
    xdp_egress::exercise_hash(bpfman_store_sqlite::Backend, bpfman_model::XdpMode::Drv);
}

#[test]
fn sqlite_xdp_devmap_egress_hash_skb() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_egress_hash_skb");
    xdp_egress::exercise_hash(bpfman_store_sqlite::Backend, bpfman_model::XdpMode::Skb);
}

#[test]
fn sqlite_xdp_devmap_egress_hash_frags() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_egress_hash_frags");
    xdp_egress::fragments_hash(bpfman_store_sqlite::Backend, bpfman_model::XdpMode::Drv);
}

#[test]
fn sqlite_xdp_devmap_egress_hash_frags_skb() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_egress_hash_frags_skb");
    xdp_egress::fragments_hash(bpfman_store_sqlite::Backend, bpfman_model::XdpMode::Skb);
}

#[test]
fn sqlite_xdp_devmap_hash() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_hash");
    xdp_devmap::exercise_hash(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn sqlite_xdp_devmap_hash_skb() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_hash_skb");
    xdp_devmap::exercise_hash(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::Linear,
    );
}

#[test]
fn sqlite_xdp_devmap_hash_frags() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_hash_frags");
    xdp_devmap::exercise_hash(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Drv,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[test]
fn sqlite_xdp_devmap_hash_frags_skb() {
    let _timing = support::TestTiming::start("sqlite_xdp_devmap_hash_frags_skb");
    xdp_devmap::exercise_hash(
        bpfman_store_sqlite::Backend,
        bpfman_model::XdpMode::Skb,
        xdp_delivery::Frames::MultiBuffer,
    );
}

#[path = "kernel/tc.rs"]
mod tc;
#[test]
fn sqlite_tc_ingress_lifecycle() {
    let _timing = support::TestTiming::start("sqlite_tc_ingress_lifecycle");
    tc::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::TcDirection::Ingress,
    );
}

#[path = "kernel/tc_unload.rs"]
mod tc_unload;

#[path = "kernel/tc_orphan.rs"]
mod tc_orphan;

#[test]
fn sqlite_tc_orphan_recovery() {
    let _timing = support::TestTiming::start("sqlite_tc_orphan_recovery");
    tc_orphan::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn sqlite_tc_ingress_attached_unload() {
    let _timing = support::TestTiming::start("sqlite_tc_ingress_attached_unload");
    tc_unload::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::TcDirection::Ingress,
    );
}

#[path = "kernel/tc_replace.rs"]
mod tc_replace;

#[test]
fn sqlite_tc_ingress_replacement() {
    let _timing = support::TestTiming::start("sqlite_tc_ingress_replacement");
    tc_replace::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::TcDirection::Ingress,
    );
}

#[test]
fn sqlite_script_corpus() {
    let _timing = support::TestTiming::start("sqlite_script_corpus");
    cli::corpus();
}

#[test]
fn sqlite_tc_egress_lifecycle() {
    let _timing = support::TestTiming::start("sqlite_tc_egress_lifecycle");
    tc::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::TcDirection::Egress,
    );
}

#[test]
fn sqlite_tc_egress_attached_unload() {
    let _timing = support::TestTiming::start("sqlite_tc_egress_attached_unload");
    tc_unload::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::TcDirection::Egress,
    );
}

#[test]
fn sqlite_tc_egress_replacement() {
    let _timing = support::TestTiming::start("sqlite_tc_egress_replacement");
    tc_replace::exercise(
        bpfman_store_sqlite::Backend,
        bpfman_model::TcDirection::Egress,
    );
}

#[path = "kernel/tcx.rs"]
mod tcx;

#[test]
fn sqlite_tcx_native_link_recovery() {
    tcx::exercise(bpfman_store_sqlite::Backend);
}
#[test]
fn sqlite_tcx_script_corpus() {
    cli::tcx_scripts();
}
#[test]
fn sqlite_kprobe_script_corpus() {
    cli::kprobe_scripts();
}

#[path = "kernel/kprobe.rs"]
mod kprobe;

#[test]
fn sqlite_kprobe_native_link_recovery() {
    kprobe::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn sqlite_map_sharing_script_corpus() {
    cli::map_sharing_scripts();
}
#[path = "kernel/map_sharing.rs"]
mod map_sharing;
#[test]
fn sqlite_map_sharing_native_recovery() {
    map_sharing::exercise(bpfman_store_sqlite::Backend);
}
#[test]
fn sqlite_uprobe_script_corpus() {
    cli::uprobe_scripts();
}

#[path = "kernel/uprobe.rs"]
mod uprobe;

#[test]
fn sqlite_uprobe_native_link_recovery() {
    uprobe::exercise(bpfman_store_sqlite::Backend);
}

#[test]
fn sqlite_container_uprobe_script_corpus() {
    cli::container_uprobe_scripts();
}

#[path = "kernel/container_uprobe.rs"]
mod container_uprobe;

#[test]
fn sqlite_container_uprobe_namespace_lookup() {
    container_uprobe::exercise();
}

#[path = "kernel/library_uprobe.rs"]
mod library_uprobe;

#[test]
fn sqlite_uprobe_library_resolution() {
    library_uprobe::exercise();
}

#[path = "kernel/function_tracing.rs"]
mod function_tracing;
#[test]
fn sqlite_function_tracing_native_link_recovery() {
    function_tracing::exercise(bpfman_store_sqlite::Backend);
}
#[test]
fn sqlite_function_tracing_script_corpus() {
    cli::function_tracing_scripts();
}

#[path = "kernel/lsm.rs"]
mod lsm;
#[test]
fn sqlite_lsm_native_link_recovery() {
    lsm::exercise(bpfman_store_sqlite::Backend);
}
#[test]
fn sqlite_lsm_script_corpus() {
    cli::lsm_scripts();
}
