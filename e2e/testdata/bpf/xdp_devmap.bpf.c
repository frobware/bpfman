// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman

// XDP devmap redirect-target program. It exists only to exercise
// bpfman's rejection of unsupported XDP attach types: the SEC("xdp/devmap")
// name makes cilium/ebpf parse it with expected attach type
// BPF_XDP_DEVMAP, which bpfman refuses at load because it attaches XDP
// programs to interfaces, not devmap entries. The body is irrelevant --
// the rejection happens before the kernel ever loads it.

#include <linux/bpf.h>

#include <bpf/bpf_helpers.h>

SEC("xdp/devmap")
int xdp_devmap_redirect(struct xdp_md *ctx) {
  return XDP_PASS;
}

char _license[] SEC("license") = "Dual BSD/GPL";
