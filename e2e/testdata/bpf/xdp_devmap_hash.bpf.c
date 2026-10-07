// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman
#define DEVMAP_TYPE BPF_MAP_TYPE_DEVMAP_HASH
#define DEVMAP_ENTRIES 2
// Hash keys are independent of the capacity, unlike DEVMAP array slots.
#define DEVMAP_KEY 0x80000001U
#include "xdp_devmap.bpf.c"
