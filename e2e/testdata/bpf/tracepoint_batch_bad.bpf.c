// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman

// The second selection fails verification after the first has acquired pins.
#include <linux/bpf.h>
#include <bpf/bpf_helpers.h>

SEC("tracepoint/good")
int good(void *ctx) { return 0; }

SEC("tracepoint/bad")
int bad(void *ctx) { return *(volatile int *)0x1234; }

char _license[] SEC("license") = "Dual BSD/GPL";
