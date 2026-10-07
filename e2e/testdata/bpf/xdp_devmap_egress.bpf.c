// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman
#include "xdp_delivery_common.bpf.h"

volatile const __u32 egress_action = XDP_PASS;
volatile const __u32 expected_ingress = 0;
volatile const __u32 expected_egress = 0;

#ifdef DELIVERY_FRAGS
#define EGRESS_SECTION "xdp.frags/devmap"
#else
#define EGRESS_SECTION "xdp/devmap"
#endif

SEC(EGRESS_SECTION)
int devmap_egress(struct xdp_md *ctx) {
  if (!probe(ctx))
    return XDP_PASS;
  observe(ctx, 0);
  if (ctx->ingress_ifindex == expected_ingress &&
      ctx->egress_ifindex == expected_egress)
    count(DELIVERY_COUNTERS - 1);
  return egress_action;
}

// Loading unsupported specialized roles must fail before runtime effects.
SEC("xdp/cpumap")
int cpumap_egress(struct xdp_md *ctx) { return XDP_PASS; }

char _license[] SEC("license") = "Dual BSD/GPL";
