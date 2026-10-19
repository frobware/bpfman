// SPDX-License-Identifier: (GPL-2.0-only OR BSD-2-Clause)
// Copyright Authors of bpfman
// The inner template is valid; outer creation must fail and close both handles.
#define MOM_PINNING LIBBPF_PIN_BY_NAME
#define MOM_CAPACITY 0
#include "map_of_maps.h"
