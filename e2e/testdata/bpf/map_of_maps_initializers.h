// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman
#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>

#ifndef MOM_INITIAL_PINNING
#define MOM_INITIAL_PINNING LIBBPF_PIN_NONE
#endif

struct mom_seed_inner {
  __uint(type, BPF_MAP_TYPE_ARRAY);
  __type(key, __u32);
  __type(value, __u64);
  __uint(max_entries, 1);
};

struct mom_seed_inner mom_seed_a SEC(".maps");
#ifdef MOM_INVALID_INNER
struct {
  __uint(type, BPF_MAP_TYPE_ARRAY);
  __type(key, __u32);
  __type(value, __u64);
  __uint(max_entries, 2);
} mom_seed_b SEC(".maps");
#else
struct mom_seed_inner mom_seed_b SEC(".maps");
#endif

struct {
  __uint(type, BPF_MAP_TYPE_ARRAY_OF_MAPS);
  __type(key, __u32);
  __uint(max_entries, 4);
  __uint(pinning, MOM_INITIAL_PINNING);
  __array(values, struct mom_seed_inner);
} mom_seed_array SEC(".maps") = {
    .values = {[1] = &mom_seed_a, [3] = (struct mom_seed_inner *)&mom_seed_b},
};

struct {
  __uint(type, BPF_MAP_TYPE_HASH_OF_MAPS);
  __type(key, __u32);
  __uint(max_entries, 2);
  __uint(pinning, MOM_INITIAL_PINNING);
  __array(values, struct mom_seed_inner);
} mom_seed_hash SEC(".maps") = {
    .values = {[1] = &mom_seed_a, [3] = (struct mom_seed_inner *)&mom_seed_b},
};

static __always_inline void increment(void *outer, __u32 key) {
  void *inner = bpf_map_lookup_elem(outer, &key);
  if (inner) {
    __u32 zero = 0;
    __u64 *value = bpf_map_lookup_elem(inner, &zero);
    if (value)
      __sync_fetch_and_add(value, 1);
  }
}

SEC("kprobe/mom_seed_counter")
int mom_seed_counter(void *ctx) {
  increment(&mom_seed_array, 1);
  increment(&mom_seed_array, 3);
  increment(&mom_seed_hash, 1);
  increment(&mom_seed_hash, 3);
  return 0;
}

SEC("kprobe/mom_seed_bad")
int mom_seed_bad(void *ctx) {
  __u32 key = 1, zero = 0;
  void *inner = bpf_map_lookup_elem(&mom_seed_array, &key);
  // A real verifier failure after the initialized maps have been acquired.
  __u64 *value = bpf_map_lookup_elem(inner, &zero);
  if (value)
    (*value)++;
  return 0;
}

char _license[] SEC("license") = "Dual BSD/GPL";
