// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman
// Only the packet probe's experimental Ethernet frames exercise these actions.
#include "xdp_delivery_common.bpf.h"

volatile const __u32 delivery_action = XDP_TX;
volatile const __u32 delivery_ifindex = 0;

SEC(DELIVERY_SECTION)
int delivery(struct xdp_md *ctx) {
  struct ethhdr *eth = probe(ctx);
  if (!eth)
    return XDP_PASS;
  observe(ctx, 0);
  if (delivery_action == XDP_TX) {
    __u8 source[ETH_ALEN];
    __builtin_memcpy(source, eth->h_source, ETH_ALEN);
    __builtin_memcpy(eth->h_source, eth->h_dest, ETH_ALEN);
    __builtin_memcpy(eth->h_dest, source, ETH_ALEN);
    return XDP_TX;
  }
  if (delivery_action == XDP_REDIRECT) {
    // The private topology gives sink0 this address.
    const __u8 sink[ETH_ALEN] = {2, 0, 0, 0, 0, 3};
    __builtin_memcpy(eth->h_dest, sink, ETH_ALEN);
    return bpf_redirect(delivery_ifindex, 0);
  }
  return XDP_ABORTED;
}

SEC(DELIVERY_SECTION)
int delivery_tail(struct xdp_md *ctx) {
  if (!probe(ctx))
    return XDP_PASS;
  observe(ctx, 1);
  return XDP_DROP;
}

#ifdef DELIVERY_FRAGS
SEC(DELIVERY_SECTION)
int delivery_observer(struct xdp_md *ctx) {
  if (probe(ctx))
    observe(ctx, 0);
  return XDP_PASS;
}
#endif

char _license[] SEC("license") = "Dual BSD/GPL";
