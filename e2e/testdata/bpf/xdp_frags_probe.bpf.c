// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Multi-buffer evidence: total length exceeds linear length, and helpers can
// read the final payload byte and across the linear/fragment boundary.
#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>

struct {
  __uint(type, BPF_MAP_TYPE_PERCPU_ARRAY);
  __type(key, __u32);
  __type(value, __u64);
  __uint(max_entries, 4);
} frags_probe_stats SEC(".maps");

static __always_inline void count(__u32 key) {
  __u64 *value = bpf_map_lookup_elem(&frags_probe_stats, &key);
  if (value)
    (*value)++;
}

SEC("xdp.frags")
int frags_probe(struct xdp_md *ctx) {
  __u32 linear = ctx->data_end - ctx->data;
  __u32 total = bpf_xdp_get_buff_len(ctx);
  count(0);
  if (total > linear && linear > 0) {
    count(1);
    __u8 tail = 0;
    if (!bpf_xdp_load_bytes(ctx, total - 1, &tail, sizeof(tail)) && tail == 0xa5)
      count(2);
    __u8 boundary[2] = {};
    if (!bpf_xdp_load_bytes(ctx, linear - 1, boundary, sizeof(boundary)) &&
        boundary[0] == 0xa5 && boundary[1] == 0xa5)
      count(3);
  }
  return XDP_PASS;
}
char _license[] SEC("license") = "Dual BSD/GPL";
