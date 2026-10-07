// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman
// Only the packet probe's experimental Ethernet frames exercise these actions.
#include <linux/bpf.h>
#include <linux/if_ether.h>

#include <bpf/bpf_endian.h>
#include <bpf/bpf_helpers.h>

volatile const __u32 delivery_action = XDP_TX;
volatile const __u32 delivery_ifindex = 0;

struct {
  __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
  __type(key, __u32);
  __type(value, __u64);
  __uint(max_entries, 2);
} delivery_stats SEC(".maps");

static __always_inline struct ethhdr *probe(struct xdp_md *ctx) {
  void *data = (void *)(long)ctx->data;
  void *end = (void *)(long)ctx->data_end;
  struct ethhdr *eth = data;
  __be32 *magic = (void *)(eth + 1);
  if ((void *)(magic + 1) > end || eth->h_proto != bpf_htons(0x88b5) ||
      *magic != bpf_htonl(0xb9f00001))
    return 0;
  return eth;
}

static __always_inline void count(__u32 key) {
  __u64 *value = bpf_map_lookup_elem(&delivery_stats, &key);
  if (value)
    (*value)++;
}

SEC("xdp")
int delivery(struct xdp_md *ctx) {
  struct ethhdr *eth = probe(ctx);
  if (!eth)
    return XDP_PASS;
  count(0);
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

SEC("xdp")
int delivery_tail(struct xdp_md *ctx) {
  if (!probe(ctx))
    return XDP_PASS;
  count(1);
  return XDP_DROP;
}

char _license[] SEC("license") = "Dual BSD/GPL";
