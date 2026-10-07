// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman
#ifndef E2E_XDP_DELIVERY_COMMON_H
#define E2E_XDP_DELIVERY_COMMON_H

#include <linux/bpf.h>
#include <linux/if_ether.h>

#include <bpf/bpf_endian.h>
#include <bpf/bpf_helpers.h>

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

#endif
