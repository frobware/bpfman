// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman
#include <linux/errno.h>

#include "xdp_delivery_common.bpf.h"

volatile const __u32 selected_map_id = 0;
volatile const __u32 selected_ifindex = 0;

// xdp_redirect_err tracepoint layout. Acceptance verifies the fields' offsets
// and sizes against the running kernel's tracefs format before loading this.
struct redirect_event {
  __u64 common;
  __u32 prog_id;
  __u32 action;
  __s32 ifindex;
  __s32 error;
  __s32 to_ifindex;
  __u32 map_id;
  __s32 map_index;
};

SEC("tracepoint/xdp/xdp_redirect_err")
int redirect_error(struct redirect_event *ctx) {
  if (ctx->map_id != selected_map_id || ctx->ifindex != selected_ifindex)
    return 0;
  count(0);
  if (ctx->error == -EOPNOTSUPP)
    count(1);
  return 0;
}

char _license[] SEC("license") = "Dual BSD/GPL";
