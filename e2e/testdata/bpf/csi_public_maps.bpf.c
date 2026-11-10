// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman
// Each function uses one shared map; both maps are published with the object.
#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>

struct {
  __uint(type, BPF_MAP_TYPE_ARRAY);
  __type(key, __u32);
  __type(value, __u64);
  __uint(max_entries, 1);
  __uint(pinning, LIBBPF_PIN_BY_NAME);
} csi_first_map SEC(".maps");

struct {
  __uint(type, BPF_MAP_TYPE_ARRAY);
  __type(key, __u32);
  __type(value, __u64);
  __uint(max_entries, 1);
  __uint(pinning, LIBBPF_PIN_BY_NAME);
} csi_second_map SEC(".maps");

SEC("tracepoint/csi_first")
int csi_first(void *ctx) {
  __u32 key = 0;
  __u64 *value = bpf_map_lookup_elem(&csi_first_map, &key);
  if (value)
    (*value)++;
  return 0;
}

SEC("tracepoint/csi_second")
int csi_second(void *ctx) {
  __u32 key = 0;
  __u64 *value = bpf_map_lookup_elem(&csi_second_map, &key);
  if (value)
    (*value)++;
  return 0;
}

char _license[] SEC("license") = "Dual BSD/GPL";
