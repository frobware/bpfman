// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman
#include <linux/bpf.h>
#include <linux/if_ether.h>
#include <linux/pkt_cls.h>

#include <bpf/bpf_endian.h>
#include <bpf/bpf_helpers.h>

struct {
  __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
  __uint(max_entries, 1);
  __type(key, __u32);
  __type(value, __u64);
} tc_stats SEC(".maps");

volatile const int tc_action = TC_ACT_SHOT;
SEC("classifier")
int tc_ingress(struct __sk_buff *skb) {
  void *data = (void *)(long)skb->data;
  void *end = (void *)(long)skb->data_end;
  struct ethhdr *eth = data;
  if ((void *)(eth + 1) > end || eth->h_proto != bpf_htons(0x88b5))
    return TC_ACT_OK;
  __u32 key = 0;
  __u64 *count = bpf_map_lookup_elem(&tc_stats, &key);
  if (count)
    (*count)++;
  return tc_action;
}
SEC("classifier/foreign")
int tc_foreign(struct __sk_buff *skb) { return TC_ACT_PIPE; }

char _license[] SEC("license") = "Dual BSD/GPL";
