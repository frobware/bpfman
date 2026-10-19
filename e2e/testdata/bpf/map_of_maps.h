// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman
#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>

#ifndef MOM_PINNING
#define MOM_PINNING LIBBPF_PIN_NONE
#endif
#ifndef MOM_CAPACITY
#define MOM_CAPACITY 2
#endif

struct mom_inner {
  __uint(type, BPF_MAP_TYPE_ARRAY);
  __type(key, __u32);
  __type(value, __u64);
  __uint(max_entries, 1);
};

// A real inner map is populated by the test. The anonymous template used to
// create each outer map must remain unpinned and must not become a map-set user.
struct mom_inner mom_values SEC(".maps");

struct {
  __uint(type, BPF_MAP_TYPE_ARRAY_OF_MAPS);
  __type(key, __u32);
  __uint(max_entries, MOM_CAPACITY);
  __uint(pinning, MOM_PINNING);
  __array(values, struct mom_inner);
} mom_array SEC(".maps");

struct {
  __uint(type, BPF_MAP_TYPE_HASH_OF_MAPS);
  __type(key, __u32);
  __uint(max_entries, MOM_CAPACITY);
  __uint(pinning, MOM_PINNING);
  __array(values, struct mom_inner);
} mom_hash SEC(".maps");

SEC("kprobe/mom_counter")
int mom_counter(void *ctx) {
  __u32 zero = 0, hash_key = 7;
  void *inner = bpf_map_lookup_elem(&mom_array, &zero);
  if (inner) {
    __u64 *value = bpf_map_lookup_elem(inner, &zero);
    if (value)
      __sync_fetch_and_add(value, 1);
  }
  inner = bpf_map_lookup_elem(&mom_hash, &hash_key);
  if (inner) {
    __u64 *value = bpf_map_lookup_elem(inner, &zero);
    if (value)
      __sync_fetch_and_add(value, 1);
  }
  return 0;
}

SEC("kprobe/mom_bad")
int mom_bad(void *ctx) {
  __u32 key = 0;
  void *inner = bpf_map_lookup_elem(&mom_array, &key);
  // Deliberately omit the outer lookup's NULL check.
  __u64 *value = bpf_map_lookup_elem(inner, &key);
  if (value)
    (*value)++;
  return 0;
}

char _license[] SEC("license") = "Dual BSD/GPL";
