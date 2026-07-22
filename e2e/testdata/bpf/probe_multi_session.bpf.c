// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman

// Multi-attach and session probe programs. They exist only to exercise
// bpfman's rejection of unsupported probe attach types: the SEC names
// make cilium/ebpf parse them with expected attach types
// BPF_TRACE_KPROBE_MULTI, BPF_TRACE_UPROBE_MULTI, and
// BPF_TRACE_KPROBE_SESSION. bpfman attaches kprobe and uprobe programs
// individually via perf_event, so it refuses these at load, before the
// kernel ever loads them. The bodies and targets are irrelevant.

#include <linux/bpf.h>

#include <bpf/bpf_helpers.h>

SEC("kprobe.multi/does_not_matter")
int kprobe_multi_probe(void *ctx) {
  return 0;
}

SEC("uprobe.multi/does_not_matter")
int uprobe_multi_probe(void *ctx) {
  return 0;
}

SEC("kprobe.session/does_not_matter")
int kprobe_session_probe(void *ctx) {
  return 0;
}

char _license[] SEC("license") = "Dual BSD/GPL";
