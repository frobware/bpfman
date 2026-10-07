// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman
#include "xdp_delivery_common.bpf.h"

volatile const __u32 devmap_fallback = XDP_PASS;

struct {
  __uint(type, BPF_MAP_TYPE_DEVMAP);
  __type(key, __u32);
  __type(value, __u32);
  __uint(max_entries, 1);
} delivery_targets SEC(".maps");

SEC("xdp")
int devmap_delivery(struct xdp_md *ctx) {
  if (!probe(ctx))
    return XDP_PASS;
  count(0);
  return bpf_redirect_map(&delivery_targets, 0, devmap_fallback);
}

char _license[] SEC("license") = "Dual BSD/GPL";
