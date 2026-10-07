// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman
#include "xdp_delivery_common.bpf.h"

volatile const __u32 devmap_fallback = XDP_PASS;
volatile const __u32 devmap_flags = 0;

#ifndef DEVMAP_ENTRIES
#define DEVMAP_ENTRIES 1
#endif
#ifndef DEVMAP_KEY
#define DEVMAP_KEY 0
#endif
#ifndef DEVMAP_TYPE
#define DEVMAP_TYPE BPF_MAP_TYPE_DEVMAP
#endif

struct {
  __uint(type, DEVMAP_TYPE);
  __type(key, __u32);
  __type(value, __u32);
  __uint(max_entries, DEVMAP_ENTRIES);
} delivery_targets SEC(".maps");

SEC(DELIVERY_SECTION)
int devmap_delivery(struct xdp_md *ctx) {
  if (!probe(ctx))
    return XDP_PASS;
  observe(ctx, 0);
  return bpf_redirect_map(&delivery_targets, DEVMAP_KEY,
                          devmap_fallback | devmap_flags);
}

char _license[] SEC("license") = "Dual BSD/GPL";
