// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman
#define DEVMAP_ENTRIES 3
// Broadcast must ignore this out-of-range key, including with an empty map.
#define DEVMAP_KEY 99
#include "xdp_devmap.bpf.c"
