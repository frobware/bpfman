// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman
#ifndef E2E_XDP_DELIVERY_COMMON_H
#define E2E_XDP_DELIVERY_COMMON_H

#include <linux/bpf.h>
#include <linux/if_ether.h>

#include <bpf/bpf_endian.h>
#include <bpf/bpf_helpers.h>

// Separate objects retain the normal-sized acceptance surface.
#ifdef DELIVERY_FRAGS
#define DELIVERY_SECTION "xdp.frags"
#define DELIVERY_COUNTERS 8
#else
#define DELIVERY_SECTION "xdp"
#define DELIVERY_COUNTERS 2
#endif

struct {
  __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
  __type(key, __u32);
  __type(value, __u64);
  __uint(max_entries, DELIVERY_COUNTERS);
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

// Each executing member must independently prove genuine multi-buffer input.
static __always_inline void observe(struct xdp_md *ctx, __u32 role) {
#ifdef DELIVERY_FRAGS
  __u32 key = role * 4;
  __u32 linear = ctx->data_end - ctx->data;
  __u32 total = bpf_xdp_get_buff_len(ctx);
  count(key);
  if (total > linear && linear > 22) {
    count(key + 1);
    __u8 tail = 0;
    if (!bpf_xdp_load_bytes(ctx, total - 1, &tail, sizeof(tail)) &&
        tail == (__u8)((total - 1 - 22) % 251 + 1))
      count(key + 2);
    __u8 boundary[2] = {};
    if (!bpf_xdp_load_bytes(ctx, linear - 1, boundary, sizeof(boundary)) &&
        boundary[0] == (__u8)((linear - 1 - 22) % 251 + 1) &&
        boundary[1] == (__u8)((linear - 22) % 251 + 1))
      count(key + 3);
  }
#else
  count(role);
#endif
}

#endif
