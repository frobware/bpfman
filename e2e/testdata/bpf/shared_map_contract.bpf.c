// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman
// A deliberately incompatible definition of the ordinary tracepoint counter's
// shared map. The bad selection also verifies failure after shared-pin creation.
#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>

struct {
  __uint(type, BPF_MAP_TYPE_HASH);
  __type(key, __u32);
  __type(value, __u32);
  __uint(max_entries, 2);
  __uint(pinning, LIBBPF_PIN_BY_NAME);
} tracepoint_stats_map SEC(".maps");

// Multiple read-only variables with a nonzero offset exercise shared-map BTF
// section extent fixups. Member count is not the section's byte size.
const volatile char shared_btf_padding[64] = {1};
const volatile __u32 shared_btf_marker = 1;

SEC("kprobe/shared_map_good")
int shared_map_good(void *ctx) {
  __u32 key = 0;
  __u32 *value = bpf_map_lookup_elem(&tracepoint_stats_map, &key);
  if (value && shared_btf_padding[0] == 1 && shared_btf_marker == 1)
    (*value)++;
  return 0;
}

SEC("kprobe/shared_map_bad")
int shared_map_bad(void *ctx) {
  __u32 key = 0;
  __u32 *value = bpf_map_lookup_elem(&tracepoint_stats_map, &key);
  (*value)++; // Hash lookups may return NULL: the verifier must reject this.
  return 0;
}

char _license[] SEC("license") = "Dual BSD/GPL";
