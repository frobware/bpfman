// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman

#include <linux/bpf.h>
#include <linux/pkt_cls.h>
#include <linux/types.h>

#include <bpf/bpf_helpers.h>

// This counting program example was adapted from
// https://github.com/xdp-project/xdp-tutorial/tree/master/basic03-map-counter

/* This is the data record stored in the map */
struct tcdatarec {
  __u64 rx_packets;
  __u64 rx_bytes;
} tcdatarec;

struct {
  __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
  __type(key, __u32);
  __type(value, tcdatarec);
  __uint(max_entries, TC_ACT_VALUE_MAX);
  __uint(pinning, LIBBPF_PIN_BY_NAME);
} tc_stats_map SEC(".maps");

static __u32 tc_stats_record_action(struct __sk_buff *skb, __u32 action) {
  void *data = (void *)(long)skb->data;
  void *data_end = (void *)(long)skb->data_end;

  if (data_end < data)
    return TC_ACT_SHOT;

  if (action >= TC_ACT_VALUE_MAX)
    return TC_ACT_SHOT;

  /* Lookup in kernel BPF-side return pointer to actual data record */
  struct tcdatarec *rec = bpf_map_lookup_elem(&tc_stats_map, &action);
  if (!rec)
    return TC_ACT_SHOT;

  /* Calculate packet length */
  __u64 bytes = data_end - data;

  rec->rx_packets++;
  rec->rx_bytes += bytes;

  return action;
}

SEC("classifier/stats")
int stats(struct __sk_buff *skb) {
  __u32 action = TC_ACT_OK;

  return tc_stats_record_action(skb, action);
}
