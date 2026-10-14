# Rust reimplementation using the Go design

## Status

Implementation is in progress in the independent `rust/` workspace. The latest
capability checkpoint adds native TCX loading, ordered ingress/egress links and
consuming teardown on both stores, alongside legacy TC ingress/egress dispatchers.
For legacy TC, one to ten EXT members are rebuilt in priority order around a
native dispatcher, using the same exact legacy filter handle. Publication failures restore the old
chain before staged cleanup; restoration and retirement failures retain consuming
receipts for explicit retries. Attached-program unload rebuilds surviving members,
including repeated attachments of the same program. Direction is part of dispatcher,
store, pin and exact-filter identity. Both hooks retain shared clsact ownership
through either attach/drain order.
The preceding XDP checkpoint added native DEVMAP_HASH egress PASS/DROP for ordinary and genuine
multi-buffer frames in driver and generic SKB modes on both SQLite and JSON stores,
building on hash/array unicast, broadcast, and ingress exclusion acceptance.
At the egress checkpoint (`e64d4ccb2`), Rust's managed XDP surface has gone beyond
the current Go implementation: Rust can load and execute native DEVMAP egress
programs, while Go's loader converts all managed XDP selections into dispatcher
extensions. The comparison below also records mode and fragment-dispatch differences.
All-fragment-aware chains select an `xdp.frags` dispatcher; mixed chains select
a linear dispatcher. Published ELF declarations are read beneath the retained
runtime descriptor without changing the Go-compatible persistence schema.
Driver and SKB veth acceptance proves multi-buffer lengths and cross-buffer
helper reads using 8 KB ICMP payloads with IP fragmentation prohibited, including
replacement, publication rollback, mixed membership, and either survivor. The unchanged
normal-MTU fragments script also passes on both stores.
Configurable modes were committed and pushed as `d072f207d`; hardware-request
fallback is verified but actual offload remains unverified. Namespace descriptors
and consuming recovery transitions remain in use. Marked-frame captures now
prove TX return to the sender and direct REDIRECT delivery to a separate receiver,
proceed-on stopping/continuation, replacement, publication rollback, and either
survivor. DEVMAP tests additionally prove live target updates, PASS/DROP lookup
fallback, stable map identity/contents across replacement and restoration, and
eventual kernel reclamation after unload. Four additional suites repeat the direct
and map-backed forwarding lifecycle with 8 KB Ethernet payloads, exact full-payload
capture, and independent multi-buffer evidence in each executing member and
receiving peer. Eight broadcast suites additionally prove ordinary-sized fan-out,
ingress exclusion, ignored lookup keys, empty/sparse map behavior, and preservation
through replacement and restoration. On Linux 6.18.54, multi-buffer broadcast with
multiple eligible targets reports `EOPNOTSUPP`; tests require that exact rejection
and retain successful single-target jumbo delivery. DEVMAP egress tests additionally
prove correct ingress/output interface context, live program replacement, rollback,
and map-held program lifetime after managed unpinning. Fragment-capable egress
loads correctly, but array DEVMAP multi-buffer egress pairing remains blocked by
Aya 0.14's extension flag handling; tests require that exact map-update rejection
and prove
jumbo unicast without an egress program remains intact. Aya is unchanged.
DEVMAP_HASH acceptance uses two sparse keys beyond the map's capacity value,
checks the exact key set and unused target through replacement/restoration, and
proves capacity rejection without modifying existing entries. Live updates,
PASS/DROP missing-key fallback, either survivor, and eventual map reclamation
reuse the same forwarding lifecycle as DEVMAP. Genuine multi-buffer counters and
full 8014-byte payload capture remain required. No production changes were needed.
Eight additional broadcast suites reuse the complete fan-out lifecycle with a
three-entry DEVMAP_HASH keyed by `7`, `0x80000001`, and `0xffffffff`. Exact key sets
and values, including deletions and repopulation, remain stable through replacement,
publication restoration, and either survivor. Broadcast ignores missing lookup key
99 and PASS fallback; ingress exclusion prevents return to the sender. Ordinary
frames fan out successfully. Genuine multi-buffer frames retain successful
single-target delivery and require the same exact `EOPNOTSUPP` rejection with no
copies for multiple eligible targets on Linux 6.18.54. No production changes were
needed. Eight hash-backed egress suites additionally prove PASS/DROP delivery,
exact native-egress execution and interface context, live program changes at full
capacity, extension and fragment-compatibility rejection, replacement/restoration,
and map-held program lifetime through managed unpinning and unload retry. Sparse
keys `0x80000001` and `0xffffffff` retain the complete key set and unrelated target.
All four jumbo suites require full 8014-byte payload capture and fragment/tail/
boundary reads in executing ingress, egress, and receiving programs. No production
behavior, persistence, dependency, or Aya changes were needed.
Unlike array DEVMAP, Linux 6.18.54 omits DEVMAP_HASH from the load-time owner check;
the first native egress update initializes hash ownership with its fragment flag.
This permits genuine jumbo egress through the existing public API. Later egress
updates still require compatible flags. Array DEVMAP's Aya boundary remains.
Script parity now has an in-corpus manifest: passing scripts carry
`#pragma labels={"rust":"ok"}`. Run that selection through the existing Go runner,
using its interface pool and scheduling machinery, once per store. Grow this set
before expanding Rust-specific packet scenarios; corpus failures should drive
subsequent implementation. TC egress traffic and ingress/egress independence now
pass on both stores. Compare performance
on the same scripts after behavior matches.
Full behavioural parity remains unfinished.

The 14 October tmpfs reassessment fixes tracepoint detach/reattach ordering by
retaining a validated live link FD across unpinning and closing it afterward.
XDP and TC cleanup now applies the same ordering to freplace links, with immediate
program-ID assertions when no additional holders remain. Physical kernel
reclamation and map-held native egress references remain asynchronous. Fake and
real kernel suites now prefer owned
tmpfs fixture scopes while preserving synchronization and explicit disk comparison
lanes. See the [release investigation](#reassessing-real-kernel-tmpfs-release-failures-14-october-2026).

The runtime uses pure membership planning and consuming ownership transitions to
stage a complete revision, conditionally switch the durable outer link, and
atomically publish every member. Failed publication restores the previous target
before staged cleanup. Failed restoration retains both revisions and evidence
for an explicit retry. Successful publication permits only old-revision
retirement; retirement errors expose the committed snapshot and retain cleanup
ownership. Surviving managed link IDs and operator fields remain stable.

| Surface | Implemented checkpoint | Remaining boundary |
| --- | --- | --- |
| Local program load | Atomic tracepoint/XDP/TC batches, private maps, named selection, compensation | Other program families, shared maps, OCI sources |
| Tracepoint links | Pending intent, attach/detach, observations, attached-program unload | Broader attachment families |
| XDP links | One to ten members per interface, current or explicit namespace, configurable drv/skb/hw requests with non-SKB fallback, replacement, last detach, attached-program unload | Actual hardware offload, physical NIC packet execution |
| XDP replacement | Pure ordering/configuration; complete store publication; owned kernel switching/restoration; runtime recovery and explicit retries; fill/drain, chain-execution, driver/SKB multi-buffer, TX/direct-REDIRECT, DEVMAP and DEVMAP_HASH unicast, broadcast, and ingress exclusion acceptance; ordinary-frame DEVMAP egress and ordinary/multi-buffer DEVMAP_HASH egress PASS/DROP | Array DEVMAP multi-buffer egress blocked by Aya extension flags; kernel multi-buffer fan-out limitation |
| XDP observations | Program/link get and list; dispatcher get/list as JSON | Broader dispatcher CLI |
| TC ingress/egress | One to ten members per interface/namespace/direction; complete revision replacement; stable managed link IDs and exact filter handle; signed continuation; borrowed/owned clsact; atomic publication, restoration, retirement, explicit retries, attached-program unload, missing-namespace dispatcher cleanup, dispatcher get/list | TCX, outer-filter status and general orphan repair |
| TCX | Native SCHED_CLS loading; pinned ingress/egress links; ascending priority with attachment-order ties; duplicate refusal, namespace attachment, observations, compensation and attached unload | Foreign chain reconciliation and recovery after losing in-memory reports |
| Persistence | Go-compatible SQLite schema 2; new JSON stores use format 10 for TCX, format 9 retains TC egress; format 8 retains ingress replacement; format 7 retains singleton ingress; format 6 retains namespace-aware XDP | No implicit upgrade or conversion of existing state |
| Kernel boundary | One injected backend across reads and lifecycle; real adapters and stateful fake use the same runtime interpreter | Additional attachment families |

Attached XDP unload holds one writer scope and defers program teardown until every
dispatcher prerequisite succeeds. Failed restoration or revision retirement
retains ownership for an explicit retry; earlier successful detachments remain
complete. Foreign occupied attach points and unsupported commands/flags fail
clearly. See
[the XDP checkpoint](../../rust/README.md#xdp-attachment-and-replacement) and
[TC ingress lifecycle](../../rust/README.md#tc-ingress-attachment-and-unload) for the
supported command surface.

The TC ingress replacement checkpoint passed `direnv exec . make rust-check`:
formatting, Clippy, all userspace and compile-fail contracts, documentation, 60
shared fake-kernel lifecycle tests, and all 168 real-kernel tests, with none ignored.
The serial kernel run completed in 1834.78 seconds.
`make rust-test-tc-ingress` passed two shared store contracts and 36
kernel/CLI/script checks. Makefile lint and the Go runner's clsact capability
unit check also passed. The normal gate runs all userspace contracts
before the complete serial kernel binary; CLI test targets are discovered
automatically. Existing unicast, broadcast, and all sixteen array/hash egress suites
remain in that gate, including the four array-map jumbo rejection tests. Aya and
Go's SQLite schema remain unchanged.

Before the testing migration, the label-selected Go runner passed all 42 shared
`rust=ok` scripts in parallel
against Rust on each store: approximately 52 seconds for SQLite and 52 seconds
for JSON, with backend batches run sequentially. Both final inventories and all
managed artifact collections were empty. Script changes add only header labels;
the assertions and existing scheduling pragmas are preserved.

The testing migration checkpoint also passed `direnv exec . make rust-check`:
formatting, Clippy, all userspace and compile-fail contracts, documentation, and
98 real-kernel tests, with none failed or ignored. Individual script wrappers
were replaced by two backend-wide batches, each checking all 50 admitted scripts
and empty final inventories/artifact collections. The serial kernel suite took
1420.39 seconds (23m40s), compared with 1834.78 seconds (30m35s) before migration.
These are local checkpoint timings, not a controlled benchmark. The fast unit
target separately passed 92 tests in 0.60 seconds of aggregate test execution.
See the [migration audit](#testing-migration-audit-8-october-2026) for completed
work and the remaining behavioural tests to move into scripts.

The CLI load/unload and TX/direct REDIRECT migration follow-up passed the complete
`direnv exec . make rust-check` gate: formatting, Clippy, all userspace and
compile-fail contracts, documentation, and 98 real-kernel tests with none failed
or ignored. Each backend batch verified all 62 admitted scripts and empty final
inventories/artifact collections. The kernel stage took 1358.95 seconds (22m39s),
compared with 1420.39 seconds (23m40s) at the preceding migration checkpoint.
These remain local checkpoint timings, not a controlled benchmark. Makefile lint
and canonical formatting of the new scripts/shared helper also passed.

The ordinary DEVMAP broadcast migration passed the complete
`direnv exec . make rust-check` gate: formatting, Clippy, userspace and compile-fail
contracts, documentation, and all 98 real-kernel tests with none failed or ignored.
Both backend batches verified every one of the 66 admitted scripts and empty final
inventories/artifact collections. The kernel stage took 1180.41 s (19m40s), compared
with 1358.95 s (22m39s) at the CLI/delivery checkpoint. These are local timings,
not a controlled benchmark. Makefile lint and canonical DSL formatting also passed.
See the [broadcast follow-up](#ordinary-devmap-broadcast-follow-up) for coverage
and the Go comparison.

The jumbo DEVMAP broadcast migration passed the complete
`direnv exec . make rust-check` gate: formatting, Clippy, userspace and compile-fail
contracts, Rustdoc and all 98 real-kernel tests, with none failed or ignored.
Both backend batches verified all 70 admitted scripts and empty final inventories
and artifact collections. The kernel stage took 1124.80 s (18m45s), compared with
1180.41 s (19m40s) at the ordinary broadcast checkpoint. These are local run
measurements, not a controlled benchmark. Makefile lint and canonical DSL
formatting also passed.
See the [jumbo broadcast follow-up](#jumbo-devmap-broadcast-follow-up) for the
coverage mapping and Go comparison.

The egress testing migration passed the complete `direnv exec . make rust-check`
gate: formatting, Clippy, userspace/compile-fail contracts, Rustdoc and all 98
real-kernel tests, with none failed or ignored. Both backend batches verified
all 78 admitted scripts and empty final inventories/artifact collections. All
sixteen retained egress contracts passed. The kernel stage took 986.98 s
(16m27s), compared with 1124.80 s (18m45s) at the jumbo broadcast checkpoint.
These are local run measurements, not a controlled benchmark. Makefile lint and
canonical DSL formatting also passed.
See the [egress follow-up](#native-egress-testing-follow-up) for the coverage
mapping and Go comparison.

The jumbo PASS/mixed-fragment migration passed the complete
`direnv exec . make rust-check` gate: formatting, Clippy, userspace/compile-fail
contracts, Rustdoc and all 98 kernel tests, with none failed or ignored.
Both backend batches verified all 80 admitted scripts and empty final inventories
and artifacts. The four retained PASS restoration/ABI contracts passed.
The kernel stage took 978.34 s (16m18s), compared with 986.98 s (16m27s) at the
egress checkpoint. These are local measurements, not a controlled benchmark.
Makefile lint and canonical DSL formatting also passed. See the
[PASS follow-up](#jumbo-pass-and-mixed-fragment-testing-follow-up).

The ordinary lifecycle/timing follow-up passed the full
`direnv exec . env BPFMAN_KERNEL_TIMINGS=1 make rust-check lint-make` gate:
formatting, Clippy, userspace/compile-fail contracts, Rustdoc, Makefile lint and
all 96 kernel tests, none failed or ignored. Each backend batch checked all 84
admitted scripts and empty final inventories/artifacts. The kernel stage took
782.36 s (13m02s), versus this session's fresh 1138.08 s (18m58s) baseline.
Serial contract time fell from 751.592 s to 472.738 s; script-batch variation
is reported separately. These are local samples, not a controlled benchmark.
See the [coverage and timing follow-up](#ordinary-lifecycle-and-timing-follow-up).

The contention/isolation follow-up passed the complete timed `rust-check
lint-make` gate: all 96 kernel tests, userspace/compile-fail contracts, formatting,
Clippy, Rustdoc and Makefile lint. Every backend executed all 84 admitted scripts
in isolated runtimes plus four concurrent shared-runtime scripts, with empty
inventories/artifacts. Kernel time fell from 782.36 s (13m02s) to 540.19 s
(9m00s). Acceptance time fell from 309.472 s to 126.418 s; retained serial
contracts fell from 472.738 s to 413.631 s, including broadcast's 130.351 s to
82.874 s. These are local samples, not a controlled benchmark. See the
[contention and fixture-reuse follow-up](#script-contention-isolation-and-broadcast-fixture-reuse-9-october-2026).

The TX/direct REDIRECT and DEVMAP unicast fixture-reuse follow-up passed the same
complete gate, including all 96 kernel tests and both backend script batches.
Both survivor choices share loaded programs and networks while preserving
continuation masks, restoration assertions and 300 ms packet capture windows.
Kernel time fell from 540.19 s (9m00s) to 467.88 s (7m48s); the 20 affected
contracts fell from 216.462 s to 144.647 s. Separate packet-observation and
fixture-teardown phases identify the remaining cost. See the
[forwarding fixture-reuse follow-up](#txdirect-redirect-and-devmap-unicast-fixture-reuse-9-october-2026).

The bounded-parallelism follow-up passed the complete gate with the same 96
kernel tests and both script batches. Two synchronous workers run independent
retained forwarding and broadcast fixtures, while top-level kernel tests and
backend batches stay sequential. Kernel time fell from 467.88 s (7m48s) to
368.81 s (6m09s); the 36 affected contracts fell from 227.230 s to 122.383 s.
Assertions and 300 ms capture windows are unchanged. See the
[concurrency audit and timing evidence](#bounded-parallel-forwarding-fault-fixtures-9-october-2026).

The subsequent concurrent-stage and named-case follow-up passed the full gate:
all 96 kernel tests ran exactly once, backend stages remained sequential, and
both stores passed the complete 84-file corpus with 32 named DEVMAP cases plus
four shared-runtime scripts. Kernel time fell from 368.81 s (6m09s) to 213.75 s
(3m34s), approximately 42%. Retained contracts took 96.87 s and script acceptance
116.83 s. Assertions, fault matrices and capture windows remain intact. See the
[runner abstractions and final evidence](#concurrent-kernel-stages-and-named-script-cases-9-october-2026).

The expanded packet-case follow-up passed the full gate in 193.25 s (3m13s),
with all 96 kernel tests, 84 admitted files and 64 named cases per store.
Isolated acceptance uses 32 Go-runner slots; shared-runtime coverage uses four. The
10 October follow-up overlaps these cohorts in one runner per backend, with shared
execution still capped at four. All 96 kernel tests passed in 163.20 s (2m43s);
acceptance fell to 67.36 s, and explicit cleanup outcomes now accompany process
telemetry. See the [cohort follow-up](#overlapping-script-cohorts-10-october-2026).
The 11 October follow-up adds paired per-test start/end timestamps and raises
independent retained contracts to eight workers. The full gate passed all 96
kernel tests in 150.41 s (2m30s), with retained stages taking 68.96 s versus
104.45 s in the repeated four-worker baseline. Script acceptance took 81.40 s
in this sample. See the [worker timing follow-up](#eight-worker-retained-contracts-11-october-2026).
The subsequent jumbo split passed all 100 kernel tests in 139.55 s (2m20s),
with the same fault matrix. Matching contract-only samples took 60.32 s before
and 60.48 s after: smaller jobs reduced idle worker capacity but did not establish
a wall-time saving. This slice also fixes a pre-bind capture race exposed by
the unchanged baseline. See the
[jumbo scheduling follow-up](#separately-scheduled-jumbo-forwarding-contracts-11-october-2026).
Normal bpffs release waits within a bounded deadline for busy references;
persistent-holder rejection is tested. Assertions and capture windows remain
unchanged. See the
[throughput measurements and cleanup evidence](#expanded-packet-cases-and-script-throughput-9-october-2026).

The replacement fault matrix covers attach and
non-last detach, partial acquisition at either extension slot, rejected and
post-mutation switch failures, publication, restoration, cleanup, cancellation,
foreign-runtime retries, and post-commit retirement. Real packet tests prove
both-member execution, proceed-on stopping the second slot, either survivor,
stable outer identity, and restoration after failed attach/detach publication.
Unload tests cover multiple links per program, either survivor, last-member
detach, blocked program teardown, failed publication/deletion, and retained
program-pin identity. Shared fake tests additionally cover multiple interfaces,
foreign kernel instances, cancellation, and links added during retained recovery.

Both stores run unchanged Go DSL scripts for XDP link round-trip, last detach,
priority ordering, ten-slot capacity refusal, slot reuse, configuration after
detach, default proceed-on rebuilding, and attached-program survivor rebuilding.
Twelve more unchanged scripts run individually on each backend: ten-slot chain
execution, four fill/drain/refill peaks, exact weighted packet counts with
staggered detach, default/custom proceed-on continuation and stopping, mask
encoding, priority-zero/name ordering, independent interfaces, and normal-MTU
`xdp.frags` traffic. The shared
DSL harness checks empty program/link/dispatcher inventories and no owned XDP
revision artifacts after cleanup. These scripts required no production changes.
See the [corpus matrix](../../rust/README.md#xdp-attachment-and-replacement).
Direct adapter tests additionally cover moved pins/parents, undeclared slots,
and recovery after successful updates whose observations fail. Four additional
TX/direct-REDIRECT delivery tests cover both stores and explicit driver/SKB modes.
Each exercises both actions, default stopping or explicit continuation into DROP,
and either survivor. Capture sockets open before transmission and reject outgoing
copies and duplicate frames; exact per-member counters independently prove chain
execution. Exhausting a chain whose last action permits continuation returns PASS,
matching the shared dispatcher used by Go and legacy Rust. No production behavior
changes were needed. See
[packet-delivery acceptance](../../rust/README.md#xdp-tx-and-redirect-packet-delivery).
Four DEVMAP suites additionally exercise live target changes, missing-entry
PASS/DROP fallback, REDIRECT continuation, either survivor, and preservation of
the original pinned map through successful replacement and failed attach/detach
publication. Map updates run inside the target namespace; final reclamation is
observed with a bounded wait for kernel-deferred program/map release. See
[DEVMAP acceptance](../../rust/README.md#xdp-devmap-forwarding).

Four combined multi-buffer forwarding suites run the same direct-delivery and
DEVMAP scenarios on both stores and ingress modes. All interfaces use MTU 9000;
fragment-aware receive dispatchers enable native veth transmission. The raw helper
sends three 8014-byte frames with an offset-dependent payload pattern and checks
all received payload bytes. Exact BPF counters prove total length exceeds linear
length, successful final-byte reads, and correct reads across the linear/fragment
boundary at each executing member and receiving peer. The same proof is required
after successful replacement, failed attach/detach publication restoration,
either survivor, and live DEVMAP changes. No production changes were required.
See [multi-buffer forwarding](../../rust/README.md#xdp-multi-buffer-forwarding).

Eight broadcast suites add a third veth pair and capture both outputs plus the
sender and local ingress. A three-entry DEVMAP uses deliberately out-of-range
lookup key 99, proving broadcast ignores key lookup and PASS fallback. Tests cover
both ingress-exclusion choices, exact per-receiver copies and payloads, sparse maps,
live destination changes, continuation into DROP, either survivor, stable outer
identity, successful replacement, failed attach/detach publication restoration,
and complete cleanup. Ordinary-sized frames fan out successfully. Multi-buffer
frames with one eligible destination still pass full payload/fragment checks;
multiple eligible destinations must report `EOPNOTSUPP` for all three frames and
produce no copies. A managed, map/ifindex-filtered `xdp_redirect_err` observer
verifies the exact errno and refuses other redirect failures, with its field layout
checked against tracefs. The restriction is explicit in the
[Linux 6.18.54 clone paths](https://github.com/gregkh/linux/blob/v6.18.54/kernel/bpf/devmap.c).
No production changes or skipped tests were needed. See
[broadcast acceptance](../../rust/README.md#xdp-devmap-broadcast-and-ingress-exclusion).
The hash-backed suites repeat this lifecycle using three arbitrary keys beyond
capacity three, checking the exact populated key set rather than array slots.
`make rust-test-xdp-broadcast` runs both map types; the eight hash-only suites are
available through `make rust-test-xdp-broadcast-hash`.

Twelve positive egress suites exercise `xdp/devmap` or `xdp.frags/devmap` PASS/DROP
programs through real traffic (four ordinary array-map suites and eight ordinary/
genuine multi-buffer hash-map suites),
live entry updates, successful dispatcher replacement, failed attach/detach
publication restoration, survivor rebuilding, and last detach. The loader keeps
ordinary interface XDP as EXT but loads `xdp/devmap` and `xdp.frags/devmap` as native
XDP with the section's expected attach type. Egress programs cannot join interface
dispatchers, including after store reopen; CPUMAP sections fail preparation before
runtime effects. Native XDP uses the existing load compensation and unload/retry
ownership paths. Removing a program's owned pins/record leaves map-held kernel
references alive; deleting the entry or releasing the final map reference releases
the program. The tests verify both orders and eventual kernel reclamation.

Four additional array-map suites record the multi-buffer egress boundary on both
stores and ingress modes. Aya 0.14 preserves `BPF_F_XDP_HAS_FRAGS` for native XDP but drops it
when overriding a program as an extension. Linux records a linear array DEVMAP owner
and rejects a fragment-capable egress program with `EINVAL`. Rejected updates must
preserve empty/populated entries, dispatcher identity, and successful full-payload
jumbo unicast; the rejected program must never execute. Review of Aya's public API
found no extension flag setter or XDP-to-extension conversion. Keep upstream Aya
unchanged; code TODOs identify the flag preservation needed before adding positive
array DEVMAP multi-buffer egress execution tests. See
[egress acceptance](../../rust/README.md#xdp-devmap-egress-programs).

Hash-backed jumbo egress uses the same public loader and lifecycle. On Linux
6.18.54, [`map_type_contains_progs`](https://github.com/gregkh/linux/blob/v6.18.54/include/linux/bpf.h#L2151)
omits DEVMAP_HASH, so loading the ingress extension does not initialize hash
ownership. The first native egress insertion initializes compatible ownership;
[`bpf_prog_map_compatible`](https://github.com/gregkh/linux/blob/v6.18.54/kernel/bpf/core.c#L2308)
still rejects later programs with mismatched fragment flags. Positive tests prove
both ordinary and genuine multi-buffer PASS/DROP, exact context and helper reads,
live updates at capacity two, failed updates preserving both sparse entries,
replacement/restoration, and map-held lifetime through managed unload and retry.
Closing the retained map releases the final DROP program. This is acceptance of
existing behavior on the tested kernel, not a repair of Aya's extension flags or
a claim that array DEVMAP supports jumbo egress.

The full gate uses the shared Go/Rust kernel-build helper outside the sandbox,
allowing Nix to discover and realize the matching development output; see the
`KERNEL_DEV` and NixOS guidance in `rust/AGENTS.md`.

`Bpfman<S, K>` uses one kernel backend throughout. Aya and BPF syscalls remain in
`bpfman-kernel-aya`; descriptor-confined filesystem authority stays in
`bpfman-fs`. Persistence, bytecode publication, and runtime locking remain real
in fake-kernel tests. Namespace acceptance includes the unchanged namespace
round-trip/rebuild scripts, path replacement and disappearance, identical
interface indices across namespaces, publication failure and retained unload,
and caller-namespace isolation.

### Where Rust goes beyond Go

The new Rust implementation now provides additional managed XDP behavior beyond
Go. The clearest addition in `e64d4ccb2` is native DEVMAP egress support:

- **DEVMAP egress programs:** Rust distinguishes interface programs from
  `xdp/devmap` and `xdp.frags/devmap` selections. Interface programs load as EXT;
  egress programs load as native XDP with `BPF_XDP_DEVMAP`. Go's
  [loader](../../platform/ebpf/load.go) sets every managed XDP selection to EXT
  and clears its attach type, so it cannot load the native egress role. Rust
  proves ordinary-frame PASS/DROP forwarding, ingress/output interface context,
  rollback, and map-held program lifetime through managed unload on both stores
  and driver/SKB ingress. Cilium/ebpf and Linux support the underlying mechanism;
  this difference is in the managers' load and lifecycle paths.
- **Configured attachment modes:** Rust exposes driver, generic SKB, and hardware
  requests with the established fallback behavior, following the legacy Rust
  implementation. Go's [attachment path](../../platform/ebpf/attach_xdp.go)
  supplies no mode flag to `link.AttachXDP`, relying on default attachment
  behavior without per-interface mode configuration. Actual hardware offload
  remains unverified.
- **Fragment-aware dispatcher chains:** Rust applies libxdp's all-members fragment
  policy, selecting a fragment-aware dispatcher only when every member declares
  support. Real native/SKB jumbo tests verify helper reads, forwarding, replacement,
  and recovery. Go's [dispatcher configuration](../../dispatcher/dispatcher.go)
  currently leaves its fragment flag zero; successful loading of an `xdp.frags`
  selection does not establish this packet-path behavior.

The TX, redirect, and DEVMAP/DEVMAP_HASH unicast and broadcast work expanded
packet and lifecycle acceptance for existing Rust behavior. Egress adds a
production loader capability. DEVMAP entry updates still use a test helper;
there is no production map-editing API or shared-map protocol in this slice.
Array DEVMAP multi-buffer egress pairing awaits upstream Aya extension flag
preservation,
and Linux 6.18.54 rejects
multi-buffer fan-out. These boundaries are covered by explicit rejection tests.

Go remains broader overall, including other program families, shared maps, OCI,
and gRPC, and remains the behavioural authority for the existing shared surface.
Rust's XDP additions do not complete overall feature parity or establish physical
NIC execution and hardware-offload support.

The multi-buffer checkpoint (`d5c4652ae`) also passed Go `test-all`: package tests,
lint, script acceptance, kernel tests, and gRPC concurrency. This includes the unchanged
`TestLoad_XDPFragsProgram` script. The Nix development shell required
`GOFLAGS=-ldflags=-linkmode=external`; existing image, policy, and shared-runtime
skips remain. This validates the existing Go test surface; new multi-buffer,
direct-delivery, DEVMAP, multi-buffer forwarding, and broadcast assertions run
separately against Rust.

## Summary

This document proposes a new Rust implementation of bpfman based on the
architecture and behaviour of the current Go implementation.

The repository already contains two valuable bodies of work:

- the current Go implementation, whose domain model, lifecycle ordering,
  persistence model, and external behaviour are the specification for the new
  implementation; and
- the legacy Rust implementation, which contains useful knowledge about Aya,
  eBPF program loading and attachment, dispatchers, netlink, namespaces, OCI
  images, packaging, and integration with the rest of the project.

The new implementation will be developed beside both of them. It will not
compile, link, or otherwise depend on the legacy Rust crates. The legacy source
will remain in the tree as reference material. This permits a clean design
without losing access to either the working Go behaviour or the accumulated
low-level Rust knowledge.

The design follows a SANS-I/O model similar to that used by `rty`: difficult
lifecycle decisions live in pure Rust crates, while small interpreters at the
edges perform kernel, database, filesystem, image-registry, and network I/O.
The crate dependency graph will enforce this separation.

## Goals

- Reimplement the current Go behaviour in Rust without reviving the legacy
  Rust architecture.
- Keep the Go and legacy Rust sources available throughout development.
- Preserve compatibility with the current public API, SQLite schema, bpffs
  layout, dispatcher ABI, image format, and operational behaviour.
- Put lifecycle decisions, rollback policy, and dispatcher planning in pure,
  deterministic code.
- Use Rust's type system and ownership model to make invalid states and invalid
  resource operations difficult or impossible to express.
- Make failure at every I/O boundary straightforward to simulate and test.
- Enforce crate layering and SANS-I/O dependency closure mechanically.
- Keep public APIs small and introduce abstractions only at real boundaries.

## Non-goals

- Translating the Go source file by file.
- Translating every Go interface into a Rust trait.
- Building, linking, or incrementally refactoring the legacy Rust crates.
- Restoring sled as bpfman's persistent store.
- Changing public behaviour merely to make the initial Rust implementation
  easier.
- Replacing proven external formats or protocols during the initial parity
  effort.
- Introducing a durable operation journal before behavioural parity. The
  architecture should permit one later, but it is not a prerequisite for the
  first replacement implementation.

## Source-tree coexistence

The three implementations have deliberately different roles:

| Source | Role | Built by the new workspace |
| --- | --- | --- |
| Go implementation | Behavioural specification and production reference | No |
| Legacy Rust implementation | Source of low-level mechanisms and historical knowledge | No |
| New Rust implementation | Replacement implementation | Yes |

The new implementation will use an independent nested workspace rooted at
`rust/`:

```text
Cargo.toml                 legacy Rust workspace; retained unchanged
bpfman/                    legacy Rust implementation
platform/, manager/, ...   current Go implementation
rust/
  Cargo.toml               new Rust workspace
  crates/                  new implementation crates
```

All new Rust build, test, lint, and metadata commands must identify
`rust/Cargo.toml` explicitly. New CI and Make targets must do the same. A plain
Cargo command at the repository root addresses the legacy workspace and must
not be part of the new implementation's workflow.

The new workspace must have no path dependency on a legacy Rust crate. Code may
be studied and mechanisms may be rewritten, but the resulting implementation
must enter through the new crate boundaries. Assets or fixtures that genuinely
need to be shared should be moved or copied into an explicitly neutral location
with their provenance recorded; they must not be obtained by compiling a
legacy crate.

A workspace-structure test will assert that:

- every new workspace member is under `rust/`;
- no normal, development, or build path dependency reaches a legacy crate;
- every workspace crate is assigned a dependency tier; and
- every dependency edge points in the permitted direction.

Removal of the legacy Rust source is explicitly outside this project.

## Existing designs used as input

### Go implementation

The Go implementation establishes the desired high-level design:

- [`platform/interfaces.go`](https://github.com/bpfman/bpfman/blob/main/platform/interfaces.go)
  defines the I/O boundaries.
- [`manager/doc.go`](https://github.com/bpfman/bpfman/blob/main/manager/doc.go)
  describes fetch, compute, and execute, including compensating rollback.
- [`manager/action/`](https://github.com/bpfman/bpfman/tree/main/manager/action)
  represents effects as data and interprets them in one place.
- [`manager/attach_simple.go`](https://github.com/bpfman/bpfman/blob/main/manager/attach_simple.go)
  records a pending link before creating its bpffs pin and finalises it after
  attachment.
- [`manager/executor_dispatcher.go`](https://github.com/bpfman/bpfman/blob/main/manager/executor_dispatcher.go)
  stages dispatcher revisions, swaps live attachments, persists complete
  snapshots, and rolls back failed replacements.
- [`platform/store/sqlite/`](https://github.com/bpfman/bpfman/tree/main/platform/store/sqlite)
  makes exported store methods atomic domain operations and uses larger
  transactions only to compose those operations.
- [`cmd/bpfman-shell/`](https://github.com/bpfman/bpfman/tree/main/cmd/bpfman-shell)
  and the scripts under
  [`e2e/scripts/`](https://github.com/bpfman/bpfman/tree/main/e2e/scripts)
  provide an implementation-neutral, outside-in behavioural contract through
  the `bpfman` CLI.

The new implementation should preserve these semantics, not necessarily these
exact abstractions.

### Legacy Rust implementation

The legacy Rust implementation is particularly useful for:

- Aya APIs and conversions;
- eBPF program loading and pinning;
- XDP and TC dispatcher bytecode and ABI layouts;
- netlink and network-namespace mechanics;
- OCI authentication and signature verification;
- the namespace helper, protobuf history, packaging, and integration tests.

It is not the architectural base. Persistent storage is embedded directly in
domain objects, and dispatcher code combines sled, Aya objects, filesystem
mutation, and lifecycle decisions. For example, `ProgramData` wraps a
`sled::Tree` in
[`bpfman/src/types.rs`](https://github.com/bpfman/bpfman/blob/main/bpfman/src/types.rs),
while the dispatcher implementation combines database and kernel state in
[`bpfman/src/multiprog/`](https://github.com/bpfman/bpfman/tree/main/bpfman/src/multiprog).

The rule for reuse is:

> Reuse syscall knowledge and tested algorithms; rewrite ownership and
> orchestration.

## Architectural principles

### Pure decisions, effectful interpreters

The core decides what must happen but cannot perform I/O. It receives events or
complete observations and emits commands describing effects. The interpreter
executes those commands and returns classified results.

The pure dependency closure must contain no kernel bindings, SQL driver,
filesystem access, network client, async runtime, clock, process API, or global
configuration.

### Use state machines where state is the problem

Bpfman operations are finite workflows, not all long-lived machines. SANS-I/O
must not become ceremony.

Use operation-specific state machines for workflows in which later actions
depend on earlier I/O results or in which rollback state accumulates, including:

- program load and batch publication;
- standalone attach and detach;
- program unload and shared-map cleanup;
- XDP and TC dispatcher rebuild and removal; and
- reconciliation of store, kernel, and bpffs observations.

Use ordinary pure functions when all required observations are already
available, including:

- request validation and refinement;
- dispatcher member ordering and configuration;
- attach compatibility checks;
- map-ownership decisions; and
- list filtering and view construction.

### Closed vocabularies are enums

Program kinds, attach specifications, dispatcher bindings, lifecycle states,
effects, and effect results are closed vocabularies. They should be enums with
exhaustive matches rather than trait-object hierarchies or parallel optional
fields.

### Parse, do not validate

External requests are transport data. They must be converted at the boundary
into domain values whose constructors establish their invariants. The core must
not repeatedly check whether an interface name is empty, a priority is
negative, a dispatcher slot is out of range, or an attach specification is
missing a required field.

### Traits belong at genuine substitution boundaries

The Go package defines many useful narrow interfaces and then composes them
into `Store` and `KernelOperations`. The new implementation must not reproduce
those aggregate interfaces as large Rust traits.

Most core testability comes from pure effect values, not mocked traits. Where
multiple interpreters are genuinely useful, define a small trait near the
consumer and prefer static dispatch. Dynamic dispatch should normally appear
only at a composition root or an explicitly pluggable policy boundary.

### Atomic store verbs, not a general transaction interface

The store adapter should expose domain operations whose names carry their
atomicity, such as:

- `commit_load_batch`;
- `create_pending_link`;
- `finalise_link`;
- `replace_dispatcher_snapshot`;
- `delete_dispatcher_snapshot`; and
- `remove_program_and_map_references`.

The SQLite adapter owns the statements, transaction, busy handling, and retry.
The core must not pass an arbitrary callback into a transaction or have its
decision logic rerun implicitly by the database layer.

### Replaceable persistence backend

Persistence is selected at the binary composition root. `bpfman-store` owns
backend-independent `OpenStore`, `ProgramReader`, `CommitLoad`, `UnloadStore`,
`LinkReader`, `LinkStore`, `XdpReader`, and `XdpStore` contracts. Runtime operations
are generic over the capabilities they consume;
they neither import a backend nor know its storage format or version.
`bpfman-store-sqlite` implements the contracts and owns all SQL, schema checks,
transactions, and concrete receipt evidence. `bpfman-store-json` implements the
same contracts with atomic whole-file publication. Both adapters and the runtime
are peers in the dependency tiers, depending on the contracts below them.

This boundary supports a serialized JSON file without changing lifecycle
orchestration. Each implementation must still provide atomic visibility, writer
coordination, ownership revalidation, classified failures, and an unambiguous
commit result. A successful commit must never subsequently be returned as a
pre-commit error authorizing compensation. A backend must resolve that outcome
inside its operation rather than leaving runtime to infer it from an I/O error.

Unload uses associated, non-cloneable program and map-set receipts. Failures
return those receipts with their causes. An explicit retry supplies the same
backend and runtime authority; the backend validates evidence again before
mutation. No generic transaction callback, SQL row, or file-format payload
appears in the contracts. Shared tests exercise both real persistent backends.
Failure decorators may intercept individual store operations, but never implement
storage, atomicity,
or receipt validation themselves. No fake store is needed; SQLite remains the
default selection for Go interoperability.

### Ownership within an adapter, compensation across adapters

Use RAII for local handles inside an adapter operation. Persistent pins and
filesystem artifacts require explicit, fallible cleanup: dropping a temporary
directory or a pinned handle is not proof that its cleanup succeeded.

Rollback across kernel, bpffs, SQLite, and the bytecode store must remain
explicit. `Drop` cannot communicate a cleanup error and must not hide a failed
cross-subsystem compensation. The operation machine records acquired resources
and emits rollback effects in the required order.

For single-program loading, keep forward dependencies in consuming typed
continuations and use a small compensation instruction set: remove owned
bytecode, remove an owned program pin, and remove one owned map pin. This is
VM-like routing of domain effects, not a bytecode VM, register file, or generic
transaction framework. Raw unlink/rmdir operations remain private to
`bpfman-fs`; atomic store operations remain inside the SQLite adapter.

Instructions carry non-cloneable ownership receipts, not merely paths or IDs.
The adapter consumes a receipt on successful removal and returns unresolved
ownership with its error on failure. Partial forward failures likewise return
unresolved acquisitions. Borrowed/shared resources do not confer deletion
authority. Removing a pin does not imply destruction of the kernel object.

The runtime drives a narrow `LoadCleanup` trait, injected for tests, under
`RuntimeWriter` authority. Both the driver and each mutating method require the
writer. A real implementation must delegate managed-object I/O to `bpfman-fs`
and verify receipt/root identity. The fake exercises the same driver; it does
not replace confinement or real-kernel testing. The local tracepoint slice now
has real pin/bytecode receipts and removal. Map-directory cleanup remains
separate dependent work: it is attempted only after all map-pin instructions
have succeeded, with blocked ownership retained for explicit retry.

Every independent compensation is attempted once per pass. Failure records its
error and unresolved receipt, then continues; it does not propagate early or
retry inline. The terminal report retains the primary failure, all attempt
history, and unresolved instructions with stable per-operation identities.
Explicit retries run only unresolved work and preserve earlier error history.
Successful receipts cannot re-enter the retry set through the report.

Do not add dependent cleanup, such as removing a containing directory or
releasing shared ownership, to this independent instruction set without
modelling its prerequisites and blocked work. Cancellation budgets, process
termination and crash reconciliation remain interpreter/recovery concerns;
type-level sequencing alone cannot guarantee cleanup eventually succeeds.

## Proposed crate structure

The initial structure should remain small enough that every boundary is
meaningful:

| Crate | Purity | Responsibility |
| --- | --- | --- |
| `bpfman-model` | Pure | IDs, validated requests, records, snapshots, paths, domain errors, and public views |
| `bpfman-core` | Pure | Lifecycle machines, effect vocabulary, dispatcher planning, reconciliation, and rollback policy |
| `bpfman-fs` | Effectful | Runtime layout, bpffs paths and scanning, bytecode publication, and readiness capabilities |
| `bpfman-lock` | Effectful | Go-compatible writer locking, borrowed write permits, and inherited descriptor ownership |
| `bpfman-store` | Boundary | Backend-independent read, atomic commit, and conditional teardown contracts |
| `bpfman-store-sqlite` | Effectful | SQLite schema, migrations, queries, and atomic persistence operations |
| `bpfman-store-json` | Effectful | Versioned JSON snapshots, atomic publication, and conditional teardown |
| `bpfman-kernel` | Boundary | Backend-independent kernel capabilities, observations, errors, and opaque ownership contracts |
| `bpfman-kernel-aya` | Effectful | Concrete Aya/Linux implementation; owns Aya types and BPF syscall details |
| `bpfman-image-oci` | Effectful | OCI pull, cache, authentication, and signature-policy adapters |
| `bpfman-runtime` | Interpreter | Drives core machines, routes effects, retains effect error sources, and exposes the `Bpfman` application API |
| `bpfman-proto` | Boundary | Generated protobuf vocabulary only; not the domain model |
| `bpfman-api` | Front end | gRPC request conversion, status mapping, and server implementation |
| `bpfman-csi` | Front end | CSI integration using narrow runtime capabilities |
| `bpfman` | Composition root | CLI, daemon mode, namespace-helper mode, configuration, logging, and dependency construction |

The kernel split is implemented for the currently supported operations.
`bpfman-kernel` defines portable observations and lifecycle capabilities;
`bpfman-kernel-aya` owns ELF parsing, loading, concrete handles, and BPF syscalls.
Runtime and filesystem normal dependency closures cannot reach Aya or aya-obj.

This is a starting point, not a target crate count. A crate should be split when
doing so enforces a dependency rule, isolates a portability constraint, or
creates a reusable boundary. Shared process fixtures may later justify a
`bpfman-test-common` crate.

The generated protobuf vocabulary must not leak into `bpfman-model`. The API
crate converts protobuf requests into validated domain requests and converts
domain results back to protobuf responses.

## Dependency rules

The intended direction is:

```text
bpfman-model
      ^
      |
bpfman-core                 bpfman-proto
      ^                           ^
      |                           |
effect adapters ---------------  |
      ^                           |
      |                           |
bpfman-runtime ------------------+
      ^
      |
front ends
      ^
      |
bpfman binary
```

More concretely:

- `bpfman-model` depends only on vetted pure libraries.
- `bpfman-core` depends on `bpfman-model` and vetted pure libraries.
- Effect adapters may depend on `bpfman-model` and the effect vocabulary
  exposed by `bpfman-core`.
- Effect adapters must not depend on one another.
- `bpfman-runtime` composes effects through narrow contracts. Persistence and
  kernel implementations are selected by the binary and must not enter runtime's
  normal dependency closure.
- Aya and aya-obj belong to the concrete kernel adapter's normal dependency closure.
  Model, core, runtime, store contracts, and filesystem capability contracts must
  not import their types or expose them in signatures.
- Front ends depend on runtime and boundary-specific types, never on Aya or
  SQLite.
- The binary is the only general composition root.

As in `rty`, tests will inspect Cargo metadata to enforce tiers and will
allow-list the complete normal-dependency closure of every SANS-I/O crate.

## Domain model

Rust should encode distinctions that the Go implementation currently enforces
by convention.

Candidate types include:

- `KernelProgramId`, `KernelLinkId`, `KernelMapId`, and `ManagedLinkId`;
- `ProgramPin`, `LinkPin`, `MapPin`, and `MapDirectory`;
- `DispatcherRevision` and bounded `DispatcherSlot`;
- `ProgramName`, `InterfaceName`, and a resolved `NetworkInterface`;
- `PendingLink` and `AttachedLink`;
- `DispatcherBinding::Xdp { link_id }` and
  `DispatcherBinding::Tc { priority, handle }`; and
- `ProgramSource::File` and `ProgramSource::Image`.

Fields should be private unless callers can change them without violating an
invariant. Constructors and `TryFrom` implementations refine transport or
storage data into domain data.

Persisted rows, protobuf messages, and kernel observations are different
representations and must not be collapsed into one universal structure.

## Effect model

The exact API should be proven with a vertical slice before it is generalised.
Conceptually, an operation receives an event and returns a transition:

```rust
enum Transition<O, E> {
    Effects(Vec<E>),
    Complete(O),
    Failed(OperationFailure),
}
```

Each complex operation should use its own state and effect vocabulary. A
standalone attach might use effects corresponding to:

1. read the managed program;
2. create a pending link record and allocate its durable ID;
3. attach and pin the kernel link at the recorded path;
4. finalise the record with the observed kernel link ID;
5. detach and unpin during rollback; and
6. delete the pending record during rollback.

The state variant contains only the information valid at that point. A pending
record cannot be mistaken for a finalised record, and a rollback cannot try to
release a resource that was never acquired.

Effects and their successful observations should be serialisable where useful.
This permits deterministic event traces and turns field failures into replayable
core tests. Backend error objects themselves remain at the runtime edge; the
core receives only the classifications on which it can legitimately decide.

Independent effects may be returned as a batch. Effects with data dependencies
remain sequential. The design must not build a generic workflow framework
before two or more operations demonstrate the same abstraction.

## Kernel and bpffs adapter

### Injectable kernel boundary

`Bpfman<S, K>` receives one kernel backend alongside the real store. The public
application drives the existing production interpreters for both concrete and
fake kernels. Small capabilities separate program/map/link observations, local
object validation, program loading and resources, tracepoint links, and XDP
lifecycle operations. Each operation composes only the traits it needs.

`PreparedProgram::new(&kernel, ...)` captures input and validates it before
runtime initialization. Globals and additional selections use the same local
validation capability. Prepared inputs contain captured bytes and domain values;
they carry neither a runtime selection nor live Aya objects.

Aya, aya-obj, concrete program/map/link handles, and BPF syscalls live exclusively
in `bpfman-kernel-aya`. Runtime uses classified kernel errors with private concrete
causes in diagnostic source chains. Associated types carry opaque live handles
and non-cloneable ownership receipts through partial failures and explicit retries.

`bpfman-fs` retains runtime-root verification, descriptor-relative traversal,
pin-path authority, and managed-object removal. Its narrow kernel bridge lends
non-forgeable `PinTarget` and `PinSource` values to opaque handles, preserving
filesystem authority without exposing Aya types or arbitrary path-plus-permit
mutations. The concrete kernel adapter implements this bridge; runtime calls only
the higher-level lifecycle contracts. The filesystem crate forbids unsafe code.

The shared fake in `tests/support/kernel.rs` tracks IDs, live handles, program/map
and link pins, dispatcher targets, resource generations, and runtime/backend
identity. `tests/kernel_lifecycle.rs` exercises public lifecycle and failure
scenarios with SQLite and JSON, real runtime locking and bytecode publication,
and the forwarding store fault decorator. Tests cover partial acquisitions,
synchronous outer detach with a retained handle, blocked dependent cleanup,
cancellation, post-commit failure, and foreign-instance retries. No fake store is
needed. Run the focused suite with `direnv exec . make rust-test-kernel-fake`.

Workspace dependency laws prevent Aya/aya-obj from re-entering runtime or
filesystem APIs. The full `rust-check` gate retains operation-level fault tests,
filesystem confinement checks, real-kernel tests, and unchanged admitted DSL
scripts on both stores. A fake cannot establish verifier, syscall, confinement,
or actual kernel-lifetime guarantees. CLI behaviour and persistence formats
remain unchanged.

### Rust generics at the backend boundary

Use generics to express the store and kernel dependencies, conceptually
`Bpfman<S, K>`. Each operation should require only the capability traits it uses.
The application uses this generic shape for observations, loading, attachment,
and cleanup, including explicit retries.

Associated types on the relevant capabilities keep backend-owned resources opaque:
for example, `K::Loaded` and `K::LinkPin`. The concrete adapter owns Aya objects;
the fake owns simulated resources. Runtime orchestration uses the same contracts
for both. These types must encapsulate implementation details rather than expose
Aya through public aliases or accessors.

Ownership is part of those contracts. Operations consume owned handles or receipts,
and failures return unresolved ownership with their causes. Non-`Copy`,
non-`Clone` receipts and consuming continuations let the compiler reject reuse
after consumption. Preserve this across the generic boundary, including partial
acquisition and explicit cleanup retries.

Keep small capability traits and operation-specific effect interfaces. Introduce
type parameters where they express substitution or a useful type relationship;
avoid propagating long lists of receipt parameters through application APIs.
Associated types and private aliases should keep those relationships local and
readable. Test the same public application orchestration with both stores and a
shared fake kernel.

Generic parameters distinguish backend types; they do not distinguish two
instances of the same backend or two runtime roots. Opaque receipts must still
carry the identity evidence needed to reject a different runtime, backend
instance where relevant, or changed resource. Compile-time ownership checks do
not replace runtime revalidation or prove that a kernel object has disappeared.

### Adapter operation shape

The kernel adapter should expose complete, resource-safe operations rather than
individual low-level syscalls. Examples include:

- load and pin one selected program and its maps;
- attach and pin a standalone link;
- load and pin a configured dispatcher revision;
- attach and pin all freplace extensions for a revision;
- atomically update an XDP dispatcher link;
- create or remove a precisely identified TC filter; and
- inspect programs, links, maps, pins, and tracepoints.

Aya objects and file descriptors must not cross into the pure core. The adapter
returns stable observations: IDs, typed pin paths, handles, priorities, and
diagnostics.

Operations that create several kernel resources should use ownership guards so
failure before return does not leak a partially created resource set. Once a
successful observation has crossed the adapter boundary, the core owns the
decision to retain or compensate it.

Any necessary `unsafe` code must be isolated in the smallest possible adapter
crate or module, documented with its safety invariant, and forbidden throughout
the rest of the workspace.

## Store adapter

The implementation supports SQLite and JSON behind the same store contracts.
SQLite retains Go schema version 2 and its migration history. JSON format 4 adds
complete single-member XDP dispatcher snapshots. Older JSON formats retain their
existing operations: tracepoint programs from version 1, tracepoint links from
version 2, and XDP loads from version 3. XDP attachment requires a separately
initialized version 4 or newer runtime. Format 5 adds multi-member replacement;
format 6 adds explicit network-namespace paths; format 7 adds TC extension loads
and singleton ingress snapshots. New stores use format 8, which adds complete TC
revisions. Format 7 retains its singleton operations and refuses replacement;
it never upgrades implicitly. Formats 1–6
retain their existing operations and refuse TC without upgrading. Formats 4 and 5
refuse namespaced attachments; format 4 also refuses replacement. Neither backend
implicitly migrates or repairs existing state. The implementation does not use sled.

The store has two classes of operation:

- observations, which return complete domain snapshots suitable for a decision;
  and
- atomic mutations, which preserve their named invariant for every caller.

The adapter is responsible for WAL configuration, foreign keys, prepared
statements, transaction mode, busy timeout, bounded retry, and migration
validation. A retry repeats only an adapter-owned atomic operation, never an
arbitrary core callback.

Where several reads must describe one point in time, expose a cohesive snapshot
query or execute those reads in an adapter-owned read transaction.

## Dispatcher lifecycle

### Implemented: XDP first attach and last detach

The model owns validated interface names, proceed-on masks, the namespace/interface
key, and one-slot dispatcher configuration. A private runtime interpreter performs
forward acquisitions; the pure core owns cleanup ordering and consuming
continuations. Aya objects and filesystem/store receipts remain outside the model.

First attach resolves the interface in the selected network namespace and adopts
the managed EXT program. Under one writer lock it checks that the attach point is
vacant, loads a configured revision, pins the dispatcher and extension link, and
creates and pins the outer interface link. It then publishes the complete snapshot
atomically. SQLite commits the dispatcher header, managed link, and XDP details in
one transaction; JSON publishes one complete file. No pending standalone-link
record is used for this operation. A successful commit ends compensation, including
when cancellation arrives during that commit.

Last detach observes the complete stored snapshot and validates every present
artifact before mutation. It synchronously detaches the outer BPF link before
removing its pin, so an observer retaining another descriptor cannot keep the
interface attachment active. Extension-link and dispatcher-program cleanup are
independent once traffic has stopped. Revision-directory deletion waits for both;
conditional snapshot deletion waits for all owned artifacts. A missing outer pin
is accepted only when its kernel link is absent or proven detached. Mismatched
pins, unexpected revision children, and changed store evidence are refused.

Forward failures retain the original error, unresolved ownership, and every cleanup
attempt. Each pass attempts independent work once, retains blocked dependents, and
never retries inline. `retry_xdp_cleanup` performs one explicit pass over retained
receipts under the same runtime authority. Cancellation can stop admission or
forward attachment before commit; admitted cleanup runs to completion. The CLI
reports one pass and does not persist a recovery queue.

`bpfman-fs` owns descriptor-confined XDP artifact operations and typed removal
receipts. Private `syscall.rs` and `pin_syscall.rs` modules in `bpfman-kernel-aya`
contain the narrowly reviewed unsafe boundary for observation, link creation,
fd-preserving pinning, and synchronous detach. Workspace-law tests preserve all
other lint gates; the filesystem crate inherits the workspace unsafe prohibition.

### XDP dispatcher replacement checkpoint

The implemented milestone attaches a second program to an existing managed XDP
dispatcher, removes either member while the survivor remains active, and finally
detaches the last member. Each nonempty membership change stages a complete new
revision and updates the existing durable outer link. Foreign attachments remain
refused. Current and explicitly selected network namespaces accept `skb`, `drv`,
and `hw` requests. A failed non-SKB first attach retries in SKB mode;
replacement reuses the existing pinned outer link. Driver/SKB traffic and hardware
request fallback are verified on veth; actual offload is unverified.
Attached-program unload uses the same protocol.

Dispatcher replacement exercises the architecture across three adapters. The core owns:

- the attach-point key;
- ordering by priority and deterministic tie-breaker;
- slot allocation and the maximum-program invariant;
- proceed-on mask construction;
- revision selection;
- the complete desired snapshot; and
- the rollback decision for each failure point.

The kernel adapter owns how a revision is loaded, pinned, attached, swapped, and
removed. The store adapter owns atomic replacement of the dispatcher snapshot
and member records.

Replacement must preserve the Go ordering:

1. observe the existing snapshot;
2. compute the complete desired member set and next revision;
3. load and pin the new dispatcher;
4. attach and pin every extension to it;
5. update the existing live XDP link to target the staged dispatcher;
6. atomically persist the complete new snapshot;
7. restore the old live attachment if persistence fails;
8. remove the new resources only after proving they are no longer the live
   target; and
9. remove the old revision after successful persistence.

The store snapshot and live kernel target cannot change atomically together.
Retain the old revision until publication succeeds. The switch capability must
distinguish rejection before mutation from failure after changing the target,
returning ownership and evidence for restoration in the latter case. A rejected
switch leaves the old snapshot and attachment authoritative. If publication fails
after switching, restore the previous target before cleaning the staged revision. If restoration
fails, retain the ownership and evidence required to retry safely; do not discard
resources that may still serve traffic. Preserve the original error and every
cleanup attempt. After successful publication, old-revision cleanup failures must
retain retryable ownership without rolling back the committed membership.

Cancellation before publication follows the same compensation path, including
restoration after a successful switch. An in-flight store commit determines its
own outcome. Cleanup and restoration must finish their admitted pass even when
the forward token is cancelled; retries remain explicit and caller-budgeted.

Implementation and validation checkpoints:

1. Implemented: pure planning and transition tests for deterministic ordering,
   bounded slots, proceed-on configuration, revision selection, and rollback
   dependencies. `plan_xdp_membership` preserves Go's stable ordering by priority,
   unattached before attached, and program name. `XdpReplacement` distinguishes
   rejected switches from failures after mutation and gates staged cleanup on
   successful restoration. `XdpCleanup` distinguishes individual member cleanup
   attempts by stable instruction IDs. Run `direnv exec . make rust-test-xdp-core`.
2. Store portion implemented: `XdpDispatcherSnapshot` validates complete membership
   in contiguous slot order. `XdpReplacementStore` observes and atomically replaces
   an unchanged snapshot under runtime writer authority, retaining receipts on
   failure. Surviving links preserve managed IDs, programs, metadata, timestamps,
   priorities, and proceed-on masks; their slots, kernel extension links, and
   revision change together. SQLite uses schema 2 transactions; new JSON stores
   use format 8; format 7 retains singleton TC; format 6 retains namespace-aware XDP; format 5 retains current-namespace replacement support, with no
   implicit upgrade of older stores. Run
   `direnv exec . make rust-test-xdp-store` for shared contracts and backend faults.
   Kernel portion implemented: `bpfman_kernel::XdpReplacement` loads the complete
   configuration, stages any validated slot/revision, adopts all declared members,
   and conditionally updates the existing outer link using `BPF_F_REPLACE`.
   Rejection returns no switch receipt; post-mutation failure retains both target
   descriptors for restoration. Restoration validates runtime, pin, link, and
   live target, accepts already-restored state on explicit retry, and refuses
   unrelated targets. Run `direnv exec . make rust-test-xdp-switch` for the real
   adapter contracts on both stores. These tests call the adapters directly;
   runtime coverage is listed separately below.
3. Implemented: the public one → two → one → zero lifecycle uses the shared
   stateful fake and both real stores. The fault matrix crosses staging failures
   with cleanup failures for attach and non-last detach, including partial
   acquisitions at either staged extension slot. It checks restoration before
   cleanup, repeated and foreign-runtime retries, cancelled admission, original
   causes and attempt history, and commit winning late cancellation.
4. Implemented: runtime-driven real-kernel traffic tests prove both-member
   execution, proceed-on stopping the second slot, either survivor, durable
   outer identity, and failed attach/detach publication restoring the old chain.
   Direct adapter traffic and identity/failure tests remain separate coverage.
5. Implemented: unchanged Go priority-ordering, slot-reuse, ten-slot capacity,
   configuration-after-detach, and default-proceed-on rebuild scripts run on
   both stores in `rust-check`. The batched `rust-test-scripts` gate
   also admits fill/drain/refill, ten-slot traffic, exact counters, proceed-on
   chains and encoding, priority-zero/name ordering, independent interfaces, and normal-MTU
`xdp.frags` traffic.

Use the forwarding store fault decorator for publication and deletion failures;
no fake store is needed. SQLite `:memory:` remains suitable for adapter tests;
use temporary files for the shared lifecycle's reopening, locking, and receipt
identity checks. Fake-kernel tests establish orchestration and simulated resource
ownership; real-kernel traffic and lifetime tests establish execution behaviour.

TC replacement now persists and reuses the exact kernel-assigned filter handle.
Its signed 84-byte CONFIG remains separate from XDP configuration.

## Filesystem and locking capabilities

Runtime layout is configuration, not a package global. `bpfman-fs` owns an
immutable `RuntimeLayout`, constructed through `TryFrom<PathBuf>` with private
fields and no invalid default value. It validates an absolute, nonempty root
and refuses filesystem-root aliases, then normalises it lexically like Go's
`Layout`, without filesystem access or
symlink resolution. Native non-UTF-8 paths remain supported.

Clap constructs this type at the input boundary. Application operations accept
`&RuntimeLayout`; they obtain the writer lock and database paths through
`lock_path()` and `database_path()`, never by spelling out `.lock` or
`db/store.db` themselves. The default root is defined alongside the layout.
Add further accessors when their consumers exist.

A layout describes where files belong, not whether they exist.
`RuntimeDirectory::open_or_create` refines it into an opened root capability,
without acquiring a lock. This is not `ReadyRuntime`: it proves neither bpffs
mount readiness nor managed-object existence. Future readiness types should
express those additional observations separately.

Mutation entry points requiring serialisation borrow `&RuntimeWriter<'_>` or
are methods on that writer, rather than accepting a target path and an unrelated
lock. `RuntimeDirectory::with_writer` acquires the runtime's lock, rechecks its
inode identity, prepares the database directory, and only then lends the writer
to a callback. It cannot be constructed or cloned by a caller, or escape that
callback. The writer contains both the opened runtime and its low-level
`WritePermit`; the target cannot be substituted. Nested operations borrow the
same writer, rather than acquiring another lock. For example, database creation
is `create_if_missing(&RuntimeWriter)`, not `create_if_missing(path, permit)`.

Opening/creating the root and lock file are necessarily lockless bootstrap
operations. Database-directory preparation requires a writer internally; no
public generic mkdir/delete API is exposed. Raw implementation helpers remain
private. Compile-fail tests exercise authority construction, cloning, escape,
unlocked calls, and the obsolete arbitrary-path-plus-permit combination.

`bpfman-lock` still provides the lower-level `with_write_lock`,
`with_write_lock_file`, and `InheritedWriteLock::with_permit` operations.
Their `WritePermit` proves ownership of one file lock, not authority for any
particular runtime. The filesystem capability acquires via an already opened
descriptor; its diagnostic pathname is never reopened by the lock adapter.

Acquisition uses the same `<runtime>/.lock` and exclusive `flock` protocol as
Go. Timeout and cooperative cancellation govern acquisition only, not work
already running under the lock. Same-thread re-entry fails fast. Helpers own a
duplicated descriptor rather than reopening the path. Closing descriptors,
without an explicit unlock, preserves the lock until the last inherited copy
is closed. The helper launcher will explicitly map its close-on-exec duplicate
and set the existing `BPFMAN_WRITER_LOCK_FD` protocol variable.

The composition root validates input, then opens a runtime-bound `ActiveStore`
and moves it into `Bpfman`. The instance holds private dependencies and exposes
load, unload, get, and list methods; each operation uses the already-adopted
runtime. Shared behavioural tests construct the same instance in setup.
Read methods have no lock argument. Mutations and explicit cleanup retries
acquire writer authority internally, retaining receipts on admission failure. Existing stores open without the giant lock; only absence acquires the
writer and rechecks before initialization. Inside the SQLite adapter, the pure
`plan_store_open` function receives a
`StoreObservation<T>` and the supported schema version, and returns a
`StoreOpenPlan<T>`: create, use existing, or reject incompatible state. Existing
observations carry the version and opaque interpreter-owned evidence. The
interpreter supplies its opened store as that evidence; `UseExisting(store)`
returns it directly, rather than consulting a separate optional handle.
There is no representable "use existing, but no store" combination and no
runtime fallback error for it. Rejection also returns the evidence so any
resource cleanup remains in the interpreter, not pure policy. The core knows
nothing about the evidence's representation and neither clones nor drops it.
Failed observations are errors, never absence. The SQLite adapter applies the
decision within the same lock scope. This is a small explicit sequence, not a
general startup state machine or a separate effect for every mkdir and SQL
statement. The adapter's `create_if_missing` requires the runtime writer, embeds the
authoritative Go migration SQL,
and publishes a complete database without overwriting existing state. Existing
databases are not implicitly repaired or migrated. Startup releases initialization
authority before dispatch. The active store binds the backend to the adopted root
and retains the opened reader handle instead of discarding it after validation.
Cloning that handle performs no I/O and does not retain an old snapshot. SQLite
owns reuse of one idle connection and opens independent connections for overlapping
reads; no mutex is held while queries execute. JSON shares its directory handle
and acquires a fresh immutable snapshot on each read. Mutation preflight explicitly
revalidates a retained handle. The active store never recreates missing state
during an operation. Ordinary
snapshot queries remain read-only and recheck the schema within their own
transaction, with SQLite WAL permitting concurrent readers and a writer. JSON
readers consume an independently opened immutable snapshot; a zero-link inode
after concurrent rename remains valid read evidence, but never staging authority.
All read-only commands, including combined store/kernel observations, bypass
the writer lock. The store snapshot and subsequent kernel/filesystem reads are
not one atomic observation: concurrent unload may remove an object between
them. Report that absence without diagnosing inconsistency from it alone.
For XDP network namespaces, the kernel adapter opens a validated namespace fd
and performs interface lookup and outer-link creation on disposable threads.
Each thread enters once and exits; the calling thread never changes namespace.
The synchronous join keeps the writer lock in scope, while acquired link fds
return to the caller for pinning and publication. Retained unload evidence
revalidates namespace-path identity before further effects. Go's XDP path also
uses scoped OS-thread namespace switching; its subprocess helper serves a
different purpose, described below. See the
[implemented namespace contract](../../rust/README.md#explicit-xdp-network-namespaces).

### Planned mount-namespace helper for uprobes

Use a hidden subcommand of the same `bpfman` executable, provisionally
`bpfman __ns-helper open-uprobe-target ...`. This is a separate child process,
not a separately installed `bpfman-ns` binary. The subcommand selects the mode
explicitly and gives it typed argument parsing, without an inherited environment
variable changing normal CLI behaviour. Hidden means omitted from normal help;
the helper must still validate its arguments and inherited descriptors.

Dispatch this mode at the start of `main`, before ordinary signal handling,
store construction, or any other initialization that starts threads. The child
calls `setns()` directly from synchronous Rust while still single-threaded; it
does not need Go's pre-runtime C constructor. Keep helper dispatch and execution
in a private binary module, with process launching behind an injected effect
boundary so the reusable `Bpfman` API does not expose CLI mechanics.

Preserve the current Go helper's division of responsibility:

1. The parent launches the same executable with the target mount namespace and
   target lookup arguments, an inherited Unix socket, and a duplicate writer-lock
   descriptor using the existing `BPFMAN_WRITER_LOCK_FD` protocol.
2. The child enters the target mount namespace, resolves and opens the target
   binary, and sends its file descriptor to the parent through the socket using
   `SCM_RIGHTS`. It performs no BPF attachment and exits without switching back.
3. The parent attaches through `/proc/self/fd/<received-fd>` and pins the link in
   its own namespace. Attachment ownership, receipts, and compensation remain
   with the parent. Helper errors propagate as operation failures; the parent
   closes transferred descriptors and reaps the child on every outcome.

The legacy Rust helper instead attached inside the target mount namespace and
switched back to pin the link. Retaining Go's file-descriptor handoff keeps BPF
effects and their cleanup in the parent while isolating target filesystem lookup
in the child. This remains planned work, not an implemented command.

### Object creation and deletion boundary

The invariant is confinement to the supplied runtime root, not merely a blacklist
of dangerous paths. Every managed-object lookup, creation, rename, and removal
must remain within the directory authority adopted for that runtime. A prepared
runtime must hold an opened, verified root directory and resolve descendants
relative to it, with no absolute-path override, parent traversal, symlink escape,
or unintended mount crossing. Per-operation subtrees further narrow authority.
Checking path strings and then reopening by absolute path is not sufficient.
`RuntimeLayout` remains configuration only. `RuntimeDirectory` now holds an
opened root and performs directory/lock operations through Linux `openat2`,
with no symlinks or unintended descendant mount crossings. Root adoption may
cross mount points such as `/run`; unsupported kernels fail closed. The lock
must be a singly linked regular file, and replacement during acquisition is
rejected before lending authority. Tests also cover symlinked roots/ancestors,
root-path replacement, lock/database-directory replacement, and interoperation
with the Go-compatible pathname flock protocol in another process.

The filesystem capability adopts directory identity, not a permanently fixed
pathname. This is cooperative locking, not a sandbox against other processes
with authority to replace directories or lock inodes. Participants must keep
the lock inode stable during operations; the acquisition recheck does not
prevent arbitrary later external replacement.

SQLite is intentionally a separate boundary. `bpfman-store-sqlite` uses ordinary
rusqlite operations; SQLite owns database, journal, WAL, and shared-memory access.
We do not implement a custom VFS or manually route those auxiliary files through
managed-object cleanup. `RuntimeWriter::database_path` is an explicit pathname
handoff, not descriptor-relative confinement against external replacement.
Runtime directory authority and the SQLite adapter must not claim otherwise.
WAL interoperability with Go remains required and tested.

`bpfman-fs` owns the filesystem representation of runtime objects: both where
they live and how they are created or removed. Prepared load operations pin
programs and maps, publish bytecode, and remove only owned artifacts, using typed
receipts and `&RuntimeWriter`. Future link operations follow the same boundary. The core emits object-level intent; the runtime routes it to
that capability. Callers must not join path fragments or pass arbitrary paths
for deletion. A layout change then remains local to the filesystem adapter.

One private removal module is the only workspace-owned home for unlink/rmdir
primitives. Clippy permits raw unlinkat only within that module, rather than
its entire crate. No recursive deletion is implemented.
Third-party temporary-file and SQLite cleanup remains dependency-owned and is
not a way to delete managed objects. Current cleanup revalidates parent and
artifact identity and refuses symlinks and unexpected children. Like the writer
lock, this is cooperative: it does not sandbox privileged processes replacing
entries between a check and its syscall.

The removal boundary must refuse `/`, the configured runtime root, bpffs mount
root, and collection roots, including equivalent spellings. Every object target
must be a strict owned descendant with the expected identity and object kind.
Root refusal is a backend invariant as well as input validation; it must not
depend solely on callers using the constructors correctly. The lock inode must
never be unlinked as part of object cleanup.

Preserve the tests in Go's `fs/safe_test.go` and `fs/bpffs_ops_test.go`: traversal
outside the hierarchy, mount-root removal, malformed object names, and sibling
preservation. Strengthen actual string-prefix coverage with `programs` versus
`programsX`. Add filesystem-root aliases, symlinked ancestors/targets,
unexpected file types, and replacement-race cases. Lexical checks or a
canonicalise-then-delete sequence cannot by themselves prevent symlink races;
the implementation must use anchored directory descriptors and constrained
traversal, and must not cross unintended mount points. Refusal tests for host
roots use pure validation or an instrumented syscall boundary, never a real
destructive call. Actual deletion tests are confined to owned temporary trees.

Program, link, map, dispatcher, and bytecode paths must be distinct types.
Conversion back to a general path should occur only at the filesystem or kernel
edge.

## Synchronous concurrency

The domain and core crates are synchronous and runtime-independent. They contain
no futures, executor handles, mutexes, or async traits.

The new workspace stays synchronous throughout, including adapters and front
ends. Do not introduce async/await, Tokio, other async runtimes, or synchronous
wrappers around async libraries. Kernel operations and rusqlite run directly
on operation threads. Concurrent callers use ordinary threads and scoped
locking. Future server transports must respect this constraint.

SQLite access remains on rusqlite. Connection reuse and any future synchronous
pooling belong inside the backend adapter, without changing the `Bpfman` API.

The existing cross-process writer-lock and SQLite WAL model remain part of the
compatibility contract:

- mutating operations serialise where required;
- read operations proceed without the writer lock; and
- conditional load locking for shared maps remains explicit and testable.

## Error model

Errors have three layers:

1. domain errors, such as an incompatible attach type, missing map owner,
   dispatcher capacity, or unmanaged program;
2. classified effect failures on which the core may act, such as not found,
   conflict, unsupported, permission denied, interrupted, or backend failure;
   and
3. concrete adapter errors retaining the Aya, SQL, I/O, registry, or protocol
   source chain for diagnostics.

Library crates should use typed errors. The binary composition layer may use a
general report type. Error strings are context, not identity; gRPC status and
CLI exit mapping must use structured variants.

An operation failure should preserve its primary error and separately record
rollback failures in the returned report, not just in logs. A rollback failure
must not replace the error that caused rollback or skip unrelated cleanup.
Clean compensation and unresolved residue must be distinguishable. After a
successful commit, a reporting failure is not permission to compensate the
committed operation.

### Signals and cooperative cancellation

The normal CLI path, after excluding namespace-helper mode, installs `SIGINT`
and `SIGTERM` handling before preparing the request or opening the store.
The first signal requests cancellation through a caller-owned
`Cancellation`. Signal-safe handlers record/wake only; an ordinary signal thread
translates delivery into the library request. A second signal forces immediate
exit (130 for INT, 143 for TERM), explicitly abandoning graceful cleanup. The
signal implementation is confined to the binary; libraries install no handlers.

`Bpfman` remains reusable across independent callers. Each operation has an
explicit `*_with_cancellation` entry point; convenience methods use an independent
uncancelled token. Cloning a token shares one request; creating another token
keeps cancellation independent. Tokens have no reset operation. Startup and ELF
preparation also accept cancellation. Existing-store reads still bypass the
writer lock. Lock waiting checks cancellation with a maximum 25 ms backoff when
a token is supplied, and checks again after acquisition before admitting work.
This is a polling interval, not a bound on scheduling or filesystem latency.

Cancellation is observed at safe boundaries, not by interrupting arbitrary
adapter calls:

- Preparation and read operations check before and after blocking observations;
  full views also check between kernel queries. A single query or syscall may
  still take time to return.
- Missing-store initialization can cancel admission. Once initialization starts,
  its atomic publication finishes; cancellation may leave an empty valid store.
- Loading checks before each forward effect. Before commit, cancellation becomes
  the primary failure and uses the same receipt-bearing compensation interpreter
  as adapter failures. The writer lock remains held through cleanup.
- Once the commit effect starts, its actual success or failure wins over a later
  request. Successful commit ends compensation authority. Result observation
  and output still describe the committed program; late cancellation cannot
  turn success into a claim that no program was loaded.
- Unload checks during preflight and immediately before teardown. After admission
  to destructive work, finish the full pass, respecting dependencies and retaining
  both successful attempts and failures. Removing an existing pin cannot generally
  be undone.
- Compensation attempts each independent instruction once even if cancellation
  arrives during cleanup. It preserves the original failure and unresolved
  receipts. Explicit retry may cancel lock admission without losing those
  receipts, but an admitted pass runs to completion. No automatic retries follow
  a signal.

An operation that stops for a signal reports cancellation and exits 130 or 143
after unwinding and flushing telemetry. A completed mutation retains its real
success or failure status even if the token was set concurrently. Cleanup errors
and unresolved work remain visible. Retained receipts are in-process evidence,
not persisted recovery jobs; they do not survive process exit. No crash, power
loss, or forced-termination recovery guarantee is added.

Tests inject cancellation at production effect boundaries, cross cancellation
with cleanup failures, retain history across explicit retries, and exercise the
same lifecycle against SQLite and JSON. Separate subprocess tests send real
signals during startup and mutation lock contention, check trace flushing, and
prove that a second signal terminates an unresponsive operation.

## Compatibility contract

Until an intentional compatibility change is approved, the Go implementation
defines expected behaviour for:

- protobuf services and messages;
- CLI syntax, JSON shapes, and exit behaviour;
- program and link identifiers;
- SQLite schema, migrations, and stored semantics;
- bpffs, bytecode, dispatcher, and image-cache layout;
- cross-process locking;
- supported program and attach types;
- dispatcher ordering, proceed-on encoding, and embedded C ABI;
- XDP, TC, and TCX attachment semantics;
- map ownership and shared pin cleanup;
- OCI pull policies, authentication, signature policy, and cache admission;
- namespace behaviour;
- error classification exposed to callers; and
- inspection and residue reporting.

The Rust implementation may improve internal diagnostics and type safety while
remaining compatible at these boundaries.

The Go and new Rust processes must never mutate one runtime concurrently. Tests
may run them sequentially against copied state or isolated runtime roots.

### `bpfman-shell` as the parity harness

The existing test DSL is a major part of the migration strategy, not merely a
test suite to run at the end. The corpus contains 132 shared `.bpfman`
end-to-end scripts plus eight Rust-only DEVMAP scripts covering program and link lifecycles, dispatcher rebuilds,
ordering and proceed-on behaviour, map sharing, namespaces, OCI images, CLI
errors, persistence, traffic, and cleanup.

`bpfman-shell` is especially valuable because its `bpfman` builtin executes a
CLI subprocess. It does not call the manager to perform the operation. The
subprocess is selected by `BPFMAN_BIN`, defaulting to `bpfman` on `PATH`, and
the builtin automatically requests JSON where the command supports it. The Go
shell then decodes that JSON into the current Go result types and exposes typed
values to the DSL. Existing scripts can consequently run unchanged against a
Rust binary while retaining the mature Go runner, fixtures, assertions,
process cancellation, and cleanup machinery.

This gives the parity suite several independent checks:

- the Rust CLI must accept the same arguments and return compatible exit
  statuses and stderr diagnostics;
- structured stdout must decode into the existing Go result vocabulary;
- DSL assertions check the fields and relationships on which callers rely;
- setup and stimulus builtins create namespaces, interfaces, traffic, and
  process targets independently of the implementation under test; and
- traffic and kernel observations demonstrate real behaviour rather than only
  self-consistent stored state.

Successful Go decoding proves structural compatibility for decoded and
asserted fields, but it is not a byte-for-byte JSON check: Go decoding permits
unknown and omitted fields. Focused golden tests remain necessary where exact
field presence, ordering, text output, stderr, or exit codes are public
contracts.

There are two CLI execution paths in the corpus and both must select Rust:

1. the typed `bpfman` builtin uses the absolute path in `BPFMAN_BIN`; and
2. scripts that deliberately use `exec bpfman`, including commands nested in a
   namespace shell, resolve `bpfman` through `PATH`.

The parity runner must therefore set `BPFMAN_BIN` to the absolute Rust binary
and put that binary's directory before the directory containing the Go-built
`bpfman-shell` on `PATH`. Setting only `BPFMAN_BIN` silently leaves raw
`exec bpfman` calls testing the Go binary. The `run-e2e-scripts` Make target now
accepts `BPFMAN_UNDER_TEST`, defaulting to `$(BIN_DIR)/bpfman`, and sets both
paths consistently inside `sudo`. The selected executable must be named
`bpfman`. Unprivileged process-fixture tests exercise the selection paths;
behavioural parity still requires the real CLI and kernel-backed corpus.

The Go shell and script runner remain test infrastructure throughout the
rewrite. They are intentionally outside the new Rust workspace and are allowed
to build Go code; the rule against building existing Rust code remains
unchanged. Shell language tests under `cmd/bpfman-shell/testdata/` validate the
runner itself, while the scripts under `e2e/scripts/` validate bpfman behaviour.
Only the latter form the implementation-parity corpus.

Parity runs obey these rules:

- run the Go and Rust implementations sequentially, with the suite-wide lock
  held and residue removed between them;
- begin with the same bytecode mode, configuration, fixtures, kernel, and
  script selection;
- use the existing script labels and exact-test selection to grow an explicit
  per-feature parity manifest;
- treat a script as implemented only when the unchanged script passes against
  Rust; do not maintain a weakened Rust-specific copy;
- retain serial and exclusive scheduling annotations and exercise the parallel
  and stress lanes once a feature reaches basic parity; and
- run both file-bytecode and image-bytecode modes before declaring the related
  capability complete.

The primary differential result is semantic: the same script passes against
each implementation. Normalized Go/Rust output comparison is an additional
tool for stable read-only commands, not a prerequisite for every script because
kernel IDs, pin paths, timing, and diagnostics may contain run-specific data.

## Testing strategy

### Pure core tests

The core should carry most behavioural coverage:

- table-driven transition tests;
- exhaustive small-state exploration where practical;
- property tests for ordering, bounded slots, uniqueness, and idempotence;
- failure injection at every emitted effect;
- rollback-order tests;
- event-trace replay; and
- crash-cut models describing the observable state after interruption between
  any two effects.

These tests require no root privileges, bpffs, SQLite, network, or async
runtime.

### Adapter contract tests

Each effect adapter should have a contract suite covering both success and
partial failure. SQLite tests use temporary databases and the real schema.
Kernel tests use the smallest available test boundary first and reserve a real
kernel for behaviour that cannot be simulated meaningfully.

Generic outside-in lifecycle scenarios inject failures through store operations,
not through SQL or serialized-file edits. The privileged Rust integration tests
use the public runtime API, real kernel/filesystem effects, and a `Faults<S>`
store decorator. CLI and DSL acceptance inspect public output and artifacts.
Backend-specific adapter tests retain SQL triggers and direct state inspection
where they test SQLite's own guarantees. A second store backend must run the
same generic scenarios in addition to its own persistence-format tests. SQLite
and JSON now run the same lifecycle, CLI, and unchanged tracepoint and admitted
XDP DSL scenarios, with backend selection confined to setup. The runtime and pure
crates did not need backend-specific branches.

The JSON adapter publishes versioned whole-file snapshots through descriptor-
relative filesystem operations under the runtime writer. Program and private
map-set membership commit together. Deletion receipts bind the runtime, store
identity, and record generation; failed deletion retains the receipt. Publication
errors precede the atomic rename and therefore authorize compensation safely.
Interrupted staging can be reused without changing published state. Runtime state
lives under `/run`; power-loss durability is outside this contract. XDP deletion
receipts additionally bind the complete dispatcher/member snapshot, so a retry
cannot delete a replacement snapshot.

The CLI selects `sqlite` (default) or `json` with `--store` / `BPFMAN_STORE`.
Both occupy the same runtime store slot, so a format mismatch is rejected rather
than opening an independent inventory. Existing state is never converted implicitly.

Preserve the value of Go's stateful fake kernel (`manager/fake_kernel_test.go`).
An in-memory effect interpreter should track IDs, programs, links, pins, and
dispatcher revisions, enforce resource invariants, and support deterministic
failure injection. This complements pure transition tests with full runtime
tests for rollback ordering, resource lifetime, and residue. Real-kernel tests
still establish verifier, syscall, namespace, and traffic guarantees.

SQLite uses explicit SQL through `rusqlite`, following the successful Go store
design. Tests embed the existing Go migration SQL with `include_str!`; there is
one schema authority. An ORM and a second schema representation are unnecessary
for this migration.

### Compatibility and differential tests

Prioritize binary behavior against the unchanged `.bpfman` corpus before expanding
Rust-specific packet scenarios or tuning execution speed. There are 167 scripts:
50 shared scripts and 30 Rust-only scripts carry `rust=ok`, admitting
80 scripts against Rust on each store. The remaining 87 are unselected, not
established failures. Add `rust=ok` only after the script passes against Rust with
both SQLite and JSON in file-bytecode mode. Preserve existing assertions and
scheduling labels.

Rust-only scripts additionally carry `#pragma labels={"rust-only":"true"}`.
`BPFMAN_E2E_IMPLEMENTATION=rust` admits them; the default `go` skips them regardless
of selector. The explicit implementation setting is independent of binary path
selection via `BPFMAN_UNDER_TEST`. Shared parity is selected with
`rust=ok,!rust-only,!external`; all admitted Rust acceptance uses
`rust=ok,!external`. Rust-only scripts cover already implemented Rust behaviour;
they do not establish Go parity or, by their label alone, show that Go cannot
support the same scenario.

The normal gate now runs one parallel Go-runner batch per backend, replacing the
individual script launches in serial Rust wrappers. Backend batches remain
sequential because separate runners hold the shared suite lock. Each batch
checks a nonempty manifest, a PASS for every selected script, empty final
inventories and no managed artifacts. The existing interface pool, scheduler,
serial/exclusive pragmas, cleanup and failure reporting remain authoritative.

### Testing migration audit (8 October 2026)

Externally observable behaviour belongs in `.bpfman` scripts. Existing corpus
coverage comes first; new scripts fill gaps in implemented functionality. Reuse
existing syntax and helpers before adding specific runner support. Unit tests
remain valuable for small internal decisions and must be quick. Integration
contracts that perform I/O or need internal failure injection remain distinct
from unit tests, even when they run without root.

The test-target inventory across the new workspace has been reviewed by family:

| Existing tests | Destination and current status |
| --- | --- |
| Model validation, XDP/TC ABI, core transitions, CLI parsing and output formatting | Keep fast Rust unit tests. `rust-test-unit` runs library/binary unit targets; 92 tests reported 0.60 s aggregate execution, excluding build/startup. |
| Workspace laws and compile-fail ownership/confinement examples | Keep Rust architecture/type contracts; separate from the fast unit target. |
| Runtime load, compensation, store, link, XDP and unload unit modules; `kernel_lifecycle` and `tests/lifecycle/*` | Keep internal failure-injection and ownership contracts. Real store/file effects make these integration contracts where applicable; do not duplicate their ordinary external outcomes as packet matrices. |
| Filesystem layout/directory/artifacts/unload/snapshot and flock tests | Keep adapter contracts for descriptor confinement, replacement races and process locks. Filesystem/process timing is not unit-test execution. |
| SQLite/JSON adapter tests and shared store/link/XDP/TC/concurrent-store contracts | Keep atomicity, version, receipt and persistence-format integration contracts. Shared CLI behaviour belongs in scripts. |
| Existing tracepoint/XDP/TC DSL wrappers and XDP/TC corpus modules | Migrated to two backend-wide parallel script batches; obsolete wrappers removed. All 42 existing script bodies are unchanged. |
| DEVMAP/DEVMAP_HASH unicast packet scenarios | Forwarding, fallback, continuation, live updates, both survivors, payload/fragment evidence and map identity moved to eight parallel Rust-only scripts. Rust retains publication-failure restoration and adapter-lifetime checks. Both stores passed. |
| Process-signal policy formerly under the CLI unit module | Moved to `tests/signal_policy.rs`; still runs in the normal userspace integration gate. Deterministic in-flight/second-signal mechanics need internal access. |
| `cli`, `load`, `unload` integration targets | Reviewed assertion groups against the corpus below. Retain process parsing, no-effects preflight, unsupported-surface, persistence and captured-input contracts. Public load/global-data/unload outcomes run in scripts. |
| `signals`, `telemetry`, `cancellation`, `kernel_observations`, `sqlite_compatibility`, `e2e_selection` integration targets | Retain internal/process, cross-implementation persistence and wiring contracts separately; remaining public-scenario extraction is pending. |
| Real-kernel CLI helper | Ordinary file capture, record/get/list/quiet-list and unload assertions moved to shared scripts. Retain provenance ownership, input-before-effects, foreign pin/map refusal and output failure after commit. |
| Real-kernel TC CLI and ordinary XDP/TC replacement paths | Four parallel lifecycle scripts cover XDP modes/fallback, stopping/continuation and either survivor, plus TC signed continuation, stop, detach, attached unload and CLI output. Two duplicate TC CLI kernel wrappers are removed. Kernel publication, retirement, ownership, cancellation, foreign-filter coexistence and retry contracts remain. |
| Real-kernel batch CLI and remaining normal lifecycle paths mixed with faults | Reuse the admitted corpus first, extract any missing public assertions, then remove duplicates. Keep injected failures, cancellation and retained receipts. Remaining extraction is pending. |
| TX/direct REDIRECT, ordinary and jumbo frames | Eight parallel scripts cover modes, continuation, both survivors and full payload/fragment evidence. Rust retains two post-failure captures per scenario, snapshot restoration, ownership and cleanup checks. Both stores passed. |
| Ordinary/jumbo DEVMAP/DEVMAP_HASH broadcast | Eight parallel scripts cover driver/SKB forwarding, ingress exclusion, empty/sparse maps, live updates, continuation and both survivors. Jumbo cases retain full payload/fragment counters and exact filtered `EOPNOTSUPP` rejection of cloning. Rust retains post-publication-failure traffic, exact map contents, ownership and lifetime checks. Both stores passed. |
| Native DEVMAP/DEVMAP_HASH egress | Eight parallel scripts cover ordinary PASS/DROP, hash jumbo PASS/DROP, array jumbo rejection, exact context/fragment counters, live updates, replacement/survival and forwarding after managed unload. Rust keeps publication/teardown failures, restoration and map-held lifetime checks. Both stores passed. |
| Jumbo PASS and mixed-fragment membership | Two parallel driver/SKB scripts cover single/two-member execution, stopping, normal-MTU mixed membership, restored jumbo operation, both survivors and native jumbo rejection. Rust retains post-publication-failure traffic and internal dispatcher ABI flags. Both stores passed. |
| XDP attach/switch/unload/netns and TC unload modules | Remaining packet/lifecycle migration candidates. Separate ordinary externally observable scenarios from real-kernel recovery evidence. Do not remove coverage or label migration complete until equivalent scripts pass on both stores. |

These are incremental migration slices, not a claim that all Rust behavioural
tests have moved. Next: migrate remaining ordinary lifecycle/CLI scenarios using
the same runner. No new
bpfman functionality is needed for that migration.

A representative pre-migration DEVMAP test took 40.83 s and performed 88 captures.
Its 300 ms observation windows alone accounted for 26.4 s. The shared helper sends
three marked frames per wave, checks source/local/redirected delivery, detects
duplicates and payload damage, and checks fragment reads through BPF counters.
The new scripts share the setup for both survivor choices; Rust retains just the
two post-failure packet checks per scenario. Observation windows have not been
shortened. The eight scripts passed together in about 40 s per backend.

#### CLI load/unload and TX/direct REDIRECT follow-up

At the CLI/delivery checkpoint the corpus had 149 scripts, with 62 admitted
against Rust: 48 shared Go/Rust scripts and 14 Rust-only scripts. The three
newly admitted existing scripts passed unchanged on both SQLite and JSON; only `rust=ok` header labels
were added:

- `TestLoadWithMetadataAndGlobalData`: metadata/global-data load, get and list.
- `TestLoadGlobalData_UnknownKeyRejected`: real globals round-trip; unknown names fail.
- `TestLinkMetadata_DispatcherRebuildPreservation`: independent link metadata
  survives rebuilding an XDP dispatcher.

The new shared `TestProgram_FileLifecycle` fills the remaining ordinary CLI gap:
exact captured ELF bytes in file mode, complete record equality after get/list,
quiet-list membership, unload and disappearance of the program pin/bytecode.
It passed against Go (SQLite), and Rust with both stores. The existing exhaustive
`TestTracepoint_LoadAndGet` and `TestXDP_LoadAndGet` cover load/get kernel status,
map observation shapes, license and source/name fields. Duplicate assertions
were removed from Rust's real-kernel CLI helper only after these scripts passed.

The nonprivileged CLI assertions were reviewed as follows. A shared error outcome
does not replace a check that invalid input caused no source/runtime effects.

| Rust assertion group | Script coverage or reason to retain |
| --- | --- |
| `cli`: invalid flags, timeout, runtime roots, IDs, link requests and XDP config | Retain process-boundary checks, including stdout/exit classification and runtime noncreation. They do not require packet tests. |
| `cli`: help, version, absent/unimplemented commands | Retain executable identity, experimental scope and unsupported-command contracts. |
| `cli`: selected store reopens/refuses another format | Retain backend selection and persistence-format refusal; shared scripts select the backend rather than reinterpret another backend's files. |
| `load`: malformed programs/options and unsupported program families/tracepoint options | Retain rejection before any source access or runtime setup. The mismatch corpus also needs unsupported probe/TCX families. |
| `load`: registry credentials and native non-UTF-8 paths | Retain redaction and native-path process contracts; no network/kernel load is needed. |
| `load`: unknown/wrong-size globals, malformed/missing local ELF | Unknown-key outcome is shared with `TestLoadGlobalData_UnknownKeyRejected`; retain no-runtime-effects checks and wrong-size cases. |
| `load`: batch captured ELF and duplicate/missing selections | Retain `PreparedProgram` ownership: replacing the source after preparation must not change the captured batch. This is an API/input-lifetime contract. |
| `unload`: malformed/unsupported operands | Retain parsing before runtime setup, including explicit rejection of unsupported `--ignore-missing`. |
| `unload`: missing ID initializes an empty store without bpffs objects | Retain nonprivileged startup/effect-boundary evidence. Successful ordinary unload is covered by the file-lifecycle and existing load/get scripts. |
| `unload`: unsupported persisted state unchanged | Retain the backend-specific fixture and byte-for-byte preservation check. |
| Real-kernel CLI: provenance, foreign live program/map pins, `/dev/full` after load/attach | Retain private ownership receipts and post-commit output-failure contracts; ordinary success is now scripted. |

Three additional existing scripts were executed unchanged against both Rust
stores and remain **untagged**, with the same incompatibilities on each store:

| Script | Observed gap |
| --- | --- |
| `TestUnload_IgnoreMissingIsIdempotent` | Rust says `managed program … not found` instead of `does not exist`, and rejects `--ignore-missing`. |
| `TestLoad_RejectsSectionTypeMismatch` | Probe/TCX families are unsupported; the supported XDP mismatch says `selected ELF program is not XDP` rather than the expected `program type mismatch`. |
| `TestLoad_SurfacesVerifierLog` | The fixture selects a kprobe, rejected as unsupported before verifier execution. This does not establish a verifier-log failure for supported Rust program types. |

No flags, diagnostics or program families were implemented merely to admit those
scripts, and their assertions were not changed.

Eight new `TestXDP_TX_*` / `TestXDP_Redirect_*` scripts reuse
`e2e/xdp-delivery.bpfman`: driver/SKB modes times 64/8014-byte frames times TX/direct
REDIRECT. Each checks terminal versus continued action, a DROP tail, both survivor
choices, stable outer link identity, and ordinary local delivery after last detach.
Jumbo counters independently prove multi-buffer input plus tail and cross-buffer
reads for every executing member and receiving observer. Captures retain exact
delivery counts, duplicate detection, full payload checks and the 300 ms window.
The eight ran concurrently alongside the file-lifecycle script in about 18 s per
Rust backend. Final program/link/dispatcher inventories were empty.

Unlike the earlier DEVMAP admission labels, these new delivery scripts were also
compared with Go before choosing their labels. Ordinary driver TX and REDIRECT
passed and are shared. The six SKB/jumbo cases are Rust-only: Go attached in driver
mode despite the SKB configuration, and all jumbo cases failed to attach the
fragment observer on the MTU-9000 receiving peer (`numerical result out of range`).
This demonstrates Rust acceptance beyond the tested Go paths, without claiming
Go cannot support other SKB or jumbo topologies. A final Go run passed the three
shared new scripts and skipped all six Rust-only scripts.

Rust's direct-delivery matrix still injects failed attach and non-last detach
publication for both actions, continuation settings and survivor choices on each
mode/store/frame size. Only the two post-failure packet waves remain there:
48 to 16 captures per matrix invocation. No fault, restoration, ownership or
cleanup assertion was removed. The full workspace/kernel gate is the final
checkpoint validation; its result is recorded with the current checkpoint above.

#### Ordinary DEVMAP broadcast follow-up

At this checkpoint the corpus had 153 scripts, with 66 admitted against Rust:
50 shared Go/Rust scripts and 16 Rust-only scripts. Four new `TestXDP_{Devmap,DevmapHash}_Broadcast_`
wrappers cover ordinary driver/SKB broadcast through the existing Go runner.
Each script owns a pooled private namespace with three veth pairs; scripts run
in parallel without serial/exclusive labels. The existing delivery probe already
supports the fourth capture interface, so no runner or probe changes were needed.

Both ingress-exclusion settings cover empty maps, ignored lookup key 99 and PASS
fallback, ingress-only targets, complete fan-out, deletion/repopulation and live
retargeting, stable dispatcher revision during map updates, ordered replacement,
both survivor choices, last detach and eventual pin cleanup. Explicit REDIRECT
continuation suppresses copies and reaches DROP; exhausting the chain returns
PASS. Packet waves check exact counts at sender, local ingress and both sinks,
complete payloads, duplicate detection and executing-member counters. Map ID and
complete entry snapshots remain stable across replacement and both survivors.
The array uses keys 0/1/2; hash keys 7/0x80000001/0xffffffff exceed its capacity
value. Both survivors share the forwarding preamble rather than repeating it.
Initial four-script batches passed on both Rust backends, with empty final
program/link/dispatcher inventories. The final helper also retains zero redirect
errors per wave through the existing tracepoint fixture, filtered by map ID and
ingress ifindex; it checks the running tracefs field layout before attaching.
These filters isolate error evidence from other parallel scripts.

Go comparison passed both ordinary driver scripts on SQLite and JSON. Its two
requested SKB cases
completed the packet assertions but attached in driver mode (1 rather than 2),
despite the supplied per-interface SKB configuration. Those two wrappers are
Rust-only; the driver wrappers are shared. This records the tested mode-selection
gap without claiming Go cannot support another SKB topology. The final Go run
with zero-error observation passed both shared scripts and skipped both Rust-only
scripts on each store, with empty program/link/dispatcher inventories. No
production functionality, persistence, dependency or Aya change was needed.

Only after script acceptance on both stores were ordinary success captures
removed from Rust's broadcast matrix: 72 to 8 packet waves per ordinary matrix
invocation (two after injected publication failures in each of four scenarios).
Exact map keys/values, snapshot restoration, stable outer identity, either
survivor, map-held lifetime, final reclamation and empty inventories remain.
The separate successful continuation lifecycle now runs in scripts for ordinary
frames. Jumbo broadcast remains unchanged, including fragment/tail/boundary
reads, single-target forwarding, filtered redirect-error observation and exact
`EOPNOTSUPP` rejection of cloning on Linux 6.18.54. The next follow-up migrates
jumbo broadcast; egress remains pending.

#### Jumbo DEVMAP broadcast follow-up

At the jumbo broadcast checkpoint the corpus had 157 scripts, with 70 admitted
against Rust: 50 shared Go/Rust and 20 Rust-only. Four new
`TestXDP_{Devmap,DevmapHash}_Broadcast_{Drv,Skb}_MultiBuffer`
wrappers extend the same helper to 8014-byte frames and MTU 9000. No runner,
probe, production, persistence, dependency or Aya changes were needed. Each script
uses a pooled private namespace; both survivor choices share their setup and all
scripts remain parallel without serial/exclusive labels.

The jumbo wrappers repeat the ordinary lifecycle: both ingress-exclusion flags,
empty and ingress-only maps, ignored lookup key 99 and PASS fallback, full maps,
delete/retarget/repopulate, unchanged revision during updates, ordered replacement,
both survivors, last detach and explicit REDIRECT continuation. Single eligible
targets receive complete payloads without duplicates. Every executing member and
receiving observer independently proves genuine fragments, intact tail bytes and
reads across the linear/fragment boundary. Local PASS and DROP-tail stopping remain
explicit assertions rather than inferred from missing sink traffic.

Linux 6.18.54 rejects multi-buffer cloning when more than one destination is
eligible. Those waves require zero copies at all four capture points and exactly
three filtered redirect errors, all `EOPNOTSUPP`; every other wave requires zero
errors. The existing tracepoint fixture filters by the original map ID and ingress
ifindex, with its field layout validated against tracefs, so other parallel scripts
cannot satisfy or contaminate the evidence. Continuation cancels REDIRECT before
execution and therefore requires no redirect errors, even with a full map.

The initial eight-script ordinary/jumbo batches passed against Rust JSON and
SQLite in 35.96 s and 38.63 s respectively, with empty program/link/dispatcher
inventories. Go comparison on both stores passed the two ordinary driver scripts
and failed all four jumbo scripts while attaching the receiving fragment-aware
observer at MTU 9000: `numerical result out of range` (`ERANGE`). Jumbo wrappers
therefore carry `rust-only=true`. This establishes the observed receiving-topology
boundary; it does not establish whether Go could support jumbo broadcast with a
different setup. The already Rust-only ordinary SKB wrappers were skipped in this
comparison. No assertion was relaxed to admit Go. With final admission labels,
both Go backend runs passed the two shared ordinary driver scripts and skipped
the six Rust-only broadcast cases, with empty final inventories (20.86 s SQLite,
21.56 s JSON).

After both Rust stores passed, duplicate jumbo success captures and the separate
successful continuation lifecycle were removed from Rust. Each jumbo matrix now
uses eight packet waves instead of 72: two after injected attach/detach publication
failures in each of four exclusion/survivor combinations. Across eight jumbo matrix
invocations, that removes 512 serial captures (153.6 s of observation windows).
The retained waves still require fragment evidence and exact cloning errors after
restoration. Exact snapshots, restoration attempts, map keys/values and ID, ordered
membership, stable outer identity, both survivors, map-held ownership, eventual
reclamation and empty final inventories/artifacts remain Rust integration checks.

The jumbo DEVMAP broadcast migration passed the complete
`direnv exec . make rust-check` gate: formatting, Clippy, userspace and compile-fail
contracts, Rustdoc and all 98 real-kernel tests, with none failed or ignored.
Both backend batches verified all 70 admitted scripts and empty final inventories
and artifact collections. The kernel stage took 1124.80 s (18m45s), compared with
1180.41 s (19m40s) at the ordinary broadcast checkpoint. These are local run
measurements, not a controlled benchmark. Makefile lint and canonical DSL
formatting also passed.

### Native egress testing follow-up

At the egress checkpoint the corpus had 165 scripts, with 78 admitted against Rust: 50 shared Go/Rust
and 28 Rust-only. Eight new
`TestXDP_{Devmap,DevmapHash}_Egress_{Drv,Skb}_{Linear,MultiBuffer}`
wrappers use the existing pooled namespace, scheduler, DSL and `devmap-egress`
packet-probe command. No runner, probe, production, persistence, dependency or
Aya changes were needed. Backend batches remain sequential; these scripts run
in parallel without serial/exclusive pragmas.

Six positive scenarios cover ordinary native egress on both map types and genuine
8014-byte DEVMAP_HASH egress. They require native XDP program IDs, no managed
interface links, rejection of interface attachment after CLI/store reopening,
empty-map fallback, PASS delivery, DROP consumption, live PASS/DROP replacement,
and unchanged dispatcher revision during map updates. Invalid extension and
fragment-incompatible updates must return `EINVAL` and leave the complete map
unchanged. Hash maps retain their unrelated `0xffffffff` entry at full capacity;
lookups return the exact output ifindex and program ID rather than the update FD.
Ordered dispatcher replacement and surviving redirect retain outer-link identity
and all map entries. Last detach restores local PASS.

Every wave validates complete payloads without duplicates and exact ingress,
tail and egress execution counts. Egress counters also prove input/output
interface context. Jumbo receiving/executing counters independently require genuine fragments,
intact tail bytes and successful reads across the linear/fragment boundary.
Managed PASS/DROP unload removes program pins and records while map-held egress
continues executing. Temporary pins retain only counter maps for observation;
they do not retain program FDs and are removed before map-owner cleanup.

Two array-map jumbo scenarios preserve the current Aya extension-flag boundary:
fragment-aware egress insertion returns `EINVAL` in empty and populated maps,
with unchanged entries/revision and zero egress execution. Complete jumbo unicast
without egress remains successful. These are rejection tests, not evidence of
array jumbo egress execution.

The initial eight-script batch passed on Rust JSON and SQLite in 11.95 s and
11.85 s, with empty final program/link/dispatcher inventories. Actual Go runs on
both stores failed native egress loading: the verifier rejects the egress-ifindex
context read (`invalid bpf_context access off=20 size=4`). This agrees with Go's
managed XDP loader forcing extension type and clearing expected attach type.
All eight wrappers therefore carry `rust-only=true`; no assertion was weakened
for Go. This is an observed native-egress capability difference, distinct from
Go's broader support elsewhere. Final admission runs passed all eight scripts
on Rust JSON/SQLite (12.53 s / 12.35 s), with empty inventories. Go correctly
skipped all eight Rust-only scripts on each store, with empty inventories.

After both Rust stores passed, positive Rust suites were reduced from fourteen
packet waves to five: two after attach/detach publication restoration and three
around failed record deletion, explicit unload retry and map-held DROP lifetime.
Array-jumbo boundary suites retain native role/map compatibility, exact entries,
unchanged dispatcher identity and eventual reclamation, while their three
ordinary unicast waves and unused receiving/tail setup moved to scripts.
Across twelve positive and four boundary invocations, 120 serial captures were
removed (36 s of observation windows). Exact snapshots, restoration attempts,
unresolved ownership, unload retries, retained map descriptors, bounded kernel
program/map reclamation and empty inventories/artifacts remain Rust contracts.

The egress testing migration passed the complete `direnv exec . make rust-check`
gate: formatting, Clippy, userspace/compile-fail contracts, Rustdoc and all 98
real-kernel tests, with none failed or ignored. Both backend batches verified
all 78 admitted scripts and empty final inventories/artifact collections. All
sixteen retained egress contracts passed. The kernel stage took 986.98 s
(16m27s), compared with 1124.80 s (18m45s) at the jumbo broadcast checkpoint.
These are local run measurements, not a controlled benchmark. Makefile lint and
canonical DSL formatting also passed.

### Jumbo PASS and mixed-fragment testing follow-up

At the jumbo PASS checkpoint the corpus had 167 scripts, with 80 admitted against Rust: 50 shared Go/Rust
and 30 Rust-only. `TestXDP_PASS_{Drv,Skb}_MultiBuffer` uses the existing pooled
namespaces, parallel scheduler, delivery probe and fragment-aware PASS fixture.
The shared `xdp-frags.bpfman` helper adds assertions, without runner, probe,
fixture, production, persistence, dependency or Aya changes.

Both scripts verify the actual requested attachment mode, single-member PASS,
equal-priority incoming-member ordering, stopping when PASS is excluded from
proceed-on, and two-member execution. Every jumbo wave requires three intact
8014-byte frames locally, no duplicates or unexpected TX/redirected delivery,
and exact execution, non-linear-buffer, tail-byte and boundary-read counters
for each active member. Inactive members must remain unchanged. At MTU 1500,
an ordinary counter program joins ahead of the two fragment-aware members;
all three execute on ordinary frames, with no fragment-read increments.
Removing the ordinary member and restoring MTU 9000 proves jumbo operation
again. Both survivor choices preserve outer-link identity, and last detach
restores local delivery with no managed-program execution.

Driver mode also rejects adding the ordinary member at jumbo MTU. The complete
dispatcher snapshot remains unchanged, the rejected program has no links, and
the existing fragment-aware chain still delivers intact jumbo frames. This is
a native-veth restriction, not a requirement for SKB mode.

Before admission labels, the two-script batches passed on Rust JSON and SQLite
in 5.76 s and 5.53 s, with empty final inventories. Actual Go comparison failed
the first jumbo attachment in both modes and both stores with `ERANGE`
(`numerical result out of range`). Both scripts therefore carry
`rust-only=true`; the comparison does not establish which later transitions
Go could perform if the initial attachment succeeded. Final labelled batches
passed on Rust JSON/SQLite in 5.34 s / 5.42 s, while Go correctly skipped both
scripts on each store. All final inventories were empty.

Rust retains two jumbo traffic waves per survivor scenario: after failed attach
publication restores the single member and after failed detach publication
restores both members. Exact snapshots, successful restoration attempts,
unresolved-ownership counts, actual attachment mode, native jumbo rejection,
mixed-dispatcher ABI flags, stable outer identity and residue-free teardown
remain integration contracts. Across both stores/modes and survivor choices,
44 duplicate jumbo ping waves and eight ordinary ping waves were removed.
These used ICMP rather than the delivery probe's 300 ms capture windows; no
fixed-window time reduction is claimed for this slice.

The complete `direnv exec . make rust-check` gate passed: formatting, Clippy,
userspace/compile-fail contracts, Rustdoc and all 98 kernel tests, none failed or
ignored. Each backend batch checked all 80 admitted scripts and empty final
inventories/artifacts. All four retained PASS restoration/ABI contracts passed.
The kernel stage took 978.34 s (16m18s), compared with 986.98 s (16m27s) at the
egress checkpoint. These are local run measurements, not a controlled benchmark.
Makefile lint and canonical DSL formatting passed too.

### Ordinary lifecycle and timing follow-up

Per-test timing is opt-in with `BPFMAN_KERNEL_TIMINGS=1` through the normal
`direnv exec . make rust-test` or `make rust-check` gate. Each complete test body,
including setup and teardown, reports monotonic elapsed seconds. The timer writes
TSV to stderr independently of libtest capture and reports unwinding failures.
This separates each backend-wide parallel script batch from the retained serial
contracts without adding a dependency or changing scheduling. The corpus also
emits the Go runner's per-script result/duration rows, prefixed with the backend;
parallel script durations overlap and must not be added as batch wall time.

Three `TestXDP_Lifecycle_{Drv,Skb,Hw}` scripts cover actual driver/SKB mode and
hardware-request fallback on private veths, same-priority incoming ordering,
PASS stopping/continuation, both survivor choices, stable outer-link identity,
and last-detach delivery with inactive counters unchanged. Default continuation
already has unchanged shared-corpus coverage. The stopping script excludes PASS;
the previous runtime test used a literal zero mask, whose representation remains
covered by the internal configuration/validation tests.

`TestTC_IngressLifecycle` covers signed UNSPEC continuation into SHOT, higher
priority OK stopping, attached unload restoring two members, non-last detach,
explicit SHOT continuation, link metadata/status/proceed-on output and last
attached-program unload. Exact counters and intact local/drop packet delivery
replace duplicate TC CLI acceptance. The admitted clsact-reclamation script
covers Rust's reclaim policy separately. Kernel TC tests retain publication
rollback, cancellation, malformed/foreign identity refusal, reopened owned or
borrowed qdiscs, independent retirement and cleanup retries. Traffic alongside
foreign filters remains a distinct coexistence contract.

The delivery probe gains `stimulus SENDER RECEIVER`: it sends three marked
64-byte Ethernet frames on a private veth and returns after observing all three
intact frames at the receiver, bounded by one second on failure. The ordinary
Rust XDP traffic helper uses this positive delivery evidence instead of configuring
addresses, flushing neighbours and waiting for unanswered ping. Its active
members must count at least three frames; inactive counters remain unchanged.
All existing forwarding captures retain their full 300 ms silence window.

The DEVMAP restoration matrix no longer repeats DROP/PASS missing-key fallback.
Every retained restoration wave has a populated target, so that choice cannot
change the observed path. Four scenarios still cross both REDIRECT continuation
choices with both removed members for every backend, map kind, mode and frame
shape. Scripts keep both fallback actions on empty maps. This removes 64 complete
setup/teardown scenarios and 128 captures across the sixteen matrix invocations.
TX/direct REDIRECT action choices, continuation choices, broadcast ingress
exclusion, both removal choices and native egress lifetime remain distinct
kernel contracts and are retained.

The corpus now has 171 scripts, with 84 admitted against Rust: 52 shared Go/Rust
and 32 Rust-only; the 87 remaining scripts are unselected, not established
failures. Initial actual four-script runs passed all four on Rust JSON/SQLite
in 5.84 s / 5.81 s, with empty inventories. Go passed TC and driver XDP on both
stores. Requested SKB and hardware-request fallback cases failed actual-mode
assertions because Go attached in driver mode (1 rather than 2). Those two
wrappers therefore carry `rust-only=true`; the driver and TC wrappers are shared.
No Go assertion was relaxed.

The timed baseline passed all 98 kernel tests in 1138.08 s (18m58s). The prior
checkpoint's 978.34 s (16m18s) was a separate local run; this variation is why
before/after samples need the same measurement method. The two baseline script
batches took 169.316 s (JSON) and 217.026 s (SQLite), 386.342 s combined.
The final changed gate passed all 96 kernel tests in 782.36 s (13m02s), with no
failures or ignored tests. Formatting, Clippy, userspace/compile-fail contracts,
Rustdoc and Makefile lint passed. Both batches required all 84 selected scripts
to pass and final inventories/artifacts to be empty. All 96 per-test timing
records completed. Canonical formatting of the new DSL sources passed too.

| Test group | Baseline seconds | Changed seconds |
| --- | ---: | ---: |
| Parallel script batches | 386.342 | 309.472 |
| Standalone DEVMAP unicast (ordinary array, ordinary/jumbo hash) | 184.834 | 84.238 |
| Broadcast restoration/lifetime | 136.538 | 130.351 |
| Jumbo TX/REDIRECT + array DEVMAP | 131.238 | 84.858 |
| Ordinary XDP runtime restoration | 73.469 | 9.802 |
| Ordinary TX/REDIRECT | 62.770 | 55.327 |
| Other kernel contracts | 46.823 | 28.396 |
| TC contracts | 46.061 | 36.207 |
| Native egress | 42.304 | 39.768 |
| Ordinary XDP switch/restoration | 27.555 | 3.791 |

Serial contract time sums to 751.592 s before and 472.738 s after; whole-test
records sum to 1137.934 s and 782.210 s, with the small remainder belonging to
harness overhead. Timers include setup, traffic and teardown; they do not isolate
backend CPU or sync costs. The two deleted TC CLI wrappers moved to script
acceptance; 36 ordinary XDP traffic waves and 24 TC captures were removed. The
shared stimulus also accelerates retained XDP switch and unload contracts.
The DEVMAP matrix removes a further 128 captures, as described above.
These local before/after runs are not a controlled benchmark. Improvements in
unchanged groups and script batches must not be attributed wholly to the edits.

SQLite and JSON did not show a uniform performance ordering. The full SQLite
script batch was slower, while representative retained SQLite forwarding tests
were faster (28.440 s versus JSON's 37.301 s for driver jumbo forwarding).
The SQLite reader caches an idle connection inside an opened process; scripts
start fresh CLI processes. JSON decodes and validates whole snapshots, while
SQLite opens connections and validates schema. These are plausible workload
explanations, not a measured causal breakdown.

An alternating JSON/SQLite/SQLite/JSON run of the unchanged
`TestXDP_DispatcherFillDrainRefill` used the same Rust debug binary and fresh
runtimes. All four passed and left empty inventories. Go-reported script durations
were 14.18 / 14.61 / 14.47 / 14.10 s respectively (Make wall times 14.24 / 14.67 /
14.53 / 14.17 s). This small isolated gap does not account for the full-batch
difference. Concurrent workload, writer serialization and run variation need
separate profiling before assigning the difference to a backend mechanism.
These are local samples, not a controlled performance benchmark.

Final labelled four-script batches passed on Rust JSON/SQLite in 6.18 s / 7.00 s;
Go passed driver XDP and TC and correctly skipped SKB/hardware-request fallback
on each store, with empty inventories. The focused ordinary runtime restoration
tests passed in 4.503 s (JSON) / 5.002 s (SQLite), versus baseline 37.947 s /
35.522 s; switch/restoration passed in 1.794 s / 1.918 s, versus 14.064 s /
13.491 s. In the final full gate, runtime restoration took 4.660 s / 5.142 s,
and switch/restoration took 1.802 s / 1.989 s.

Final batch timings were 159.025 s (JSON) and 150.447 s (SQLite), reversing the
baseline ordering. Per-script elapsed time exposes the concurrent workload:
fill/drain/refill took 127.39 s / 133.34 s inside those batches, versus roughly
14 s alone. Jumbo driver DEVMAP took 108.39 s in the JSON batch, and jumbo SKB
DEVMAP took 87.74 s in SQLite. These overlapping script durations are not added
to kernel totals. Shared runtime writer serialization, larger concurrent
inventories and host CPU scheduling are candidates for the next timing profile;
these measurements alone do not assign each its share.

Evidence: `/tmp/bpfman-kernel-timing-{before,after}.log`,
`/tmp/bpfman-lifecycle-{probe,admission}-summary.log`,
`/tmp/bpfman-lifecycle-focused.log` and `/tmp/bpfman-store-comparison-summary.log`.
The measurements and coverage decisions above are recorded here so the checkpoint
does not depend on retaining those temporary logs.

Next: remaining ordinary batch, attach/switch/unload and namespace lifecycle
scenarios, keeping genuine kernel recovery and ownership evidence.

A matching XDP fill/drain/refill script took 10.95 s with Go, 14.26 s with Rust
debug and 9.02 s with Rust release. These are single samples, not a benchmark;
compare optimized binaries before attributing debug-suite time to the language.

Golden fixtures should cover:

- protobuf and JSON output;
- SQLite rows and migrations;
- dispatcher configuration bytes;
- bpffs path derivation;
- image metadata and cache policy; and
- error-to-status mapping.

Where possible, the same normalized request and observation fixture should run
through the Go decision logic and the Rust core, comparing semantic results
rather than implementation-specific command names.

The outside-in test ladder is:

1. run pure Rust core and adapter contract tests;
2. build the Go `bpfman-shell` runner and the Rust `bpfman` CLI;
3. run the selected, unchanged `.bpfman` parity scripts against Rust;
4. run the same selection against Go on a clean runtime and compare the
   semantic outcome; and
5. use the existing nested-VM, stress, residue, gRPC, and operator suites for
   boundaries not fully exercised through the CLI.

The shell suite should be the acceptance gate for each vertical slice. It must
not wait for all adapters or public surfaces to be complete. Outside-in runs
execute each implementation independently and may additionally compare
normalized store, kernel, and filesystem observations.

### Script contention, isolation and broadcast fixture reuse (9 October 2026)

The parallel runner was sharing one runtime/store across every admitted script.
The first profile enabled the existing `bpfman_lock=trace` spans: ordinary debug
tracing does not include them. Completed CLI processes spent most of the batch
waiting behind unrelated mutations. These waits overlap and are not suite wall
time. The actual runner settings were `-test.parallel=16`, `GOMAXPROCS=16`.

All 84 admitted scripts passed in each profiled run, with unchanged assertions:

| Store/runtime scope | Make wall seconds | Accumulated lock-wait seconds | Longest lock wait seconds | XDP fill/drain/refill seconds |
| --- | ---: | ---: | ---: | ---: |
| JSON shared | 162.28 | 1817.112 | 9.264 | 126.47 |
| JSON isolated | 67.41 | 0.202 | 0.002 | 26.00 |
| SQLite shared | 157.85 | 1677.301 | 10.166 | 123.46 |
| SQLite isolated | 58.55 | 0.206 | 0.003 | 24.81 |

Shared lock-ownership spans totalled 154.048 s (JSON) and 146.159 s (SQLite),
close to each batch's wall time. Isolated ownership spans overlap across stores
and totalled approximately 209/211 s; they must not be interpreted as serial
wall time. Make's aggregate user/system CPU increased from 190.95/73.07 s to
281.84/112.05 s for JSON, and 183.50/75.09 s to 268.94/108.55 s for SQLite.
Isolation therefore improves parallel elapsed time; it is not a demonstrated
CPU reduction. Profiling used fresh runtimes, a temporary per-command Python
wrapper collecting the existing structured stderr spans and child CPU usage,
and debug binaries. Wrapper/logging overhead and sequential local-run variation
are present; this is not a controlled performance benchmark. The new telemetry
was added during this investigation. The measurements establish the severe
cross-script writer contention; they do not apportion every remaining cost.

`BPFMAN_E2E_ISOLATED_RUNTIME=1` now works in the existing Go script runner.
Each script receives its own runtime, store and bpffs mount, while pooled
interfaces and parallel scheduling remain unchanged. Cleanup explicitly checks
empty program/link/dispatcher inventories and artifact collections, even on
failure, then unmounts before temporary-directory removal. The Rust acceptance
gate uses this lane for all 84 scripts on each backend. It additionally runs
four scripts concurrently on a shared runtime per backend: file lifecycle,
XDP fill/drain/refill, XDP lifecycle and TC lifecycle. The shared lane retains
cross-script writer/store/dispatcher coverage. Go Make runs remain shared by
default; leaving isolation unset still exercises the complete corpus on one
store. Backend runners remain sequential under the existing suite lock.

The same isolated runner passed the Go shared selection on both stores:
51 scripts each, with the existing `requires-clsact-reclaim` capability skip
for `TestTC_ClsactReclaimedOnLastDetach`. No Go assertion or admission label
changed. Make wall times were 20.80 s (JSON) and 24.16 s (SQLite). Runner
implementation admission, capability override, interrupt cleanup and the new
telemetry failure/output-privacy contract also passed.

Optional `BPFMAN_E2E_SCRIPT_TIMELINE` JSONL now records configured parallelism,
queue/start/end markers, shell-process wall/user/system CPU milliseconds,
output byte count and exit code. End markers include residue checks and
unmounting. `BPFMAN_E2E_SCRIPT_NAME` identifies child commands for external
profilers. `BPFMAN_KERNEL_TIMINGS=1` also records runner-batch runtime scope and
implementation, preventing shared/isolated and Go/Rust timing confusion.
JSON spans separate read/decode/state validation/encoding, exposing byte lengths
and record counts without contents; SQLite dispatcher/link/unload writer
connections have a separate opening span. Existing lock and kernel spans remain
available through `RUST_LOG`, which is now forwarded through privileged runners.
No subscriber, clock or logging dependency was added to pure crates.

Broadcast fault tests reuse one loaded network/map fixture for both survivor
choices at each ingress-exclusion value. Failed attach restoration runs once,
then both failed-detach restorations and successful survivor transitions run
against the retained map and stable outer link. Reattaching the removed redirect
member reconstructs the ordered two-member chain between choices. This halves
network/program setup and removes duplicate attach-failure captures, retaining
both detach choices, exact map updates/identity, fragment reads, filtered
`EOPNOTSUPP` evidence and map-held kernel lifetime on final unload. All 300 ms
packet observation windows remain. The eight focused DEVMAP_HASH contracts
passed in 42.61 s. Opt-in `kernel-phase-timing` records setup, attach restoration,
each detach-restoration/survivor choice and managed teardown; these are portions
of the whole-test time, not additional test duration.

The complete timed `direnv exec . make rust-check lint-make` passed with 96
kernel tests, none failed or ignored, plus formatting, Clippy, all
userspace/compile-fail contracts, Rustdoc and Makefile lint. Each backend ran
84 isolated scripts and four concurrent shared-runtime scripts. Every selected
script passed and each isolated runtime plus the shared runtimes had empty
inventories/artifacts. No scripts were newly admitted or relabelled.

| Group | Previous checkpoint seconds | This full gate seconds |
| --- | ---: | ---: |
| Acceptance (both stores, including shared subset) | 309.472 | 126.418 |
| Broadcast restoration/lifetime | 130.351 | 82.874 |
| Other retained serial contracts | 342.387 | 330.757 |
| Complete kernel stage | 782.36 | 540.19 |

The full gate's ordinary JSON/SQLite batches took 46.384/47.890 s; their shared
subsets took 15.677/16.398 s. Complete backend tests took 62.097/64.321 s.
The broadcast phase totals were setup 24.005 s, attach restoration 14.546 s,
detach-restoration/survivors 37.083 s and managed teardown 4.776 s. Unmeasured
transitions and outer unmount/directory cleanup account for the remainder.
The whole kernel stage is approximately four minutes (31%) shorter than the
previous checkpoint. Unchanged groups also vary between runs; not every second
of the difference is attributed to the edits.

Post-gate focused checks rebuilt the runner/kernel suite after small phase-timer
and cleanup fail-fast corrections. Both backends again passed all 84 isolated
scripts and four shared scripts; eight DEVMAP_HASH contracts passed in 42.41 s.
The corrected output contained exactly ten whole-test timing records and 80
broadcast phase records, without duplicate zero-duration timers. Formatting,
Clippy, Makefile lint and the four runner admission/cancellation/telemetry
contracts passed. Evidence is in `/tmp/bpfman-contention-focused.log` and
`/tmp/bpfman-contention-runner-contracts.log`.

Evidence: `/tmp/bpfman-batch-{shared,isolated}-{json,sqlite}-summary.log`,
the corresponding `/tmp/bpfman-batch-profile.*` process records,
`/tmp/bpfman-go-isolation-summary.log`, `/tmp/bpfman-broadcast-reuse.log` and
`/tmp/bpfman-contention-rust-check.log`.

### TX/direct REDIRECT and DEVMAP unicast fixture reuse (9 October 2026)

The retained forwarding fault contracts now share one network and set of loaded
programs across both survivor choices. TX/direct REDIRECT still crosses both
actions and both continuation masks; DEVMAP/DEVMAP_HASH still crosses both masks
and map kinds. Every backend, requested ingress mode and frame shape remains in
the matrix. Each fixture first proves failed attach publication restores the
complete singleton snapshot and packet path. It then proves failed detach
publication restores each removal choice and its packet path. After the first
successful removal, reattaching the action/redirect member with its original
priority and mask reconstructs the chain for the second choice. The surviving
tail's managed link ID and the outer link remain stable.

DEVMAP checks retain exact map ID, entries and descriptor observations throughout
reconstruction, restoration and both survivors. Hash cases retain their sparse
key set, unused target and full-capacity rejection. Final unload still consumes
the retained map descriptor and waits for actual kernel reclamation. The existing
300 ms capture windows, exact frame/payload counts, counter deltas and genuine
fragment/helper-read evidence are unchanged. Shared setup removes the duplicate
attach-failure probe; both detach-failure probes remain. Each fixture performs
three post-failure packet observations. External successful forwarding and
survivor traffic remain in the unchanged parallel `.bpfman` scripts.

Opt-in `BPFMAN_KERNEL_TIMINGS=1` records non-overlapping successful-run phases
under `delivery.*` and `devmap.*`: setup, attach restoration, packet observation,
chain reconstruction, detach restoration, survivor transition, managed teardown
and fixture teardown. Packet observation includes counter reads and captures;
restoration timings exclude packet observation. Fixture teardown explicitly drops
the application, deletes the network namespace and unmounts/removes the temporary
runtime. All phases belong to the enclosing whole-test duration; they are not
additional wall time.

Formatting, Clippy and all 20 focused kernel contracts passed. The focused
timing output contained 832 completed forwarding phase records: 32 fixtures each
for direct and map-backed forwarding, with one attach-failure and two
detach-failure observations per fixture. A local analysis checked the expected
per-test phase counts and that phase totals fit within each whole-test duration.

| Affected group | Previous full gate seconds | Focused run seconds |
| --- | ---: | ---: |
| Ordinary TX/direct REDIRECT | 53.561 | 36.062 |
| Jumbo TX/direct REDIRECT and array DEVMAP | 81.768 | 53.861 |
| Ordinary array DEVMAP | 26.968 | 18.443 |
| Ordinary/jumbo DEVMAP_HASH unicast | 54.165 | 36.216 |
| All 20 affected contracts | 216.462 | 144.582 |

These local samples indicate approximately 72 seconds (33%) less elapsed time
in the affected contracts; they are not a controlled benchmark. The focused
phases attribute 69.725 s to packet observation, 36.980 s to fixture setup,
and only 0.190 s to outer fixture teardown. Managed teardown took 6.688 s,
including kernel map reclamation. Remaining phase time covers restoration,
reconstruction and survivor transitions. The capture windows remain the largest
measured cost after setup reuse; no shorter windows or skipped combinations were
introduced. No production behaviour, dependencies, Aya code, script assertions
or admission labels changed.

Focused evidence: `/tmp/bpfman-forwarding-fixtures-focused.log` and
`/tmp/bpfman-forwarding-fixtures-focused-summary.json`.

The final complete timed `direnv exec . make rust-check lint-make` exited
successfully: formatting, Clippy, userspace/compile-fail contracts, Rustdoc and
Makefile lint passed, alongside all 96 kernel tests with none failed or ignored.
Each backend passed all 84 isolated scripts and its four concurrent shared-runtime
scripts, retaining empty final inventories/artifacts. The final log contains
exactly 96 whole-test timing records and the same 832 completed forwarding phases
with the expected per-test counts.

| Group | Previous full gate seconds | Final full gate seconds |
| --- | ---: | ---: |
| Ordinary TX/direct REDIRECT | 53.561 | 35.763 |
| Jumbo TX/direct REDIRECT and array DEVMAP | 81.768 | 54.599 |
| Ordinary array DEVMAP | 26.968 | 18.061 |
| Ordinary/jumbo DEVMAP_HASH unicast | 54.165 | 36.224 |
| All 20 affected contracts | 216.462 | 144.647 |
| Acceptance (both stores, including shared subset) | 126.418 | 125.531 |
| Other retained contracts | 197.169 | 197.554 |
| Complete kernel stage | 540.19 | 467.88 |

The complete kernel stage is approximately 72 seconds (13%) shorter; the affected
contracts are approximately 33% shorter. Acceptance and other retained contracts
are nearly unchanged in this local comparison. Final forwarding phases total
69.806 s for packet observation, 36.931 s for setup, 6.619 s for managed teardown
and 0.187 s for outer fixture teardown. The remaining phase time covers
restoration, reconstruction and survivor transitions. Evidence:
`/tmp/bpfman-forwarding-fixtures-rust-check.log`,
`/tmp/bpfman-forwarding-fixtures-gate-summary.json` and
`/tmp/bpfman-forwarding-fixtures-gate-timeline.jsonl`.

### Workspace-law tests

Tests modelled on `rty` will enforce:

- strict crate dependency tiers;
- an allow-listed dependency closure for every pure crate;
- no undeclared feature that can introduce I/O into a pure closure;
- no legacy path dependency;
- no unsafe code outside explicitly reviewed locations; and
- documentation of every workspace crate when it is added.

## Rust coding conventions

The working guidelines are recorded in `rust/AGENTS.md`, adapted from `rty`.
`lib.rs` and `mod.rs` remain thin facades for types, exported symbols, and API
documentation; implementations live in private modules. `unreachable_pub` is
part of the normal lint gate. Adapter-specific errors remain opaque and are
translated to backend-independent application categories; library code uses
`thiserror` in adapters, while the binary uses `anyhow` to render retained cause
chains. Pure validation errors implement core traits directly and retain an
empty external dependency closure; Aya enables thiserror's std feature in the
application graph.

The new workspace begins with these conventions:

- Rust 2024 edition and a workspace-wide minimum supported Rust version;
- workspace-owned dependency versions and lints;
- `unsafe_code` forbidden by default;
- no `unwrap`, `expect`, or `panic` in production paths;
- typed library errors and source-preserving error chains;
- private fields and modules by default;
- minimal public APIs;
- no global runtime layout or mutable singleton state;
- no generated wire type used as a domain type; and
- formatter, linter, test, dependency-tier, and documentation gates driven by
  repository Make targets that explicitly select `rust/Cargo.toml`.

## Futures

Use Rust's type system to make important lifecycle mistakes harder to express,
without adding abstractions for their own sake. As operation flows settle,
consider typestate for transitions such as XDP staging, switching, publication,
restoration, and retirement. A type should encode a real precondition or
ownership change; operation-specific state machines remain clearer than a
generic workflow framework.

Represent supported choices, including XDP attachment modes and fallback
behavior, as explicit enums. Exhaustive matches should make new modes and
combinations visible at the points that must handle them. Parse external strings
at the boundary and persist only fields required for Go compatibility.

Prefer borrowing when an operation only needs temporary access to state or
authority. Continue using owned, non-cloneable receipts and descriptors where
ownership must survive an error or explicit retry. This keeps copies down while
making resource lifetime visible in function signatures.

Keep outcomes and errors structured around operation stage and recovery state.
Callers should distinguish, for example, an operation with no acquired resource
from a committed operation with cleanup still pending, without parsing messages.
Use exhaustive matches to make newly introduced outcomes receive deliberate
handling.

The implementation remains synchronous. Do not introduce async/await, an async
runtime, or synchronous wrappers around async libraries. Scoped threads are
appropriate where required for namespace-sensitive kernel operations.

Prioritize the existing Go runner for packet acceptance and reduce kernel-gate
duration without reducing coverage. Its pooled interface leases and scheduler
already support concurrent scripts. The current Rust runner
forces `--test-threads=1`: several fixtures use PID-only interface/namespace names,
and each DSL test launches a separate Go runner that takes the system-wide
`/tmp/bpfman-e2e.lock`. Simply enabling test threads would introduce name collisions
and competing runner locks. Batch admitted scripts into one Go runner per backend,
running the backend batches sequentially, so its existing parallel scheduler and
serial/exclusive pragmas apply. Prefer this harness for future packet scenarios;
keep Rust-specific ownership and fault tests separate. Give remaining Rust fixtures
independent names or process isolation, then enable bounded parallel execution and
measure per-test timings.
Keep packet assertions, cleanup checks and both-store coverage intact.

## Delivery sequence

### Phase 0: contracts and workspace

- Create the independent `rust/` workspace.
- Add workspace dependency, lint, tier, purity, and no-legacy-dependency gates.
- Write the compatibility matrix against the Go implementation.
- Capture initial golden fixtures before implementation choices can influence
  them.
- Record the `.bpfman` corpus as the outside-in behavioural baseline.
- Add one runner knob that selects an absolute bpfman binary and consistently
  configures both `BPFMAN_BIN` and `PATH`.
- Establish the per-feature parity manifest and demonstrate that the harness
  can run a small unchanged script selection against the Go binary through the
  new knob.

### Phase 1: model and observation

- Implement invariant-rich domain types.
- Read the existing SQLite schema without mutation.
- Model store, kernel, and bpffs observations.
- Implement read-only get, list, and inspect views.
- Run the corresponding CLI-only and read-only `.bpfman` scripts against Rust.

### Phase 2: first mutating vertical slice

- Load selected programs from a local ELF file.
- Publish a batch atomically to SQLite.
- Attach and detach a standalone tracepoint using the pending-link protocol.
- Unload the program and verify residue-free cleanup.
- Exercise every failure boundary with pure-machine and adapter tests.
- Admit the unchanged tracepoint lifecycle scripts to the Rust parity lane.

Phase 2 checkpoint: the tracepoint slice now includes batch selection and one
atomic commit, private per-program maps, pending-link attachment, detach, and
unload. The gate exercises both stores, failure compensation and explicit retry,
cancellation, real-kernel lifecycle tests, and the unchanged single- and
multi-program tracepoint DSL scripts. The batch implementation pins only maps
referenced by each loaded program so unload can verify their ownership.
XDP extension load/get/unload provides the entry to Phase 3, using Go's unpinned
one-slot verification dispatcher. Both stores pass unchanged XDP load/get and
named-selection DSL scripts.

### Phase 3: dispatcher proof — in progress

Completed checkpoint:

- Singleton legacy TC ingress loading/attachment with its native dispatcher,
  exact kernel-assigned filter handles, atomic publication, durable clsact ownership,
  malformed/foreign identity refusal, cancellation, compensation, and explicit
  cleanup retries on both stores. Attached-program unload adopts every attachment
  before effects, defers program teardown until all TC prerequisites finish, and
  retains nested cleanup history across explicit retries.
- Explicit XDP namespaces with retained descriptors, isolated worker threads,
  persisted paths, namespace identity refusal/retry, and unchanged namespace DSL
  scripts on both stores. Namespace-aware JSON snapshots use format 6.
- Twelve additional unchanged XDP scripts per backend covering ten-slot and
  fill/drain traffic, exact counters, proceed-on chains, ordering, and independent
  interfaces; shared DSL assertions also require empty inventories and XDP pins.
- Attached XDP program unload with retained dispatcher prerequisites, explicit
  recovery, surviving-member traffic, and the unchanged survivor-rebuild script.
- Read-only dispatcher listing from one validated store snapshot, with JSON output
  and namespace/interface filters, without the writer lock.
- Runtime multi-member attach/detach, restoration-aware error/retry ownership,
  committed-snapshot reporting after retirement failures, and shared fault tests.
- Real runtime packet acceptance and unchanged multi-member CLI scripts on both stores.
- Complete kernel revision staging, conditional outer-link switching, retained
  restoration evidence, and real adapter traffic/failure tests on both stores.
- Atomic conditional multi-member snapshot replacement in both stores, bounded
  slot observations, and retained complete-snapshot receipts on failure.
- Pure replacement planning, multi-member ABI encoding, consuming switch/publication/
  restoration transitions, and stable per-instruction cleanup retry history.
- Pure one-slot XDP configuration and dependency-aware cleanup policy.
- First attach and last detach in current or explicit namespaces with configurable
  driver/SKB/hardware requests and non-SKB fallback; actual offload is unverified.
- Atomic dispatcher/member publication and conditional deletion on both stores.
- Cancellation before commit, retained compensation receipts, and explicit retry.
- Unchanged XDP link round-trip and last-detach dispatcher scripts on both stores.
- Real-kernel refusal of foreign attachments and mismatched pins, synchronous detach
  with a retained descriptor, failed commit/deletion, and reattachment without residue.
- Store contracts for stale/foreign receipts, plus SQLite rollback, ignored-deletion,
  malformed-snapshot, and JSON format-compatibility checks.

The kernel-boundary refactor is complete for this supported surface; see
[Kernel and bpffs adapter](#injectable-kernel-boundary).
The [one → two → one → zero milestone](#xdp-dispatcher-replacement-checkpoint)
is implemented, including failed publication, failed restoration, explicit
retries, runtime traffic acceptance, attached XDP program unload, and broader
fill/drain and chain-execution acceptance using unchanged Go scripts. Explicit
namespaces and their round-trip/rebuild scripts are also implemented. Selectable
XDP modes follow the legacy Rust per-interface `xdp_mode` configuration, driver
default, and non-SKB fallback. Current Go has no interface-mode configuration;
its existing driver-mode behavior remains the default. Modes use the existing
consuming resource transitions; no additional typestate is needed for this
data-free choice. Native and generic SKB multi-buffer execution are implemented:
ELF-declared support drives the pure per-slot ABI configuration and dispatcher selection,
with all-member support required to enable fragments. Both stores run the
unchanged normal-MTU fragments script and jumbo driver/SKB veth lifecycle
acceptance; see [the verified packet boundary](../../rust/README.md#xdp-multi-buffer-packets).
Native and explicit SKB TX/direct-REDIRECT packet delivery are now covered on
both stores, including proceed-on continuation, replacement, failed attach/detach
publication restoration, either survivor, and last detach. The tests use a
Go raw-Ethernet fixture solely to send and capture packets, keeping packet syscalls
and unsafe code out of the Rust workspace. No production changes were required.
DEVMAP forwarding is also verified on both stores and modes, with live map target
updates, PASS/DROP fallback, proceed-on behavior, stable map identity and contents
through replacement/rollback/survivor rebuilding, and eventual reclamation after
unload. The test fixture updates maps inside the private network namespace and
supports Aya's four/eight-byte DEVMAP value layouts. No production changes were
needed. The same direct and DEVMAP lifecycle scenarios now also forward genuine
multi-buffer frames in native and SKB modes on both stores. Exact fragment/tail/
boundary counters at ingress and receiving peers supplement full 8014-byte frame
captures with an offset-dependent payload pattern. Broadcast and ingress exclusion
are also verified in both modes/stores with ordinary-sized frames. Multi-buffer
broadcast preserves single-target forwarding but Linux 6.18.54 rejects cloning
for multiple eligible targets with `EOPNOTSUPP`; an error tracepoint observer
proves this kernel boundary through replacement and rollback. DEVMAP egress now
loads as native XDP and proves ordinary-frame PASS/DROP on both stores and ingress
modes, including map-held program lifetime through managed unload. Multi-buffer
array-map egress awaits upstream Aya extension fragment flags; explicit rejection tests
retain successful jumbo unicast without egress. DEVMAP_HASH now repeats ordinary
and genuine multi-buffer unicast on both stores/modes, with sparse-key semantics,
capacity rejection, exact contents including an unused entry, live target updates,
PASS/DROP fallback, replacement/restoration, and reclamation. No production changes
were needed. DEVMAP_HASH broadcast now repeats ingress exclusion, empty/sparse map
behavior, live updates, continuation into DROP, replacement/restoration, either
survivor, and reclamation, with exact sparse key-set checks throughout. Both modes
and stores retain successful ordinary-frame fan-out and genuine single-target
jumbo delivery; multiple-target jumbo rejection is observed explicitly. Hash-backed
egress now proves ordinary and genuine multi-buffer PASS/DROP, interface context,
fragment helper reads, sparse-key preservation, compatible program updates,
replacement/restoration, and map-held lifetime through managed unload and retry.
Its first native egress update initializes compatible hash ownership on Linux
6.18.54; the array-map Aya boundary remains unchanged. Physical NIC packet
execution and actual hardware offload remain unverified. TC ingress now has a
complete one-to-ten-member ingress replacement lifecycle with attached-program
unload. Batched Go-runner acceptance is now the normal binary-behavior gate. The testing
audit tracks remaining behavioural migration; corpus gaps determine capability work.


TC ingress uses Go's 84-byte `CONFIG` ABI and a separate unpinned native TC verifier
target. Managed classifier selections load as EXT; the traffic dispatcher loads as
SCHED_CLS. The explicit `TcAttachOptions::Netlink` public API prevents Aya's default
TCX selection on modern kernels. Aya supplies the assigned filter handle; cleanup
validates that exact handle, priority 50, interface, ETH_P_ALL protocol, chain zero,
and dispatcher program ID.
As in legacy Rust, taking and forgetting Aya's descriptor-free netlink link
transfers detach responsibility; the new implementation retains it in a consuming
receipt that survives failed cleanup and lock admission.

Aya remains unpatched. Safe `rustix::net` sockets supply qdisc/filter inspection
and exact deletion, which Aya's public API lacks. The dependency law permits
route-netlink in the concrete kernel adapter; managed filesystem operations still
belong in `bpfman-fs`, and runtime/pure crates remain free of syscall dependencies.

Clsact ownership is separate from operator metadata and Go's SQLite schema.
`bpfman-fs` creates a confined, singly linked one-byte file at
`<runtime>/tc/dispatcher_<nsid>_<ifindex>_<revision>`: zero means borrowed, one means created.
Each staged replacement copies that evidence into its fresh revision; retiring or
compensating a revision removes only its own evidence.
The receipt is written before publication and retained with the revision's pins.
Reopened detach preserves a borrowed empty clsact; a created clsact is reclaimed
only after both ingress and egress filter dumps are empty. Foreign filters cause
ownership to be relinquished while preserving the qdisc. A classic ingress qdisc
is refused. Missing/malformed evidence or a foreign program at the stored filter
handle refuses teardown before effects. These netlink inspections and deletions
are separate requests; they do not provide a compare-and-swap against concurrent
privileged tools changing the same qdisc/filter outside bpfman's writer lock.

SQLite still uses Go schema 2, including its existing `priority` and `filter_handle`
dispatcher columns. TC teardown across Go and Rust is not yet supported: Go-created
attachments lack Rust's clsact ownership receipt. New JSON stores use format 8;
format 7 retains singleton TC and refuses replacement. Formats 1–6 retain their
previous operations without implicit upgrade. Shared store
contracts prove atomic publication,
signed proceed-on actions, vacant-point refusal, stale-receipt refusal, and
wrong-runtime retry ownership. Real veth tests prove exact marked-packet drops and
explicit continuation into final OK, preserved foreign ingress/egress filters at
the same priority, reopened borrowed/owned clsact behavior, replaced-filter refusal,
malformed ownership evidence, publication/cleanup faults, explicit retry, and
pre-admission/late-commit cancellation. `make rust-test-tc-ingress` also selects the
unchanged `TestTC_LoadAndGet` and `TestTC_LinkRoundTrip` corpus on both stores.

Attached TC unload shares one writer scope with program teardown. Admission adopts
all exact filter, namespace, dispatcher-pin, clsact-ownership and conditional store
receipts before removing anything. The existing TC cleanup interpreter consumes
filter → stage → record ownership; a failure retains blocked dependencies and
prevents program/map/bytecode removal. Independent attachments each receive one
pass. `UnloadReport::tc_attempts()` exposes each link's nested cleanup history;
`retry_unload` resumes only unresolved ownership, including after reopening the
application. Cancelled lock admission retains all receipts; cancellation after
teardown starts does not abandon the pass. A foreign runtime is refused. New links
attached by a later writer block retained program teardown until explicitly handled.
Program cleanup retries never repeat completed TC detaches. Reconstructing a lost
in-memory cleanup report after process failure remains separate recovery work.

The shared fake tests cover multiple interfaces, preflight failure on a later
attachment, filter/stage/store failures, blocked program effects, history across
repeated failures, cancellation, foreign-runtime/kernel retries, new attachments
between passes, and program cleanup failures. Real veth acceptance exercises two
namespaces per managed program, malformed later ownership before any detach,
reopened cleanup and retry, exact packet counts, borrowed/owned clsact, preserved
foreign ingress/egress filters, conditional deletion and program-record faults,
new-attachment refusal, and final kernel reclamation. CLI acceptance now unloads
an attached TC program as well as exercising explicit detach.

TC replacement stages every desired member in fresh revision/slot pins before
switching traffic. It reuses the pure consuming replacement/publication protocol:
failed restoration hides both revisions from cleanup, and successful publication
allows only retirement of the old revision. `TcError` and `TcReport` expose actual
restoration history and committed snapshots when retirement requires a retry.
Opaque cleanup receipts remain bound to their original runtime and kernel.
Retirement attempts every independent old extension pin once, collecting all
failures; native dispatcher, ownership evidence and revision-directory removal
wait for those extension removals. Real fault acceptance replaces two old pin
entries and proves a later independent pin is removed while the native pin remains,
then repairs only the injected entries and retries the retained ownership.

Aya's public `SchedClassifierLink::attached` and `SchedClassifier::attach_to_link`
replace the program at the existing filter tuple without changing Aya. Retained
native program handles support restoration after a failed store publication.
Safe route-netlink checks verify the exact program identity before switching or
restoring; they are separate requests, not an expected-program CAS against an
external privileged writer. Member IDs, metadata, creation times and requested
priorities remain stable while native/EXT IDs, slots and revision change together.
`dispatcher get tc-ingress` and dispatcher listing expose complete stored membership.

The shared replacement tests exercise capacity, ordering, repeated-program unload,
pre/post-switch faults, failed restoration, retirement, cancellation and foreign
retry authority on both stores. Real veth tests prove UNSPEC continuation into SHOT,
higher-priority OK stopping, exact per-program counters, rollback to the old chain,
reopened survivor unload and non-last detach, with owned/borrowed clsact and foreign
filters. Twelve unchanged Go scripts cover chain stop/continue, ten-slot execution,
priority zero/name ordering, fill/drain/refill, signed encoding, namespace rebuild,
survivor unload and clsact reclamation on both stores. The script runner's explicit
`BPFMAN_E2E_CLSACT_RECLAIM=true` capability setting runs the unchanged reclaim script
against Rust; Go's production reclaim policy remains unchanged.

This TC slice follows capabilities already present in Go, including replacement
and attached-program unload; the XDP advantages recorded earlier remain. TC egress,
outer-filter observations, deleted-namespace/orphan repair, the uprobe mount-namespace
helper, and TCX ordering remain later work in this phase. Unsupported operations
continue to fail clearly.

This phase validates the architecture. If the effect or state-machine model is
wrong, change it here before broadening feature coverage.

### Phase 4: remaining capabilities

- Map sharing and PinByName coordination.
- Kprobe, uprobe, fentry, fexit, LSM, and remaining attachment types.
- OCI pull, cache, authentication, and signature verification.
- Namespace helper and CSI integration.
- Grow the parity manifest to the full applicable `.bpfman` corpus, including
  file-bytecode, image-bytecode, parallel, and stress lanes.

### Phase 5: public surfaces and cutover

- Complete gRPC and CLI compatibility.
- Run the full Go and Rust outside-in suites independently and compare their
  semantic results.
- Switch packaging and production entry points only after the compatibility
  matrix and residue tests pass.

The Go implementation remains available until cutover is complete. The legacy
Rust source remains available after cutover unless a separate decision removes
it.

## Initial vertical-slice questions

The first implementation work should settle these points with code and tests:

- whether operation-specific effect enums are preferable to one workspace-wide
  effect enum;
- whether the runtime needs interpreter traits at all, or concrete adapters and
  exhaustive routing are sufficient;
- how effect error sources are retained while the pure core sees only stable
  classifications;
- whether independent effects need batch execution in the first version;
- how much dispatcher state belongs in `bpfman-model` versus `bpfman-core`;
  and
- whether compatibility requires byte-for-byte preservation of every current
  JSON and diagnostic field or only documented public output.

These questions should be answered by the local-load plus standalone-attach
slice and then revisited during the dispatcher proof. They should not be hidden
behind a generic framework designed in advance.


### Bounded parallel forwarding fault fixtures (9 October 2026)

Retained TX/direct REDIRECT, DEVMAP/DEVMAP_HASH unicast and broadcast contracts
now distribute their independent fixtures across two synchronous scoped workers.
Direct forwarding gives each worker two action/mask combinations; map unicast
splits the continuation masks, and broadcast splits ingress exclusion. Every
worker constructs and tears down its own application and fixture. The enclosing
kernel tests still run with `--test-threads=1`; backend script batches remain
sequential, with the existing Go runner's internal parallel scheduling.

The concurrency audit checked the remaining shared process/kernel boundaries:

- Namespace names use a process ID and atomic sequence; fixed interface names
  and packet markers are confined to each private network namespace. Commands
  select namespaces explicitly. Aya namespace entry uses disposable threads and
  does not change the fixture worker's network namespace.
- Runtime roots, bpffs mounts, stores, writer-lock inodes, fault decorators and
  counters are fixture-local. Workers share the runner's private mount namespace,
  but mount/unmount only their own temporary roots. They do not mutate process
  environment or install global subscribers.
- Broadcast tracepoints are global observations filtered by both the fixture's
  exact DEVMAP ID and input interface. Map lifetime checks observe the exact
  retained map ID, rather than global inventory counts. Tracefs format reads and
  bytecode fixture reads are read-only.
- All scoped workers are joined before forwarding a panic to libtest, so sibling
  fixture cleanup finishes before the next test or backend starts. Adapter
  namespace helpers remain scoped within each fixture operation.

No fault combinations, restoration/ownership assertions, survivor choices,
map identity/lifetime checks or 300 ms packet observation windows were removed.
No production code, dependencies, Aya code or script admission labels changed.
The worker limit bounds fixture concurrency; the adapter can additionally create
short-lived namespace threads and helper subprocesses.

`BPFMAN_KERNEL_TIMINGS=1` retains whole-test timings and parent-test attribution
for phase records. New `forwarding.worker` phases cover each complete worker,
including fixture cleanup. Existing phases overlap across workers, though direct
and unicast phases remain non-overlapping within each successful fixture.
Summed phase durations measure accumulated worker time, not test wall time;
worker records also contain the nested fixture phases and cannot be added to them.

Formatting and Clippy passed, followed by all 36 focused kernel contracts on both
stores. Telemetry contained all 832 forwarding fixture phases and 160 broadcast
fixture phases, plus 80 completed worker records. Per-test phase counts matched
the original fixture matrix. Worker totals fit within twice the enclosing test's
wall time, and fixture phase totals fit within their worker totals.

| Group | Previous full gate seconds | Two-worker focused seconds |
| --- | ---: | ---: |
| Ordinary TX/direct REDIRECT | 35.763 | 20.236 |
| Jumbo TX/direct REDIRECT and array DEVMAP | 54.599 | 29.740 |
| Ordinary array DEVMAP | 18.061 | 9.647 |
| Ordinary/jumbo DEVMAP_HASH unicast | 36.224 | 19.688 |
| Broadcast | 82.583 | 47.392 |
| All 36 affected contracts | 227.230 | 126.703 |

The focused run reduced elapsed time by approximately 100.5 seconds (44%). JSON
contributed 61.859 seconds and SQLite 64.844 seconds. Accumulated worker time was
252.403 seconds, illustrating the overlap; this change reduces elapsed time,
not total work. Existing capture windows remained intact. These are local
before/after samples, not a controlled benchmark.

Focused evidence: `/tmp/bpfman-forwarding-parallel-focused.log` and
`/tmp/bpfman-forwarding-parallel-focused-summary.json`.


The complete timed `direnv exec . make rust-check lint-make` exited successfully:
formatting, Clippy, userspace/compile-fail contracts, Rustdoc with warnings denied
and Makefile lint passed. All 96 kernel tests passed, with none failed or ignored.
Both stores passed all 84 isolated scripts and their four concurrent shared-runtime
scripts, including final inventory/artifact checks. The final output again
contained all 1,072 expected completed phase records and 96 whole-test records.

| Group | Previous full gate seconds | Two-worker full gate seconds |
| --- | ---: | ---: |
| Ordinary TX/direct REDIRECT | 35.763 | 18.839 |
| Jumbo TX/direct REDIRECT and array DEVMAP | 54.599 | 29.011 |
| Ordinary array DEVMAP | 18.061 | 9.762 |
| Ordinary/jumbo DEVMAP_HASH unicast | 36.224 | 19.480 |
| Broadcast | 82.583 | 45.291 |
| All 36 affected contracts | 227.230 | 122.383 |
| Both backend acceptance batches | 125.531 | 131.890 |
| Other retained kernel tests | 114.971 | 114.458 |
| Complete kernel stage | 467.88 | 368.81 |

The affected contracts saved 104.847 seconds (46%); the complete kernel stage
saved 99.07 seconds (21%), finishing in 6m09s. The unchanged script acceptance
batches varied upward by approximately six seconds; other retained tests were
nearly unchanged. JSON's affected contracts took 59.872 seconds and SQLite's
62.511 seconds. Summed worker time was 243.373 seconds, so phase totals must
continue to be interpreted as overlapping work rather than wall time.

Final evidence: `/tmp/bpfman-forwarding-parallel-rust-check.log`,
`/tmp/bpfman-forwarding-parallel-gate-summary.json` and
`/tmp/bpfman-forwarding-parallel-gate-timeline.jsonl`.


### Concurrent kernel stages and named script cases (9 October 2026)

The complete Rust gate now separates script acceptance from retained kernel
contracts. `rust/test-kernel.sh` discovers the complete libtest manifest, rejects
unclassified names and selects each case exactly once. Corpus and legacy Go
runner stages run serially. JSON contracts finish before SQLite contracts start.
Independent contracts use four top-level test threads by default; one and two
are available through `BPFMAN_KERNEL_TEST_THREADS`. With concurrent outer tests,
forwarding has one fixture worker rather than two, so nested scheduling does not
multiply the fixture concurrency. Focused targets keep their existing serial
outer runner and two forwarding workers.

All in-process network fixtures now share an atomic identity allocator. Short
hexadecimal interface names stay within Linux's 15-byte limit. The ordinary XDP
traffic helper receives its fixture's sender/receiver handle instead of
reconstructing names from the process ID. Fragment and namespace-replacement
fixtures also receive unique names. Network namespace entry still happens in
disposable adapter threads; runtime directories, mounts, fault state, descriptors
and counters remain fixture-owned. External commands inherit per-command
environment; concurrent tests do not mutate process environment or subscribers.
Tracepoint tests retain exact kernel IDs and private maps. Broadcast errors
remain filtered by exact map ID and interface.

The first four-thread run found a genuine shared observation: XDP loading checks
the global set of `xdp_dispatcher` programs to prove its temporary verification
target is released. It could mistake another test's live dispatcher for a leak.
That test now runs in a serial global-inventory stage per backend, with the
original assertion and reclamation deadline unchanged. The same exploratory run
reported one duplicate stimulus frame. Its cause was not confirmed. Packet
failures now include capture interface, sequence, kernel interface index, packet
type and actual/expected MAC addresses. The subsequent complete four-thread
contracts run passed, and three repetitions of concurrent switch/runtime/unload
contracts per store passed all 18 tests without another duplicate. Retain these
diagnostics rather than treating one unreproduced report as an established cause.

| Retained-contract scheduling | Passed cases | Stage seconds |
| --- | ---: | ---: |
| Previous serial outer runner, two forwarding workers | 94 | 236.841 |
| Two outer threads, one forwarding worker | 94 | 182.65 |
| Four outer threads, serial global-inventory stages | 94 | 95.46 |

These are local samples, not a controlled benchmark; the two-thread sample
preceded the explicit global-inventory separation. No assertions, fault cases,
survivor choices or 300 ms capture windows were removed. The serial load checks
are scheduled separately, not skipped. Fast shell runner contracts exercise
complete selection, exact filters, backend ordering, serial global stages,
invalid admission and stop-on-failure. They run in the userspace gate without
sudo or kernel resources.

The script bottleneck also contained an avoidable serial dimension: each DEVMAP
file ran its four fallback/proceed-on combinations one after another. The Go
runner now supports reusable `#pragma cases` metadata with named, typed JSON
input objects. Each case is a normal child subtest with the existing scheduler,
pooled network allocator, deadline, cancellation, defers and residue checks.
Isolation creates a fresh store and bpffs per case. Serial/exclusive labels and
repeat policy still apply; labels select files, and test-name filters can select
individual cases. Parent file subtests preserve existing selection/admission
checks. Malformed, duplicate or empty cases fail admission.

Eight existing DEVMAP/DEVMAP_HASH files declare `drop-stop`, `drop-continue`,
`pass-stop` and `pass-continue`. Their existing scenario body and assertions are
unchanged. The runner supplies only `BPFMAN_E2E_CASE` JSON, preventing parameter
keys from overriding runtime/binary settings; plain scripts have inherited case
input cleared. Standalone shell execution defaults to the complete four-case
matrix. There remain 84 admitted files (52 shared, 32 Rust-only), but isolated
acceptance now executes 108 script processes per backend: 76 plain files and
32 cases from eight parameterized files. All eight files retain their existing
Rust-only labels; this scheduling change makes no new Go parity claim.

Metadata validation, typed environment isolation and the complete Go shell test
suite passed, along with repository Go lint and runner admission/cancellation/
telemetry contracts. The runner guide documents case syntax and focused
selection. Suite/stage timers read Linux uptime to report elapsed time independent
of wall-clock adjustments; whole-test and phase totals overlap under concurrency
and must not be summed as wall time.

Evidence before the final gate: `/tmp/bpfman-kernel-parallel-two.log`,
`/tmp/bpfman-kernel-parallel-four.log` (exploratory failures),
`/tmp/bpfman-kernel-parallel-four-isolated.log`,
`/tmp/bpfman-stimulus-concurrency.log`, `/tmp/bpfman-script-cases-unit.log`,
`/tmp/bpfman-script-cases-runner-contracts.log` and
`/tmp/bpfman-script-cases-go-lint.log`.


The first full acceptance attempt caught a script-input comparison error:
structured objects cannot be compared directly with `null` in the DSL. The
helper now uses jq to produce a scalar Boolean, as other structured comparisons
do. The full gate was rerun after this correction; no scripts or cases were
removed or relabelled to make it pass.

Final `direnv exec . make rust-check lint-make` exited successfully. Formatting,
Clippy, all userspace/compile-fail contracts, the fast runner scheduling contracts,
Rustdoc with warnings denied and Makefile lint passed. All 96 discovered kernel
tests passed exactly once, with none ignored. Per-test phase counts matched the
unchanged forwarding/broadcast matrix: 832 forwarding phases, 160 broadcast
phases and 40 worker records (one per helper invocation under four-thread
execution), all completed. The reduction from 80 to 40 worker records reflects
removing nested two-worker scheduling; it removes no fixture or observation.

| Complete gate component | Previous kernel stage seconds | Final seconds |
| --- | ---: | ---: |
| Both backend script batches | 131.890 | 116.83 |
| Retained kernel contracts | 236.841 | 96.87 |
| Complete kernel stage | 368.81 | 213.75 |

The final stage breakdown was corpus 116.83 s, JSON global inventory 0.80 s,
JSON independent contracts 45.83 s, SQLite global inventory 0.85 s,
SQLite independent contracts 48.91 s and legacy Go 0.48 s. Suite elapsed time
includes discovery and process-launch overhead, so it differs slightly from the
sum of stages. The kernel stage saved 155.06 s (42%), finishing in 3m34s.
These are local samples, not a controlled benchmark or full build-duration claim.

Each backend passed all 84 admitted parent files, every named case from the eight
DEVMAP files, and its four concurrent shared-runtime scripts. The final timeline
contained 225 successful process/start/end records: 108 isolated cases per store,
four shared cases per store and one legacy Go script. All 64 named DEVMAP cases
across both stores completed cleanup; inventory/artifact checks precede their end
markers. JSON's isolated/shared batches took 41.434/15.511 s and SQLite's took
43.543/16.211 s. Named DEVMAP cases typically took around six seconds; other long
scripts still determine the batch completion time. The unexplained exploratory
duplicate-frame report did not recur in the stress run or complete final gate;
keep its diagnostic note and enriched probe output for future investigation.

Final evidence: `/tmp/bpfman-kernel-stages-final-rust-check.log`,
`/tmp/bpfman-kernel-stages-final-summary.json`,
`/tmp/bpfman-kernel-stages-final-phase-matrix.json` and
`/tmp/bpfman-kernel-stages-final-gate-timeline.jsonl`.


### Expanded packet cases and script throughput (9 October 2026)

The saved timeline identified the next corpus bottleneck: broadcast files took
20–23 seconds each, running independent ingress-inclusion and ingress-exclusion
fixtures serially. Eight broadcast files now declare `include-ingress` and
`exclude-ingress` cases. Eight TX/direct REDIRECT files declare `stop` and
`continue` cases for their independent proceed-on masks. They use the existing
Go runner's case metadata, scheduler and pooled networks; no additional runner
syntax or DSL builtin was needed. The packet scenario bodies, survivor checks,
fragment/error evidence and 300 ms capture windows remain unchanged.

A shared `delivery_case` helper reads typed JSON input using existing `exec` and
jq commands. Each family validates its supported parameters before creating a
fixture. With input absent, direct shell execution still runs the complete
matrix; an unprivileged shell check also verified the helper's null default.
Case parameters select only the independent dimension: they cannot replace the
script's object, mode, frame size, action or expected delivery assertions.

There remain 84 admitted files: 52 shared and 32 Rust-only. Twenty-four files now
expand to 64 named cases, while 60 plain files still execute once. Isolated
acceptance therefore runs 124 script processes per backend. Existing labels are
unchanged. Go passed all eight cases from the four shared ordinary-driver
TX/REDIRECT and array/hash broadcast scripts, including final inventory/artifact
checks. That comparison does not promote any Rust-only scenario to shared parity.

Splitting these cases alone passed the complete gate in 210.99 s (3m31s), versus
213.75 s (3m34s): only a small improvement. All 96 kernel tests ran exactly once,
and every file/case passed on both stores. The isolated JSON/SQLite batches took
40.999/41.303 s including invocation overhead; shared coverage took
15.858/16.207 s. The process timeline still showed work queued behind the Go
runner's 16 slots. Shortening individual scripts did not remove the throughput
limit.

A separate measurement used the runner's existing `PARALLEL=32` setting, with
backends sequential, isolated runtimes and private mount namespaces:

| Isolated process timeline | 16 slots, seconds | 32 slots, seconds |
| --- | ---: | ---: |
| JSON | 40.929 | 33.397 |
| SQLite | 41.227 | 36.666 |

Both 32-slot batches passed every one of their 124 processes and cleanup checks.
Timeline start/end pairs confirmed a maximum of 32 active cases and no unresolved
case at the end. These are local samples, not a controlled benchmark; the table
uses process/start/end markers rather than mixing them with invocation totals.
More concurrency raises some individual process times while reducing batch wall
time.

Rust's isolated acceptance now requests 32 slots from the existing Go runner;
the explicit shared-runtime subset requests four. The normal Make `PARALLEL`
override survives sudo and chooses the script limit independently of retained
kernel-test threads. A focused `PARALLEL=2` run verified the actual timeline limit
and completed its tracepoint script. Backend order, suite locks, admission,
timeouts, defers and residue assertions are unchanged.

Evidence: `/tmp/bpfman-expanded-cases-rust-check.log`,
`/tmp/bpfman-expanded-cases-summary.json`,
`/tmp/bpfman-expanded-cases-phase-matrix.json`,
`/tmp/bpfman-expanded-cases-gate-timeline.jsonl`,
`/tmp/bpfman-case-concurrency-32.log`,
`/tmp/bpfman-case-concurrency-32-summary.json`,
`/tmp/bpfman-case-concurrency-32-timeline.jsonl`,
`/tmp/bpfman-expanded-cases-go.log`,
`/tmp/bpfman-expanded-cases-go-timeline.jsonl` and
`/tmp/bpfman-script-parallel-override-timeline.jsonl`.

The first full gate after selecting 32 slots found a cleanup failure on JSON's
`TestMultiProgTracepoint_LoadAttachDetachUnload`: the script process exited zero
and inventory/artifact checks succeeded, but normal bpffs unmount returned
`EBUSY`. Thirty-one other cases were active. SQLite completed its acceptance;
the corpus stage failed and stopped subsequent stages. The mount holder was not
identified, so higher concurrency is an observed trigger rather than an
established root cause. A process's zero exit code alone is not proof that its
runner cleanup passed; inspect the suite outcome as well as timeline pairs.

Isolated-runtime cleanup now waits up to five seconds only for `EBUSY` references
to drain, polling every 10 ms before attempting normal unmount again. Successful
cleanup still requires the mount to release; no lazy/forced detach is used, no
inventory/pin assertion changes, and persistent holders or other errors fail.
Polling occurs only on busy mounts, and the runner logs its poll count. Two fast
real-kernel contracts hold an opened bpffs directory: one releases it and proves
normal unmount succeeds; the other retains it through a short deadline, verifies
`EBUSY` and deadline causes, proves the mount remains attached, then explicitly
releases it. Twenty repetitions passed both contracts. Repository Go lint passed.

Failure/contract evidence: `/tmp/bpfman-expanded-cases-final-rust-check.log`,
`/tmp/bpfman-expanded-cases-final-gate-timeline.jsonl`,
`/tmp/bpfman-bpffs-drain-contracts.log` and
`/tmp/bpfman-bpffs-drain-go-lint.log`.

The final timed `direnv exec . make rust-check lint-make` exited successfully
with bounded normal-unmount cleanup and the 32/four-slot defaults. Formatting,
Clippy, userspace/compile-fail and shell scheduling contracts, Rustdoc with
warnings denied and Makefile lint passed. All 96 discovered kernel tests ran
exactly once with none failed or ignored. Repository Go lint and explicit
e2e-tagged runner lint also passed. Required serial top-level runner tests retain
their scheduling, with narrow lint explanations; case-input imports are formatted.

| Complete gate component | First concurrent-stage gate, seconds | Expanded cases at 16 slots, seconds | Final 32-slot gate, seconds |
| --- | ---: | ---: | ---: |
| Both backend script batches | 116.83 | 114.52 | 95.26 |
| Retained kernel contracts | 96.87 | 96.43 | 97.97 |
| Complete kernel stage | 213.75 | 210.99 | 193.25 |

The final kernel stage took 3m13s: 20.50 s (10%) less than the first concurrent-stage
checkpoint, and 175.56 s (48%) less than the preceding committed 368.81 s (6m09s)
checkpoint. Retained contracts varied upward slightly; the improvement came from
script acceptance. These are local samples rather than a controlled benchmark
or a full build-time measurement.

Final stages were corpus 95.26 s, JSON global inventory 0.80 s, JSON independent
contracts 46.12 s, SQLite global inventory 0.87 s, SQLite independent contracts
49.72 s and legacy Go 0.46 s. Each store passed all 84 admitted files, all 64
named cases and all four concurrent shared-runtime scripts. The final timeline
contained 257 successful script processes with matching start/end markers:
124 isolated processes and four shared processes per backend, plus legacy Go.
Actual isolated/shared runner limits were 32/four, with observed maxima of
32/four active scripts. Process/start/end wall times were JSON 30.444/15.831 s
and SQLite 32.436/16.113 s; invocation totals were 30.518/15.904 s and
32.507/16.186 s respectively. Cleanup checks and normal unmount precede end
markers; the full suite outcome also confirms cleanup succeeded.

Per-test phase counts again matched the unchanged fault/survivor matrix:
832 forwarding phases, 160 broadcast phases and 40 worker records, all completed.
The retained exploratory duplicate-frame note remains; no duplicate report
occurred in the final gate. The busy-mount holder from the failed gate remains
unidentified; bounded normal release and persistent-holder rejection are the
verified contract, rather than a claim about that holder's origin.

The next obvious scheduling cost is the explicit shared-runtime cohort: it still
adds about 16 seconds per backend after isolated acceptance finishes. A future
runner change could overlap those cohorts in one backend invocation while
retaining separate runtime scopes, shared-store concurrency and cohort-wide
cleanup checks. That needs explicit scoped runner support; simply dropping the
shared cohort or running competing suite-lock holders would lose coverage.
Retained contracts now contribute about 98 seconds and should continue to use
phase evidence before changing concurrency or fixture boundaries.

Final evidence: `/tmp/bpfman-bpffs-drain-final-rust-check.log`,
`/tmp/bpfman-bpffs-drain-final-summary.json`,
`/tmp/bpfman-bpffs-drain-final-phase-matrix.json`,
`/tmp/bpfman-bpffs-drain-final-gate-timeline.jsonl` and
`/tmp/bpfman-bpffs-drain-runner-lint.log`.


### Overlapping script cohorts (10 October 2026)

The Rust corpus now uses one Go-runner invocation per backend. Two named cohorts
enter the existing parallel queue together: all admitted files/cases retain their
isolated runtimes, and four lifecycle scripts additionally share one owned
runtime. JSON and SQLite still run sequentially, and the system-wide suite lock
is unchanged. The shared cohort is capped at four executing scripts within the
same 32-slot runner; the Make `PARALLEL` override remains independent of retained
Rust kernel threads. No packet assertions or observation windows changed.

`BPFMAN_E2E_SHARED_SCRIPTS` names comma-separated script basenames for the
additional shared pass. It requires isolation and cannot bypass selector or
implementation admission. Invalid, duplicate, missing or unadmitted names fail
before execution; admitted exclusive scripts also prohibit overlapping cohorts.
Normal Go runs and existing test names remain unchanged when the setting is
absent. Mixed tests use `/isolated/scripts/` and `/shared/scripts/`, and Rust checks
both cohorts' expected PASS rows so a filter cannot silently omit acceptance.

Each isolated case still checks empty program/link/dispatcher inventories and
artifact collections before normal bpffs unmount. The shared cohort now owns its
runtime inside the Go runner and applies those same checks after all shared
scripts finish, then normally unmounts it. The existing bounded busy-reference
release remains; persistent holders still fail. Configuration and case parameters
never mutate the process environment while scripts run concurrently.

Telemetry separates script exit from cleanup: all script markers carry
`RuntimeScope`; `script_end` carries a test outcome; `runtime_cleanup` reports
verification/unmount success or failure independently of body exit; and
`cohort_end` follows all children and owned-runtime cleanup. A shared script's
successful end does not imply successful cohort cleanup. No captured output,
arguments, metadata or store contents are added to telemetry.

Temporary negative fixtures deliberately left an artifact after a successful
script exit. All three script processes exited zero, while both affected owned
runtimes reported failed cleanup, the isolated test failed, and the shared cohort
failed after its script had passed. Six admission checks rejected invalid,
duplicate, missing, unselected, non-isolated or exclusive configurations before
any cohort/script execution. A two-slot repeat check ran 16 processes: eight
isolated repetitions each used a distinct runtime, eight shared repetitions used
one common runtime, the scopes never shared paths, and all nine runtimes were
cleaned and removed. Fast runner admission, case input, cancellation, process
telemetry and real bpffs release contracts passed; e2e-tagged runner lint passed.

| Kernel stage component | Previous checkpoint, seconds | Overlapping cohorts, seconds |
| --- | ---: | ---: |
| Both backend script batches | 95.26 | 67.36 |
| Retained kernel contracts | 97.97 | 95.81 |
| Complete kernel stage | 193.25 | 163.20 |

The kernel stage fell by 30.05 s (16%), from 3m13s to 2m43s. Acceptance accounts
for 27.90 s of that improvement; retained contracts also varied slightly. These
are local samples, not a controlled benchmark or the full build/gate duration.
Shared scripts took longer individually while overlapping the other work:
JSON/SQLite shared process windows were 29.099/31.498 s, versus roughly 16 s in
the previous separate batches. The isolated windows were 31.704/35.392 s, and
the complete combined invocations took 31.778/35.470 s. Overlap eliminates the
serial tail despite this contention; it does not make each script faster.

All 96 discovered kernel tests ran exactly once, with none failed or ignored.
Each store passed all 84 admitted files, all 64 named cases and the four additional
shared scripts: 124 isolated plus four shared processes per store, and one legacy
Go process, for 257 total. Admission remains 52 shared and 32 Rust-only files;
labels and scenario matrices are unchanged. The timeline proved both cohorts
had scripts active simultaneously, total activity never exceeded 32 and shared
activity never exceeded four. All 250 owned-runtime cleanup outcomes and four
cohort end outcomes passed. The full per-test phase matrix matched the previous
checkpoint: 292 test/phase pairs and 1032 completed records, comprising 832
forwarding phases, 160 broadcast phases and 40 worker records.

Formatting, Clippy, userspace/compile-fail and shell scheduling contracts, kernel
acceptance and warnings-denied Rustdoc all passed through `rust-check`. The
combined command subsequently failed Make's undefined-variable probe because the
new optional setting lacked an empty Make default. Adding that default and
rerunning `rust-fmt lint-make` passed; no kernel behaviour changed. Go comparison
also passed all eight ordinary driver TX/REDIRECT and array/hash broadcast cases,
including isolated cleanup and final test outcomes.

Evidence: `/tmp/bpfman-overlap-rust-check.log`,
`/tmp/bpfman-overlap-gate-timeline.jsonl`, `/tmp/bpfman-overlap-summary.json`,
`/tmp/bpfman-overlap-phase-matrix.json`,
`/tmp/bpfman-overlap-runner-contracts.log`,
`/tmp/bpfman-overlap-runner-lint-final.log`,
`/tmp/bpfman-overlap-make-final.log`,
`/tmp/bpfman-overlap-negative-check.log`,
`/tmp/bpfman-overlap-negative-summary.json`,
`/tmp/bpfman-overlap-go-final.log` and
`/tmp/bpfman-overlap-go-final-timeline.jsonl`.

Retained Rust contracts now account for about 96 seconds of the 163-second kernel
stage. Further reductions should start with their per-test and phase evidence,
while preserving fault matrices, ownership proofs and packet observation windows.

### Eight-worker retained contracts (11 October 2026)

The existing `kernel-timing` records already covered every kernel test. New opt-in
`kernel-test-start` and `kernel-test-end` records make their overlap measurable:
process ID, test name and monotonic seconds from that process's clock origin,
plus the outcome on end records. Pair by process ID and test name; each backend
invocation has its own origin. End records include body and teardown, and the
old elapsed/phase records remain available unchanged. No command arguments,
runtime paths or store contents enter these new records. Missing end records or
a failed runner exit still invalidate a run.

The concurrency audit retained these boundaries:

- Runtime roots, mounts, stores, lock files and fault state belong to each fixture.
- The atomic fixture allocator supplies unique namespace and host-veth names;
  fixed names such as `in0` are confined to each private network namespace.
- Namespace entry runs on fresh disposable adapter threads and never changes
  another worker's namespace or process-wide environment.
- Redirect-error observation filters by the fixture's map ID; program, map and
  link reclamation checks target owned IDs. TC filters and clsact evidence remain
  confined to their namespace and exact attachment identities.
- The two XDP load tests compare global dispatcher inventories and remain in
  separate serial stages. Corpus, legacy Go and backend boundaries also remain
  sequential. Script concurrency is unchanged.

Independent retained tests now default to eight outer workers. Forwarding keeps
one inner fixture worker under concurrent outer execution, so nesting cannot
turn eight into sixteen. Focused serial targets still use two fixture workers,
joining all workers before propagating a panic. `BPFMAN_KERNEL_TEST_THREADS`
accepts 1, 2, 4 or 8; fast scheduler contracts verify every setting, the default,
serial global stages, backend order and rejection before effects. The local
measurement host exposes 16 CPUs; lower limits remain available for other hosts.

The four-worker repeat kept all four slots occupied for 47.762 of the JSON
stage's 49.12 seconds and 50.183 of SQLite's 52.79 seconds. Unused capacity
between the first test start and last end was only 2.194/4.633 slot-seconds.
Reordering the same work at four workers therefore has little headroom. Packet
observation remains the largest forwarding phase group, while setup includes
fresh program loads for distinct action, continuation, map and fragment cases.
Existing fixtures already reuse networks/programs across both survivor choices;
this slice preserves that structure and changes scheduling without sharing new
ownership across independent cases. All restoration assertions, fault matrices,
fragment reads, redirect-error evidence and 300 ms observation windows remain.

Validation used the normal Make targets, sequentially, with the runner file
unchanged while each shell process executed it:

```sh
direnv exec . env BPFMAN_KERNEL_TIMINGS=1 BPFMAN_KERNEL_TEST_THREADS=4 \
  make rust-test-kernel-load > /tmp/bpfman-contract-workers-4-repeat.log 2>&1
direnv exec . env BPFMAN_KERNEL_TIMINGS=1 \
  BPFMAN_E2E_SCRIPT_TIMELINE=/tmp/bpfman-contract-workers-gate-timeline.jsonl \
  make rust-check lint-make > /tmp/bpfman-contract-workers-gate.log 2>&1
```

| Stage, seconds | First four-worker sample | Repeated four-worker sample | Eight-worker full gate |
| --- | ---: | ---: | ---: |
| JSON independent contracts | 62.31 | 49.12 | 32.37 |
| SQLite independent contracts | 52.91 | 52.79 | 34.15 |
| Both serial global checks | 2.13 | 1.96 | 1.88 |
| Legacy Go contract | 0.58 | 0.58 | 0.56 |
| Retained stage subtotal | 117.93 | 104.45 | 68.96 |
| Script acceptance | — | — | 81.40 |
| Complete kernel stage | — | — | 150.41 |

Both contract-only four-worker runs exited zero and ran all 94 retained tests;
their wrapper totals were 117.98 and 104.50 seconds. The repeated baseline
avoids treating the first sample's slower JSON execution as an eight-worker
speedup. Against the repeated subtotal, retained work fell by 35.49 seconds
(34%). These are local samples, not a controlled benchmark. Script time also
varied: it was 67.36 seconds at the prior checkpoint and 81.40 here, despite
unchanged scheduling. The overall kernel stage improved from 163.20 to 150.41
seconds; this does not measure builds, userspace tests or total gate duration.

An initial eight-worker measurement passed all 94 retained tests but its shell
wrapper exited 127 before final counters: editing the runner's default while it
was still executing changed the file beneath the shell's read offset. That run
is not a successful gate. The subsequent full gate used a stable runner, exited
zero and supersedes it. No kernel failure or weakened assertion was involved.

The successful full gate ran all 96 discovered tests exactly once, with no
failures or ignored tests. Start/end records paired for every test, agreed with
the existing elapsed records and proved peak outer activity of eight for each
independent backend stage and one for serial stages. Every per-test phase count
matched both four-worker baselines and the prior checkpoint: 292 test/phase
pairs, 832 forwarding phases, 160 broadcast phases and 40 fixture-worker records.
Both stores passed all 84 admitted script files, all 64 named cases and the four
additional shared scripts. All 257 script processes, 250 owned-runtime cleanup
outcomes and four cohort completions passed; script scope overlap and its 32/four
limits remain intact. Formatting, Clippy, userspace/compile-fail tests, scheduler
contracts, real-kernel acceptance, warnings-denied Rustdoc and Make lint passed.

Eight workers expose a new scheduling tail: JSON/SQLite had 48.946/46.949 unused
slot-seconds between their first start and last end, compared with only
2.194/4.633 at four workers. Jumbo forwarding tests remain the longest, at
16.16–17.85 seconds. A next slice can split their already independent delivery
and DEVMAP groups into separately scheduled contracts, preserving each fixture
and fault matrix while letting idle workers take smaller jobs. Idle slot-seconds
are overlapping capacity, not wall time or a guaranteed saving.

Evidence: `/tmp/bpfman-contract-workers-4.log`,
`/tmp/bpfman-contract-workers-4-repeat.log`,
`/tmp/bpfman-contract-workers-gate.log`,
`/tmp/bpfman-contract-workers-gate-timeline.jsonl`,
`/tmp/bpfman-contract-workers-comparison.txt`,
`/tmp/bpfman-contract-workers-gate.log.summary.json`,
`/tmp/bpfman-contract-workers-acceptance-summary.json` and
`/tmp/bpfman-contract-workers-acceptance-phase-matrix.json`.

### Separately scheduled jumbo forwarding contracts (11 October 2026)

Four combined multi-buffer tests now become eight independently scheduled
contracts: TX/direct REDIRECT delivery and array DEVMAP, each on JSON/SQLite
and driver/SKB ingress. Their helpers already construct independent networks,
runtimes, programs and fault state; the split changes only their test entry
points and timing names. All names retain `xdp_multibuffer_forwarding`, so the
existing focused Make target selects every split contract. The complete runner
still uses eight outer workers, one inner fixture worker, serial inventory
checks and sequential backend stages. No scheduler or production API changes
were needed.

The fresh unchanged baseline exposed a capture isolation bug before reaching
SQLite. `json_xdp_unload_lifecycle` expected PASS counts `[0, 3]` but received
`[2, 3]`: all three frames arrived at the receiver, while the sender socket also
counted two frames. The shared Go packet helper created AF_PACKET sockets with
an active protocol before binding their interface. That gap permits other
fixtures' frames in the same network namespace to queue; binding afterward
does not discard them. Unique interface names therefore did not provide full
capture isolation.

A deterministic probe in a disposable network namespace sent a foreign frame
between socket creation and bind. The old socket retained that frame after bind;
protocol zero at creation received only the later frame on its selected interface.
The helper now enables reception in the interface-specific bind. No frame is
discarded to mask this failure, and assertions, exact payload/sequence checks,
300 ms full-capture windows and the PASS stimulus deadline remain unchanged.
The rule is recorded in `rust/AGENTS.md`. The failed initial baseline is not
used as passing evidence; both timing comparison runs use the corrected helper.

Validation used the existing Make targets:

```sh
direnv exec . env BPFMAN_KERNEL_TIMINGS=1 make rust-test-kernel-load \
  > /tmp/bpfman-jumbo-split-baseline-fixed.log 2>&1
# After splitting the four combined test entry points:
direnv exec . env BPFMAN_KERNEL_TIMINGS=1 \
  BPFMAN_E2E_SCRIPT_TIMELINE=/tmp/bpfman-jumbo-split-gate-timeline.jsonl \
  make rust-check lint-make > /tmp/bpfman-jumbo-split-gate.log 2>&1
direnv exec . env BPFMAN_KERNEL_TIMINGS=1 make rust-test-kernel-load \
  > /tmp/bpfman-jumbo-split-contracts.log 2>&1
```

| Stage, seconds | Combined, contract-only baseline | Split, full gate | Split, contract-only repeat |
| --- | ---: | ---: | ---: |
| JSON independent contracts | 28.51 | 33.98 | 28.09 |
| SQLite independent contracts | 29.60 | 31.62 | 30.16 |
| Both serial global checks | 1.70 | 2.68 | 1.70 |
| Legacy Go contract | 0.49 | 0.48 | 0.48 |
| Retained stage subtotal | 60.30 | 68.76 | 60.43 |
| Script acceptance | — | 70.74 | — |
| Complete kernel stage | — | 139.55 | — |

All three runs exited zero. Contract-only wrapper totals were 60.32/60.48 s;
the baseline selected 94 of 96 discovered tests, and the split selected 98 of
100. The full gate selected all 100 exactly once, with none failed or ignored.
Start/end records pair for every test and prove peak outer concurrency of eight
in independent stages and one in serial stages. Mapping split names back to
their former combined parents preserves every per-test phase count: 292
normalized test/phase pairs and 1032 records, including 832 forwarding, 160
broadcast and 40 fixture-worker phases. No fault cases, continuation masks,
survivor choices, fragment reads, ownership proofs or capture windows were removed.

Idle capacity between first start and last end fell from 46.351/43.934 to
28.297/29.873 JSON/SQLite slot-seconds in the matching contract-only samples.
These overlap across workers and are not elapsed-time savings. Retained wall
time is effectively unchanged; the full gate's improvement from 150.41 to
139.55 s mostly reflects script acceptance varying from 81.40 to 70.74 s.
The split must not be credited with that corpus-time change. Longer per-test
durations after script acceptance also justify the contract-only repeat rather
than comparing its 68.76 s directly to the earlier 60.30 s baseline. These are
local samples, not a controlled benchmark.

Both stores passed all 84 admitted files, 64 named cases and four additional
shared scripts. All 257 script processes, 250 owned-runtime cleanup outcomes
and four cohort completions passed, with the 32/four script limits and scope
overlap preserved. Formatting, Clippy, userspace/compile-fail tests, scheduler
contracts, real-kernel acceptance, warnings-denied Rustdoc and Make lint passed.
Go lint reported zero issues. The existing file lifecycle, XDP lifecycle,
fill/drain/refill and TC ingress lifecycle scripts also passed against the Go
binary with isolated SQLite runtimes and the corrected shared helper.

The longest matching-run tests now contain ordinary or jumbo TX/direct REDIRECT
delivery, at 10.36–11.17 s. Each still bundles four independent action/mask
fixtures. A next slice can separately schedule TX and direct REDIRECT for both
frame shapes, preserving fixture reuse within each survivor pair, then measure
whether that reduces the remaining tail. More useful scheduling granularity
does not itself guarantee lower elapsed time.

Evidence: `/tmp/bpfman-jumbo-split-baseline.log` (failed original baseline),
`/tmp/bpfman-packet-bind-regression.py`,
`/tmp/bpfman-jumbo-split-baseline-fixed.log`,
`/tmp/bpfman-jumbo-split-gate.log`,
`/tmp/bpfman-jumbo-split-contracts.log`, their `.summary.json` reports,
`/tmp/bpfman-jumbo-split-comparison.json`,
`/tmp/bpfman-jumbo-split-gate-timeline.jsonl`,
`/tmp/bpfman-jumbo-split-acceptance-summary.json`,
`/tmp/bpfman-jumbo-split-acceptance-phase-matrix.json`,
`/tmp/bpfman-jumbo-split-lint-go.log` and
`/tmp/bpfman-jumbo-split-go-shared.log`.

### CLI cost under parallel acceptance (11 October 2026)

A fresh measurement of the committed code passed `make rust-test`: all 100
kernel tests executed, with 64.06 s in script acceptance and 59.47 s in retained
contracts, for 123.56 s total. Userspace targets reported another 40.56 s of test
execution, including 22.15 s in the fake-kernel lifecycle integration target;
these sums exclude compilation and prerequisite setup. The difference from the
earlier 139.55 s kernel sample is run-to-run variation, not a code improvement.
All 257 script processes, 250 owned-runtime cleanups and four cohort completions
passed. Whole-test timing pairs were intact; one DEVMAP setup phase interleaved
with libtest stdout, so this sample does not prove a completely parsed phase
matrix. The eight delivery contracts' phase records were intact: packet
observation accounted for 38.6% and setup for 30.0% of their summed sequential
phase durations. These durations overlap across tests and are not elapsed time.

Additional measurements used temporary executable wrappers named `bpfman` and
the CLI's existing `--trace-file`, with
`RUST_LOG=debug,bpfman_lock=trace,bpfman_fs=trace`. Only XDP/TC fill/drain/refill
commands were profiled. Each store ran the two scripts alone with one runner
slot, then the normal 32-slot corpus with its four-slot shared cohort. The Rust
binary, scripts, kernel and cleanup assertions stayed unchanged. Instrumented
standalone scripts measured about 15 s versus the earlier approximately 14 s
samples; this includes profiling overhead and run variation. The following
comparisons use equally instrumented runs.

| Mean CLI command duration, ms | JSON alone | JSON corpus, isolated | SQLite alone | SQLite corpus, isolated |
| --- | ---: | ---: | ---: | ---: |
| Program load, XDP script | 140.9 | 273.7 | 141.7 | 312.2 |
| XDP attach | 80.2 | 172.8 | 79.8 | 220.1 |
| Link detach, XDP script | 80.0 | 193.0 | 78.5 | 177.6 |
| Program unload, XDP script | 21.7 | 26.4 | 20.0 | 25.4 |

Each script performs 31 loads, 31 attaches, 31 detaches and 31 unloads. TC shows
the same pattern: standalone load/attach/detach means were approximately
142–145/77–78/75–77 ms, increasing to 263–318/194–227/129–175 ms in the corpus.
The profiled isolated scripts took 15.1–15.2 s alone and 26.1–31.1 s in the
corpus. Isolated writer-lock waits summed to only 10.5–25.3 ms per corpus script;
the shared XDP script instead waited 4.34 s on JSON and 5.41 s on SQLite. JSON
snapshot publication summed to roughly 0.3–0.6 s per command category in the
corpus. Store publication and lock acquisition therefore do not explain most
of the isolated command cost. Successful execution and cleanup markers were
validated for both corpora and all focused runs; 1334 command timelines parsed
with balanced span starts and ends.

An additional isolated XDP script sampled the first load, attach and detach
using `perf record -e cycles:u -F 999 --call-graph dwarf`. The script and its
cleanup passed. The three profiles contained 137/89/82 samples, with none lost.
Inclusive sampled cycles in `EbpfLoader::new` / kernel BTF loading were
83.66%/74.56%/72.45%. These are small diagnostic CPU samples, not wall-time
percentages or a measured optimization. They identify repeated kernel BTF
reading/parsing as the principal userspace hotspot in the unoptimized build.
Aya 0.14's constructor eagerly calls `Btf::from_sys_fs()`; an ordinary program
load constructs loaders for both the user object and its verification target,
while dispatcher replacement constructs another loader. Calling `.btf(None)`
or supplying cached BTF after `new()` does not avoid that constructor work.

The next bounded experiment should optimize compilation of the `aya-obj`
dependency in the development/test profile, then compare matched script and
retained-contract timings and run the full gate. This can target the measured
parser cost without modifying Aya source, changing program semantics or
weakening acceptance. Any proposal to reduce loader constructions must separately
preserve CO-RE, verifier targets, fragment flags and object ownership. More test
splitting has limited scheduling headroom and should follow measured reductions
in per-command CPU cost.

Evidence: `/tmp/bpfman-measure-current-report.txt`,
`/tmp/bpfman-measure-current.log`,
`/tmp/bpfman-measure-current-focused-comparison.json`, and
`/tmp/bpfman-cli-profile/` containing the four runner logs/timelines,
`analysis.json`, `summary.txt`, per-command Chrome traces and the three CPU
profiles with compact reports. Temporary wrappers and profiling files are not
repository dependencies; no production or test implementation changed.

### Optimized BTF parser and loader-use audit (11 October 2026)

The next measurement compiles only `aya-obj` at optimization level two using
`[profile.dev.package.aya-obj]`; test builds inherit this setting. Workspace code
keeps its normal debug profile, and dependency debug symbols remain available.
The build log confirms `rustc --crate-name aya_obj -C opt-level=2`; its overall
`unoptimized` summary still describes the workspace profile. The initial build
took 10.34 s. No Aya source, dependency version, application operation, fixture,
assertion, capture window or concurrency limit changed. The override is retained
after the following successful measurements and full gate; optimized dependency
locals can be harder to inspect in a debugger.

The previous CLI-profiling binary was archived before rebuilding. Matching
temporary wrappers captured the same two standalone scripts per store and the
same normal mixed corpus per store. All 1334 per-command timelines parsed with
balanced spans, and every per-script command-category count matched the baseline.

| Profiled script, seconds | Before | Optimized `aya-obj` |
| --- | ---: | ---: |
| JSON XDP, alone | 15.247 | 10.290 |
| JSON TC, alone | 15.087 | 10.175 |
| SQLite XDP, alone | 15.186 | 10.372 |
| SQLite TC, alone | 15.240 | 10.212 |
| JSON XDP, isolated in corpus | 27.856 | 19.065 |
| JSON TC, isolated in corpus | 26.083 | 19.849 |
| SQLite XDP, isolated in corpus | 30.746 | 18.176 |
| SQLite TC, isolated in corpus | 31.140 | 19.809 |

Standalone XDP load means fell from 140.9/141.7 to 57.6/59.1 ms on JSON/SQLite;
attach means fell from 80.2/79.8 to 41.7/43.2 ms. The shared XDP scripts also
improved, from 30.968/33.007 to 21.753/21.634 s. These are local samples with
profiling overhead and scheduling variation, not guaranteed production speedups.
All four runner invocations passed, including 256 corpus processes, 250 corpus
owned-runtime cleanups, four cohort completions and the four focused processes
and cleanups. Both corpora preserved peak concurrency of 32 globally and four
in the shared scope, with scope overlap.

The complete `direnv exec . env BPFMAN_KERNEL_TIMINGS=1
BPFMAN_E2E_SCRIPT_TIMELINE=/tmp/bpfman-aya-build-measure/gate-timeline.jsonl
make rust-check lint-make` gate then exited zero:

| Kernel stage, seconds | Earlier unchanged-code measurement | Optimized parser gate |
| --- | ---: | ---: |
| Script acceptance, both stores | 64.06 | 54.78 |
| Retained contracts, including serial stages | 59.47 | 46.61 |
| Complete kernel stage | 123.56 | 101.45 |

The observed complete-kernel reduction is 17.9%, with 21.6% in retained contracts.
All 100 tests ran exactly once; all 1032 retained phase counts match the prior
complete baseline, including every fault/lifetime combination. Both stores
passed their 84 admitted files, 64 named cases and four extra shared scripts;
all 257 process exits, 250 owned-runtime cleanups and four cohort outcomes passed.
Formatting, Clippy, userspace/compile-fail and scheduler contracts, Rustdoc and
Make lint passed. Raw gate stdout/stderr were saved separately to avoid libtest
stdout interleaving with timing records; the analysis log groups unchanged timing
records by the runner's existing stages and retains the raw evidence.

The loader-use audit found ordinary public-API use rather than an accidental
reload of members during replacement. Aya's `EbpfLoader::new()` eagerly loads
kernel BTF even before the caller can select `.btf(None)` or supply cached BTF.
Its mutable `.load()` supports reuse, but global overrides and extension names
remain in the builder and have no public reset. Blindly sharing one builder
between user objects and differently configured verification dispatchers would
therefore risk changing load semantics. Go also uses a separate verification
dispatcher for XDP/TC, then reuses pinned members during replacement.

An isolated syscall probe of the first XDP load, attach and detach observed
two/one/one opens of `/sys/kernel/btf/vmlinux` in Rust. The same existing XDP
script passed against Go on SQLite, including normal cleanup, and its matching
commands opened kernel BTF zero times. The vendored cilium/ebpf relocation path
returns before loading kernel types when there are no CO-RE relocations, and its
kernel BTF reader caches decoded types within the process when needed. This
explains a real efficiency difference in the library defaults. It does not
justify dropping required verification targets or disabling CO-RE. An upstream
lazy or explicitly supplied BTF constructor remains a useful future option;
calling `.btf(None)` after Aya's current constructor would not fix the measured
work. Further experiments can profile the remaining optimized-build cost before
changing loader ownership or scheduling.

Evidence: `/tmp/bpfman-aya-build-measure/` contains `optimized-build.log`, the
archived `baseline/bpfman`, four profiled runner logs/timelines, `analysis.json`,
`comparison.json`, the raw `gate.stdout.log` / `gate.stderr.log`, derived
`gate.log` / `gate.log.summary.json`, `gate-acceptance-summary.json` and the
Rust/Go syscall diagnostic logs. Wrappers and analysis scripts remain temporary
measurement tooling, not repository dependencies.


### Further dependency profiling and fake-kernel storage costs (11 October 2026)

CPU sampling was repeated with the retained `aya-obj` level-two override. The
same isolated XDP fill/drain/refill script and normal runtime cleanup passed.
The first load/attach/detach profiles contained 57/44/45 samples with none lost.
Inclusive sampled cycles in `EbpfLoader::new` / kernel BTF parsing were
46.37%/29.97%/28.11%, compared with 83.66%/74.56%/72.45% before parser
optimization. Remaining `EbpfLoader::load` work accounted for
10.56%/3.54%/7.66%. These small samples include diagnostic tracing overhead;
ancestral stack percentages overlap and must not be added or read as wall time.

A reversible experiment also compiled `aya` at optimization level two. Both
binaries were archived before comparison, and the build log confirmed the Aya
compiler override. Eight sequential runner invocations exercised the unchanged
XDP and TC fill/drain/refill scripts: two samples per binary per store, with
baseline/candidate/candidate/baseline ordering on JSON and the reverse on
SQLite. These timing controls did not enable CLI tracing or CPU sampling.

| Mean script process time, seconds | `aya-obj` optimized | Both Aya crates optimized |
| --- | ---: | ---: |
| JSON XDP | 9.347 | 9.257 |
| JSON TC | 9.060 | 9.014 |
| SQLite XDP | 9.621 | 9.590 |
| SQLite TC | 9.521 | 9.443 |

All 16 script processes and owned-runtime cleanups passed. The observed
0.33–0.96% differences do not justify another dependency override; the additional
Aya setting was removed. The committed `aya-obj` override remains, with workspace
code in its ordinary debug profile. No loader construction, verifier target,
CO-RE behavior, assertion, fixture or concurrency limit changed.

The 60-test fake-kernel lifecycle suite was then timed using a temporary stdout
observer with one libtest worker, without changing test bodies. All tests passed
in 45.30 s. Its two XDP replacement failure matrices dominated: 8.55 s on JSON
and 15.16 s on SQLite. These are serial diagnostic timings, not the ordinary
parallel suite duration. Each matrix preserves all 88 combinations of attach or
detach, forward failure, failure phase and cleanup failure, including explicit
retry and residue assertions. The fake kernel still uses the real filesystem
and the two production store backends.

Separate syscall-summary runs of those two matrices passed on the normal
Btrfs-backed `/tmp`. JSON spent 7.815 s in 1224 `fsync` calls during a 9.45 s
instrumented test; SQLite spent 13.204 s in 2888 calls during a 16.02 s test.
The SQLite matrix therefore performs about 2.36 times as many synchronization
calls, although its mean call latency was lower. This identifies a backend
storage cost; the aggregate probe does not assign each call to bytecode,
snapshot publication or SQLite WAL/checkpoint work. `strace -f -w -c` also
counts the harness thread's `futex` join wait, which overlaps the worker and
must not be added to its elapsed time.

The complete, unchanged 60-test suite was then run with normal libtest
parallelism through `make rust-test-kernel-fake` on each filesystem:

| Fixture filesystem | Tests passed | Suite seconds |
| --- | ---: | ---: |
| Btrfs (`/tmp`) | 60 | 21.97 |
| tmpfs (owned temporary directory under `/dev/shm`) | 60 | 2.22 |

Only the second invocation's `TMPDIR` changed. Both production stores, actual
files, reopening, locks, publication, cleanup and every failure combination
remained active. The temporary tmpfs parent was empty after the tests and
removed normally. Matching syscall probes on tmpfs retained exactly
1224/2888 `fsync` calls, taking 0.0036/0.0112 s; both matrices passed. This is
an observed 89.9% reduction for this fake-kernel suite, not a reduction in the
real-kernel stage or proof of physical storage durability.

The next bounded implementation should give this fake-kernel suite an explicit,
owned memory-backed fixture scope with a portable disk-backed fallback. Keep
actual synchronization operations enabled and retain separate real-filesystem
persistence/durability contracts. Scope any temporary-directory selection to
this target rather than changing every workspace test or real-kernel fixture.
No persistent test-runner change was made during these measurements.

Evidence: `/tmp/bpfman-aya-next-measure/` contains archived baseline and candidate
binaries, the optimized-parser CPU samples/reports, `candidate-build.log`, eight
script logs/timelines and `benchmark-summary.json`, `fake-serial-timing.json`,
`fake-experiments.json`, and Btrfs/tmpfs syscall summaries for both matrices.
Temporary profiling wrappers, the stdout observer and the supplementary Make
file are measurement tooling only.

The normal `direnv exec . env BPFMAN_KERNEL_TIMINGS=1
BPFMAN_E2E_SCRIPT_TIMELINE=/tmp/bpfman-aya-next-measure/gate-timeline.jsonl
make rust-check lint-make` gate subsequently exited zero, with disk-backed fake
fixtures. Formatting, Clippy, all userspace/compile-fail and scheduler contracts,
real-kernel acceptance, Rustdoc and Make lint passed. The normal fake suite took
22.05 s. The complete kernel stage took 100.21 s
(52.92 s scripts and 47.23 s retained stages).
All 100 kernel tests ran exactly once, and all 1032 retained phase counts and
test identities matched the prior complete gate. Both stores passed all 84
admitted files, 64 named cases and four additional shared scripts; all 257
process exits, 250 owned-runtime cleanups and four cohort outcomes passed.
The temporary additional Aya override is absent from the final workspace.
Raw gate stdout/stderr and the corresponding coverage reports are retained
in the same measurement directory. This slice changes only this document.


### Owned tmpfs fixtures for the fake kernel (11 October 2026)

The existing Cargo test runner now selects `test-kernel-fake.sh` only for the
`kernel_lifecycle` binary. Both the focused `rust-test-kernel-fake` target and
the automatic userspace gate supply this runner; automatic target discovery
still includes every nonprivileged integration target. Other targets preserve
their incoming `TMPDIR`, and the privileged kernel runner retains its existing
namespace and concurrency behavior.

The fake runner verifies that `/dev/shm` is tmpfs and creates a private owned
suite directory there when possible. Missing tmpfs or failed directory creation
falls back to an owned directory under the normal `TMPDIR` (or `/tmp`).
`BPFMAN_FAKE_KERNEL_TMPFS=0` explicitly selects that fallback. Only the test
child receives the selected `TMPDIR`. Each fixture still creates its own
subdirectory and uses the same production stores, real files, locks and
synchronization calls. No Rust assertion, failure matrix or fixture body changed;
no dependency, privilege or asynchronous runtime was added.

After the child exits, the runner requires the owned suite directory to be empty
and removes it with `rmdir`. Residue fails cleanup and remains available for
inspection; successful test exit cannot hide failed cleanup. The runner reports
fixture selection and cleanup outcome, preserves a failed child's status, and
forwards termination before waiting for the child and cleaning its scope.
Fast shell contracts cover selection/fallback, arguments, exit status, signal
handling, residue refusal and unchanged temporary-directory scope for unrelated
targets. They run in the normal userspace gate without sudo or kernel state.

The focused suite passed all 60 tests in 2.47 s on tmpfs, including empty-scope
cleanup. Its forced default-filesystem run with `TMPDIR=/tmp` passed the same
60 tests and cleanup in 21.87 s. This retains the measured saving through the
normal Make targets while preserving an explicit disk-backed diagnostic run.
Shell syntax, ShellCheck and the fast runner contracts passed.

The full `direnv exec . env BPFMAN_KERNEL_TIMINGS=1
BPFMAN_E2E_SCRIPT_TIMELINE=/tmp/bpfman-fake-tmpfs-gate/gate-timeline.jsonl
make rust-check lint-make` gate exited zero. The fake-kernel target passed all
60 cases in 2.40 s with a verified tmpfs scope and successful removal. All
58 reported userspace test groups retained their previous counts (367 cases);
the sum of their reported target durations fell from 40.99 to 21.13 s compared
with the preceding complete gate. That sum excludes build and prerequisite
setup, rather than measuring the entire Make invocation.

Formatting, Clippy, userspace/compile-fail and both fast runner contract suites,
real-kernel acceptance, Rustdoc and Make lint passed. The real-kernel stage took
105.53 s (58.77 s scripts and
46.72 s retained stages); this fixture change does not target
that stage. All 100 kernel tests ran exactly once, with the same 1032 phase
counts and test identities as the preceding complete gate. Both stores passed
all 84 admitted files, 64 named cases and four additional shared scripts;
all 257 process exits, 250 owned-runtime cleanups and four cohort outcomes
passed. Evidence includes `/tmp/bpfman-fake-tmpfs-focused.log`,
`/tmp/bpfman-fake-disk-focused.log`, and `/tmp/bpfman-fake-tmpfs-gate/` with
separate raw gate stdout/stderr, the script timeline, coverage summaries and
`userspace-comparison.json`.


### Real-kernel storage and SQLite fixture measurements (11 October 2026)

Temporary supplementary Make targets and a measurement-only Cargo runner compared
owned Btrfs (`/tmp`) and tmpfs (`/dev/shm`) fixture scopes. Only the test child's
`TMPDIR` changed. Production test bodies, file-backed stores, synchronization,
bpffs preparation/mounts, fault cases, packet windows and concurrency remained
unchanged. The retained kernel suite kept eight workers and sequential backend
stages; the committed `aya-obj` optimization remained active. No persistent runner
or fixture change was made in this measurement slice.

The nine SQLite read contracts and five full-record contracts passed in all
four ordinary disk/tmpfs/tmpfs/disk runs:

| SQLite target | Disk mean seconds | Tmpfs mean seconds |
| --- | ---: | ---: |
| `read` (9 tests) | 3.035 | 0.020 |
| `records` (5 tests) | 6.450 | 0.030 |

These are libtest target durations, excluding Cargo/build time. Supplementary
serial observations passed the same 14 tests on each filesystem. The slowest
disk cases were `malformed_full_records_fail_instead_of_being_omitted` (5.848 s),
`rejects_malformed_stored_values` (2.201 s), and
`refuses_old_and_new_schemas_without_migrating` (1.295 s). The malformed
full-record case constructs eight fresh databases rather than timing one read.

Serial syscall-summary runs also passed all cases. `read` performed exactly
1015 `fsync` calls on both filesystems, taking 6.654 s on disk and 0.0032 s on
tmpfs. `records` performed exactly 1308 calls, taking 8.655 s and 0.0044 s.
These are instrumented, serial runs, not the parallel target means above.
Harness `futex` waits overlap test execution and must not be added to wall time.

To separate fixture construction from reads, a throwaway Rust probe linked the
same Cargo-built SQLite adapter dependencies and called the unchanged
`rust/tests/support/mod.rs` fixture. Three fresh databases per invocation used
the real schema, seeding and public `Store` reads. Uninstrumented disk means
were 622 ms for `database()`, 81.8 ms for `seed()`, 0.42 ms for `Store::open`,
0.18 ms for `read_records`, and 0.086 ms for `read_programs`. On tmpfs,
construction and seeding took 2.41 ms and 0.17 ms. Timestamped sync-only probes
assigned every successful sync to a measured phase: construction made 92
`fsync` calls per database, seeding made 12, and both read phases made none.
The fixture executes schema statements and seeds in autocommit mode; fixture
initialization, rather than row decoding or cached reads, dominates this cost.

A separate throwaway copy of that fixture wrapped only schema/version setup in
one transaction. The SQL and physical database files remained the same, and
the probe's seeded public reads still passed. Its uninstrumented disk
construction mean fell to 27.9 ms, with four construction syncs instead of 92.
Seeding remained unchanged at about 81 ms and 12 syncs. This is a measured
fixture experiment, not a change to the checked-in helper or the production
creation path; production creation already uses a transaction. Batching fixture
setup is therefore a useful next slice even for tests that must retain files.

The complete retained real-kernel suite passed all 98 selected tests on disk
in 46.65 s and 46.11 s. One tmpfs run passed the same 98 tests in 40.56 s.
All three passing runs retained identical test identities and all 1032 phase
counts, with no failed phase outcomes. The roughly 5.8-second difference from
the disk mean is indicative only: the tmpfs configuration was not reliable.
An initial tmpfs run failed `json_xdp_unload_lifecycle`; a subsequent run failed
`sqlite_xdp_unload_lifecycle`. An isolated serial tmpfs run failed both, while
its disk comparison passed both. Each failure was the immediate program-ID
absence assertion at `kernel/xdp_unload.rs:111`, after successful unload and
managed-file removal. The first program ID was absent when inspected later,
after fixture cleanup. This is consistent with a deferred kernel release
timing assumption, but those observations do not prove when the final reference
disappeared or whether cleanup accelerated it. The existing extension-link
lifetime helper already uses bounded observation polling. Do not broaden the
committed tmpfs runner to real-kernel tests until the program-release contract
has been investigated and made independent of disk latency.

Serial syscall probes of XDP switch/restoration and TC attached unload on both
stores passed all four tests on each filesystem: 18.10 s on disk and 16.20 s
on tmpfs. Both made 278 sync calls; their aggregate sync time fell from 1.742 s
to 0.00092 s. In the complete passing suites, overlapping phase sums also showed
lower forwarding setup and managed teardown costs while packet observation
remained similar. Phase sums are concurrent/nested work, not a wall-time
partition, and must not be presented as additional elapsed-time savings.

All 34 owned measurement scopes, including those belonging to failed test runs
and an initial measurement-wrapper path error, passed empty-directory cleanup
and were removed. Failed experiment logs remain part of the evidence. The
checked-in runner continues to scope tmpfs only to the fake-kernel suite.

#### Which SQLite tests can use `:memory:`?

Plain `:memory:` is connection-local. The public file-backed store opens its
own reader and writer connections, so changing its pathname to `:memory:` is
not a valid fixture substitution. SQL-only assertions can instead exercise
private connection-based functions from adapter unit modules. No public
in-memory store mode or wider visibility is needed for that approach.

| Existing coverage | Appropriate next form | Coverage to preserve |
| --- | --- | --- |
| `queries::tests::cached_statements_rebind_after_errors_and_keep_record_fields_distinct` | In-memory connection with the actual Go schema | Binding, statement reuse after errors, joins, constraints and affected-row counts; file creation is covered separately. |
| `records::timestamps_match_go_rfc3339nano_without_losing_precision` (currently an integration test) | Pure unit assertions on the private timestamp decoder | Exact fractional precision and timezone normalization; no database is necessary. |
| `malformed_full_records_fail_instead_of_being_omitted` | In-memory adapter unit matrix using the private full-record read function | Real SQL projection plus decoder failures and missing-map-set rejection; retain file-backed opening/read-only contracts. |
| `rejects_malformed_stored_values` | In-memory adapter unit matrix using the private summary read function | Invalid kinds, IDs and metadata plus existing error translation; retain file-backed opening contracts. |
| `reader::tests::sequential_reads_reuse_the_initial_connection_including_after_an_error` | Already uses an in-memory connection | Connection reuse and failed-transaction release without reopening the absent pathname. |

Retain files for the unchanged-database-byte assertions, creation/publication,
missing/corrupt/symlink paths, schema mutation through a second connection,
committed WAL visibility, overlapping readers that open additional connections,
CLI/runtime reopening, and runtime/device/inode-bound cleanup receipts. The
cached full-record integration test also needs independent writer/reader
connections to establish visibility of committed changes. Public link, XDP,
TC and unload publication/receipt contracts must continue using the concrete
file-backed adapter even when individual SQL invariants acquire fast unit tests.
Named shared-memory databases would change the connection and WAL conditions
of these contracts and are not a substitute for their current evidence.

Evidence is in `/tmp/bpfman-storage-followup/`: supplementary Make targets and
runner, raw separate stdout/stderr logs (including failures),
`measurements.json`, serial per-test observations, syscall summaries,
`fixture-probes.json`, timestamped phase/sync traces, and `report.json`.
The phase probe's transactional fixture copy is temporary measurement tooling;
the repository's test helper, production code and test scheduling are unchanged.


### Batched SQLite fixtures and connection-local unit matrices (11 October 2026)

The measured fixture improvement is now implemented. Shared test-only
`rust/tests/support/schema.rs` includes the unchanged Go migrations and owns the
existing schema/version setup and seed rows. Schema initialization and seeding
each run in their own transaction. File fixtures still create an actual
`db/store.db` and perform ordinary SQLite commits; the same helper also initializes
connection-local in-memory databases for adapter unit tests. Production creation,
store opening, connections, receipts and synchronization policy remain unchanged.

The cached-statement binding/reuse unit test now uses an in-memory connection
with the actual Go schema, retaining all its binding, duplicate-insert, constraint,
query, deletion and successful-close assertions. The full-record and summary
malformed-value matrices moved from file-backed integration targets into private
adapter unit modules, calling the same transaction/query/decoder functions and
checking the same public error classification. All seven malformed full-record
updates, three malformed summary updates and missing-map-set rejection remain.
Timestamp precision/timezone assertions moved into a pure decoder unit test.
The duplicate integration cases were removed; there is no public in-memory store
mode, widened production visibility, new dependency or asynchronous code.

File-backed coverage remains for public store opening, unchanged database bytes,
schema changes from another connection, reader/writer visibility, committed WAL,
missing/corrupt/symlink paths, production publication and conditional cleanup
identity. CLI and generic runtime tests continue using the concrete adapters.
The real-kernel runner and program-release assertions were not changed: the
preceding tmpfs experiment exposed a timing-sensitive immediate-absence assertion,
not an in-memory SQLite race. Its precise kernel-release cause remains unproven;
normal real-kernel contracts continue using disk-backed fixtures.

The focused `rust-test-xdp-store` target passed both adapters and shared XDP
snapshot/replacement contracts. The complete `direnv exec . env
BPFMAN_KERNEL_TIMINGS=1
BPFMAN_E2E_SCRIPT_TIMELINE=/tmp/bpfman-sqlite-fixture-gate/gate-timeline.jsonl
make rust-check lint-make` gate exited zero. Formatting, Clippy with warnings
denied, userspace/compile-fail and shell runner contracts, real-kernel acceptance,
Rustdoc with warnings denied and Make lint/undefined-variable probes passed.

All 58 reported userspace groups retain 367 cases, with the same test identities
after accounting for the three relocated module paths. The sum of reported
target durations fell from 21.13 s in the preceding checkpoint to 10.28 s; this
excludes compilation and prerequisites. The SQLite library's eight tests took
0.06 s, the eight remaining file read contracts took 0.17 s, and the three
remaining file full-record contracts took 0.20 s. Before this slice those groups
contained five/nine/five tests and took 0.11/3.01/6.36 s respectively. The
fake-kernel target retained all 60 cases and owned tmpfs cleanup, taking 2.21 s.

A three-fixture probe of the checked-in helper on Btrfs confirmed uninstrumented
means of 30.9 ms for schema construction and 27.1 ms for seeding, compared with
622 ms and 81.8 ms before this slice. Timestamped sync-only traces assign four
syncs to construction and four to seeding, instead of 92 and 12. Both read phases
still perform no syncs, and all probe scopes passed empty-directory cleanup.

The complete kernel stage passed all 100 tests exactly once in 96.46 s
(49.98 s scripts and 46.43 s retained stages), preserving the same 1032 phase
counts and test identities. Both backends passed all 84 admitted files, 64 named
cases and four additional shared scripts. All 257 script process exits, 250
owned-runtime cleanups and four cohort outcomes passed, with scheduler limits and
cohort overlap preserved. This slice targets userspace fixture overhead; the
kernel/corpus timing difference from the previous gate is not an attributed
optimization. Evidence is in `/tmp/bpfman-sqlite-fixture-gate/`, including raw
separate gate logs, the script timeline, coverage reports,
`userspace-comparison.json` and the post-change fixture phase/sync probes.

### Reassessing real-kernel tmpfs release failures (14 October 2026)

The earlier tmpfs failure is reproducible on the current code and is a race
between the test's immediate program observation and queued kernel link release.
It is not evidence that SQLite loses a commit or that application cleanup retains
the program indefinitely. This conclusion combines an unchanged baseline,
observation before fixture teardown, matching kernel sources and live tracing.

Four alternating disk/tmpfs pairs ran the exact JSON and SQLite XDP attached-unload
tests serially, using supplementary Make targets and owned fixture scopes. All
eight disk-backed tests passed; seven of eight tmpfs tests failed at the immediate
program-ID absence assertion. Managed program/map/bytecode absence had already
passed. Both stores failed, and the serial reproductions exclude concurrent test
fixtures as a necessary trigger. These are diagnostic repetitions, not a benchmark.

A temporary observation loop then required absence within five seconds while
keeping the application, completed unload report, interface and mounted runtime
alive. Five alternating pairs produced 20 release observations per filesystem,
covering both survivor choices on each store. All disk observations found the
program absent immediately. On tmpfs, six were immediately absent; fourteen
first observed a live program and found it absent on the next observation after
a one-millisecond sleep. The longest observation interval was 1.221 ms. These
intervals include querying and scheduling; they are upper observations of release,
not exact kernel destruction times. No fixture teardown or additional mutation
was needed to make absence pass. All owned scopes passed empty-directory cleanup.

The running kernel is Linux 6.18.54. The older reference checkout is v6.12, so
the mechanism was checked against matching 6.18.54 sources:

- [bpffs inode eviction](https://github.com/gregkh/linux/blob/v6.18.54/kernel/bpf/inode.c)
  releases a pinned link through `bpf_any_put` and `bpf_link_put`.
- [BPF link release](https://github.com/gregkh/linux/blob/v6.18.54/kernel/bpf/syscall.c)
  queues `bpf_link_put_deferred` when the link reference count reaches zero.
  `bpf_link_free` invokes tracing-link release and drops the link's program
  reference during deallocation. Final program release removes the program ID;
  later RCU memory freeing is a separate step.

Temporary bpftrace probes observed the exact affected program and extension-link
IDs. Three delayed cases showed the last extension link queued from the test
process, then freed by a different kernel worker with link reference count zero
and program reference count one. The same worker subsequently entered
`bpf_prog_put_deferred` with program reference count zero. The queued-link-to-final-
program-release intervals in these traced cases were approximately 0.755, 1.742
and 1.694 ms. Tracing changes scheduling, so those values are mechanism evidence,
not substitutes for the untraced observations. The tracer exited successfully and
removed its probes. This identifies asynchronous link teardown, rather than merely
inferring an unspecified RCU delay from eventual disappearance.

The observations support disk-backed persistence hiding the invalid immediate-
release assumption by adding time between extension unpinning and the final
program observation. The experiments do not apportion every filesystem delay
or guarantee that disk-backed tests cannot also race.

The XDP attached-unload test now retains immediate managed-file absence checks
and requires exact kernel-ID absence within a bounded five-second deadline.
Unexpected observation errors still fail. The application, report, interface
and runtime remain alive throughout this check. One survivor choice additionally
retains a real program descriptor before unload: managed teardown must succeed,
but a 20 ms bounded observation must still find the program alive. After closing
that descriptor, the same absence check must pass. This control ensures waiting
cannot silently accept a genuine retained reference. Production unload,
persistence, synchronization and packet assertions are unchanged by that test
correction. The production tracepoint fix and runner follow-up are described below.

Raw baseline, untraced observation, corrected-test and kernel-trace evidence is
in `/tmp/bpfman-tmpfs-reassessment/`, including `baseline.json`,
`release-summary.json`, `corrected.json`, `trace.log`, separate stdout/stderr
logs, matching kernel source files and the temporary supplementary Make/runner
tools.

Five further alternating pairs passed all 20 corrected XDP unload tests across
both stores and filesystems, including the held-descriptor control. The broader
eight-worker tmpfs contract run then exposed a separate tracepoint reattachment
race: JSON's `tracepoint_attachment_lifecycle` failed at `commit wins` with
`bpf_link_create` returning `EEXIST`. All 46 other independent JSON tests passed,
including the corrected XDP unload test. The runner correctly stopped before the
SQLite stages, and its owned scope passed cleanup; this is a failed experiment,
not a successful complete tmpfs gate.

Ten alternating serial tracepoint pairs reproduced four failures in ten JSON
tmpfs tests, with all ten SQLite tmpfs and all twenty disk tests passing. These
failures were all the same reattachment error. An additional eight traced pairs
reproduced four JSON tmpfs failures, again with SQLite and disk passing. The
kernel trace identifies the affected old link and program: old-link release was
queued, insertion of that same program into the tracepoint array returned
`-EEXIST`, and only afterward did a kernel worker run `bpf_link_free` and
`bpf_perf_link_release` for the old link. The kretprobe prints the 32-bit error
as `4294967279`, which is signed `-17`; it must not be read as success.

[The matching program-array code](https://github.com/gregkh/linux/blob/v6.18.54/kernel/bpf/core.c)
rejects insertion of a program already present. This is a real detach/reattach
ordering gap, distinct from XDP's eventual object-reclamation assertion.
Rust's tracepoint `remove_link` currently delegates to filesystem pin removal
without retaining a live link descriptor across unpinning. The Go
`platform/ebpf/pin.go` implementation already opens a link FD, removes its pin
while retaining that FD, then closes it. With no additional holders, that final
FD close invokes `bpf_link_put_direct` and finishes hook release synchronously.
Perf-event links have no `bpf_link_ops.detach` callback on this kernel, so adding
`BPF_LINK_DETACH` alone would not address them. Any Rust fix must preserve
descriptor confinement, exact identity checks, receipt retention on failure and
the existing policy for other descriptor holders. Do not add sleeps or automatic
attach retries to make this contract pass.

The first broader tmpfs run established this separate implementation gap.
Additional evidence
includes `tmpfs-contracts.*.log`, `tracepoint.json`, `tracepoint-traced.json` and
`tracepoint-trace.log`, with successful tracer exits and owned-scope cleanup.

The production tracepoint removal path now retains a live link descriptor through
a narrow filesystem/kernel bridge. Adoption uses an opened, confined pin and
validates the exact kernel ID and perf-event type before effects. All temporary
O_PATH descriptors close before the retained live FD, including the descriptor
used by the removal boundary: an O_PATH handle can keep the unlinked bpffs inode
and its pin reference alive. Unpin failures retain the original ownership receipt.
With no additional link holders, final FD close invokes direct kernel link release,
matching Go's remove-then-close ordering. This fixes detach/reattach without sleeps
or automatic attach retries. Other holders can still retain kernel objects; this
does not introduce a synchronous kernel detach API for perf-event links.

Ten alternating pairs with this fix passed all forty tracepoint tests on both
stores and filesystems (`tracepoint-fixed.json`). The real-kernel runner now
prefers an owned tmpfs fixture scope as well. Fake and real suites share
`test-fixtures.sh`, preserving actual stores, files, locks and synchronization
calls. It requires an empty owned scope after child termination, fails on residue,
forwards signals and falls back to the normal temporary filesystem if tmpfs is
unavailable. `BPFMAN_FAKE_KERNEL_TMPFS=0` and `BPFMAN_KERNEL_TMPFS=0` retain explicit
disk-backed comparison lanes. Other test targets keep their incoming `TMPDIR`.
Kernel privilege, mount namespace, backend ordering and concurrency limits are
unchanged. Fast shell contracts exercise both scope selections and cleanup.

The next tmpfs run exposed the identical immediate extension-program release
assumption in TC attached unload. Its owned pins and managed files were already
gone, while the EXT program was still observable. TC now uses the same bounded
exact-ID observation helper as XDP, before dropping the report or fixtures;
immediate artifact and filter/clsact assertions remain. The complete normal
`rust-test-kernel-load` target then passed all 98 retained contracts on tmpfs in
38.66 s, with both backend stages completing and successful empty-scope cleanup.
The timing is a local sample, not an attributed optimization or full-gate time.
The script runner also forwards `TMPDIR` through sudo, so its owned case/cohort
fixtures inherit the kernel suite's selected storage scope.

The final normal `direnv exec . env BPFMAN_KERNEL_TIMINGS=1
BPFMAN_E2E_SCRIPT_TIMELINE=/tmp/bpfman-tmpfs-reassessment/final-gate-timeline.jsonl
make rust-check lint-make` exited zero. Formatting, warnings-denied Clippy and
Rustdoc, userspace/compile-fail and runner contracts, real-kernel acceptance and
Make lint/undefined-variable checks passed. ShellCheck passed for the fixture and
Cargo runners. Both fake and real suites reported tmpfs selection and successful
empty-scope cleanup.

All 100 kernel tests ran exactly once in 90.71 s. Both stores passed all 84
admitted files, 64 named cases and four additional shared scripts: all 257 script
process exits, 250 owned-runtime cleanup markers and four cohort outcomes passed.
The 32/four script limits and scope overlap remain intact. The complete per-test
phase matrix is identical to the preceding gate: 1032 completed records, including
832 forwarding, 160 broadcast and 40 worker phases. This confirms retained
coverage, not a controlled speed comparison.

A final trace of one disk/tmpfs pair passed all four tracepoint tests. All twenty
observed links had two references at unpin and were subsequently freed in the
same calling process, confirming the intended direct-close ordering rather than
merely observing that reattachment stopped failing. The tracer exited zero and
removed its probes. Final evidence includes `final-report.json`, separate final
gate logs, the final script timeline, `tracepoint-final-trace.log` and its summary.

### Deterministic owned-link teardown and kernel reclamation (14 October 2026)

The bounded post-unload checks above were an intermediate checkpoint. Rust now
uses retained descriptor ordering for managed XDP and TC freplace links as well
as standalone perf-event links. The filesystem adapter adopts the link through
its verified pin, validates its exact ID and kind, removes the owned pin, closes
all pin O_PATH descriptors, then closes the retained live descriptor. A private
removal helper owns that ordering; runtime and pure lifecycle policy remain
unchanged. TC retains each extension's ID with its individual removal receipt,
so partial retirement and retries cannot shift an ID onto another remaining slot.
Independent removals still get one attempt each and dependent native/evidence/
directory cleanup remains blocked by any failure.

Aya's freplace links have tracing attach type zero: the EXT program type selects
replacement. An initial test run rejected these links because the new removal
check used a wrong nonzero attach type. That exploratory run is retained in the
evidence. The check was corrected against Aya 0.14's actual extension attachment
implementation before the successful complete retained-contract run.

XDP and TC attached-unload tests now query program-ID absence immediately after
successful teardown, while reports, applications, interfaces and mounted runtimes
remain alive. The retained-program control makes one direct presence observation,
closes its FD, then makes one direct absence observation. XDP last-detach likewise
requires immediate extension-link ID absence. Native egress programs which never
acquired a map-held reference also use immediate program observations. The added
`program_absent_within` polling helper and its 20 ms held-reference wait are gone.
The complete retained kernel suite passed all 98 tests on tmpfs in 39.68 s before
the final native-egress assertion follow-up; this is local test time, not a benchmark.

A fresh trace of the immediate XDP unload tests observed 44 managed freplace links:
each had two references at pin release and was freed in the same calling process.
Both stores passed, and the trace and fixture scopes exited/cleaned successfully.
This confirms the release mechanism rather than relying on a delay before querying.

This is a deterministic ownership boundary, not a general kernel reclamation
barrier. Extra descriptors or pins can retain a link or program. Program-ID absence
does not prove all program memory and referenced maps have been freed. Linux's
program destruction releases used maps through deferred work after RCU; DEVMAP
entry removal and map destruction can also defer their egress-program references.
The existing map/native-egress lifetime contracts retain bounded observation of
that genuinely asynchronous kernel work. They remain separate from checking that
Rust finished its owned teardown; no fixed sleep replaces a completion guarantee.
An event-driven reclamation test would require a suitable kernel notification or
explicit synchronization boundary, not merely another descriptor held by Rust.

Evidence is in `/tmp/bpfman-deterministic-release/`: initial failed and corrected
contract logs, the direct-release trace and `direct-extension-release.json`, and
the full gate's separate stdout/stderr and script timeline.

The final `direnv exec . env BPFMAN_KERNEL_TIMINGS=1
BPFMAN_E2E_SCRIPT_TIMELINE=/tmp/bpfman-deterministic-release/gate-timeline.jsonl
make rust-check lint-make` exited zero on tmpfs. All 100 kernel tests passed
exactly once, including the immediate native-egress assertions, along with
formatting, warnings-denied Clippy/Rustdoc, userspace/compile-fail and runner
contracts, and Make lint/undefined-variable checks. Both stores passed all 84
admitted files, 64 named cases and four additional shared scripts. All 257 script
process exits, 250 owned-runtime cleanup outcomes and four cohort outcomes passed.
The complete retained per-test phase matrix is unchanged at 1032 records. Kernel
stage time was 88.10 s; this local sample is not a benchmark or full Make duration.

### TC ingress parity audit and independent egress (14 October 2026)

Three previously unlabelled scripts passed unchanged against Rust on both JSON
and SQLite with owned tmpfs fixtures and successful cleanup:
`TestDispatcher_SlotReusedAfterDetachTC`, `TestTC_DefaultProceedOnRebuild` and
`TestTC_DispatcherChainProceedOn`. Their assertions needed no implementation fixes;
they now carry `rust=ok` parity labels.

TC direction is now a refined `TcDirection` inside `TcKey`. The same complete
revision/consuming recovery interpreter handles either hook. Direction travels
through preparation, staged ownership, exact legacy netlink filters, retained
switch/restoration receipts, store selection and attached unload grouping. Separate
`fs/tc-ingress` and `fs/tc-egress` collections prevent pin collisions. Ingress's
existing ownership evidence names are preserved; egress uses
`tc/egress_dispatcher_<nsid>_<ifindex>_<revision>`. Observation and removal remain
confined. No Aya, dependency, TC CONFIG ABI or Go SQLite schema changes are needed.

A first attachment on the other hook validates the existing managed dispatcher
and copies its established clsact ownership under the writer lock. Each subsequent
revision retains that evidence. Either hook can reclaim the shared owned qdisc
once both filter blocks are empty. Borrowed qdiscs remain borrowed; foreign filters
remain untouched and retain clsact. This avoids losing ownership when the original
creating hook drains before the other hook. Netlink observation/mutation remains
subject to concurrent privileged writers outside bpfman's lock.

The CLI accepts `link attach tc ... ingress|egress` and reports the stored direction.
`dispatcher get tc-egress` and directional list filters now select the matching
complete snapshot. Both unchanged requested egress gates,
`TestTC_EgressTrafficCounting` and `TestTC_IngressEgressIndependence`, passed with
both stores on tmpfs and now carry parity labels.

New JSON stores use format 9. Ingress rows omit the default direction to preserve
older wire representations; egress rows explicitly record it. Formats 7 and 8
retain their operations and reject egress without changing state. Snapshot
validation rejects egress hidden inside an older format. Tests retain the format-7
singleton/replacement boundary and add older-format egress refusal, unchanged bytes
and invalid persisted-direction evidence. The future-version fixture now uses 10.
There are no implicit upgrades or conversions.

Existing shared real-store contracts and stateful fake replacement/unload fault
and cancellation matrices now exercise both hooks. Failed cleanup on one direction
still gives the independent other direction on the same interface its one pass.
Six new real-kernel tests run the existing exact-filter, publication compensation,
restoration, partial retirement and reopened unload contracts for egress as well as
ingress. The immediate post-unload program-ID checks and retained-link ordering
remain intact. `make rust-test-tc` runs both store contracts and all twelve kernel
TC tests; ingress/egress named aliases select that same complete subset.

The marked-frame probe sends on `source0` and receives on `in0`, so the egress
contracts attach on `source0`. The first exploratory run showed ENOBUFS from an
intentional TC_ACT_SHOT at the sender. Linux 6.18.54's
[packet_snd](https://github.com/gregkh/linux/blob/v6.18.54/net/packet/af_packet.c)
translates the transmit drop status through `net_xmit_errno`. An explicit
`packets-egress-drop` mode now requires ENOBUFS for every one of the three sends;
packet absence, exact member counters, capture windows and ordinary modes retain
their existing assertions. The corrected focused target passed all twelve TC
kernel tests and both store contracts, with empty-scope tmpfs cleanup.

`TestTC_BidirectionalClsactOwnership` adds twelve Rust-only cases covering both
attach orders, both drained hooks and owned/borrowed/foreign-filter qdiscs. It
requires unchanged survivor snapshots, correct directional dispatcher listing,
reopened CLI unload, retained foreign filters and immediate clsact postconditions.
Both stores passed before adding its `rust=ok` label. Foreign-filter checks count
actual option-bearing matchall entries, excluding tc's JSON summary entries.
No script runner changes were needed.

Evidence is in `/tmp/bpfman-tc-directions/`: ingress and egress per-store logs,
initial and corrected kernel contract logs, per-store ownership case logs and
userspace/lint logs. Full-gate results follow below.

The final `direnv exec . make rust-check lint-make` completed every Rust gate:
formatting, warnings-denied Clippy/Rustdoc, userspace and compile-fail contracts,
all 60 shared fake-kernel tests (with expanded TC direction matrices), and all 106
real-kernel tests exactly once. Both fake and real owned tmpfs scopes cleaned
successfully. The kernel stage completed in 95.03 s; this is a local sample, not
a controlled performance comparison.

Both stores passed all 90 admitted script files (57 shared Go/Rust parity files
and 33 Rust-only files), including 76 named cases and the four additional
shared-runtime scripts per store. Normal inventory, artifact and unmount checks
remain required. This run did not enable phase/timeline export; it makes no new
phase-count or scheduler-occupancy comparison.

The subsequent Make lint in that composite invocation initially failed because
checkmake parsed a multi-target alias declaration as one target name. Splitting
the aliases into separate declarations left their dependencies unchanged;
`direnv exec . make lint-make` then exited zero, including the undefined-variable
probes. Final logs are `gate.stdout.log`, `gate.stderr.log` and
`make-lint-final.log`; `final-report.json` records the unique kernel-test count
and admitted manifest counts. `git diff --check` is clean.

### TC parity audit and missing-namespace cleanup (14 October 2026)

Eleven existing scripts passed unchanged against Rust with both JSON and SQLite
on owned tmpfs fixtures, including successful runner inventory/artifact checks and
cleanup:

- `TestDispatcher_AttachExceedsMaxProgramsTC`
- `TestDispatcher_LifecycleAfterLastDetachTC`
- `TestDispatcher_MultipleInterfacesIndependentTC`
- `TestTC_NetnsVethPairLinkRoundTrip`
- `TestTC_NetnsDispatcherRebuild`
- `TestTC_MultiPriorityChainWithOKProceedOn`
- `TestTC_MultiPriorityChainDefaultProceedOn`
- `TestTC_ProceedOnUnspecEncoding`
- `TestTC_RejectsAttachOverForeignIngressQdisc`
- `TestTC_DeletedNetnsDetachRemovesDispatcher`
- `TestTC_NetnsVethPairDeletedNetnsDetachRemovesDispatcher`

The capacity script exposed a diagnostic mismatch: Rust rejected the eleventh
member before staging, but combined full membership with exhausted revision space
in one message. TC now translates the pure planner's distinct errors, including
`no free dispatcher slots` for capacity. Neither its assertion nor the planner's
admission ordering changed.

The two deleted-namespace gates required a separate consuming kernel observation.
`TcRemoval` distinguishes a validated live stage/filter from an opaque `TcOrphan`.
Only ENOENT from opening an explicit namespace selector admits Rust orphan cleanup.
Interface absence, permission/inspection errors and changed namespace/interface
identity remain failures. Attachment/replacement still requires live namespace
evidence. Orphan admission validates the complete confined revision, each present
native/EXT pin and durable clsact ownership before effects. The adapter rechecks
selector absence before removing pins through the same held-descriptor teardown
and partial-progress removal as ordinary TC retirement. A reappeared selector or
foreign runtime returns the unconsumed receipt.

Pin cleanup precedes conditional store deletion. JSON and SQLite can now delete
an unchanged complete dispatcher receipt atomically, including every member;
stale and foreign receipts remain invalid. Neither the Go schema nor JSON formats
change. Other programs' member records in that orphan scope disappear, while the
programs themselves remain loaded. Other hooks and namespaces remain untouched by
detach. Attached unload consumes repeated target-program members of a successfully
removed scope once; failed cleanup blocks that scope's later members while other
scopes still get a pass. Record-only retry does not repeat successful pin removal.

An absent name does not prove kernel namespace destruction. Other holders or
another name may keep the original namespace, native filter and qdisc alive.
Rust orphan cleanup removes validated managed pins and intent without inventing
filter/qdisc authority. Its real-kernel contract keeps the original namespace
alive under another name and requires managed program-ID absence immediately
after unload. No post-unload sleep or polling was added. Fake and real contracts
cover repeated members, multiple programs, both hooks, pin/store failures,
reappeared selectors, changed namespace identity, wrong runtime, malformed
ownership before effects, detach isolation and reopened recovery. Shared store
contracts cover complete deletion and stale/foreign receipt rejection.

The new shared `TestTC_DeletedNetnsLifecycle` adds four cases combining both hooks,
repeated attachments, another program's orphan membership and an independent live
namespace. That live attachment uses a separate veth pair: deleting either
endpoint of the orphan pair also destroys its peer. Go initially passed both
detach cases but failed both attached-unload cases on both stores: its first
member removed the complete dispatcher snapshot, then the next member attempted
to rebuild that missing snapshot (`record not found`). Rust passed all four.

Review of the original Rust implementation in `bpfman/src/lib.rs` found historical
precedent for accepting an already-removed dispatcher. That is evidence of intent,
not a correctness specification: the old code uses `Path::exists`, deletes its
dispatcher database tree before filesystem cleanup, ignores namespace-entry
failures and leaves other members' records until individually removed. Its
`test_netns_delete` does not cover repeated attachments; the legacy binary was not
executed during this review.

The Go fix is deliberately limited to unload bookkeeping. The existing rebuild
and removal operations now return a private typed result indicating whether they
rebuilt or removed the dispatcher. Unload records successful removals within its
own writer-locked operation and skips later admitted members only for those keys.
A missing snapshot at the first lookup and a failed removal still fail; arbitrary
`ErrRecordNotFound` results are not suppressed. Its real-SQLite/fake-kernel tests
verify that both failures retain the program and unchanged member records, and
that a repaired retry consumes the removed scope once.

Go's namespace checks, kernel adapter, cleanup ordering, warning/GC policy and
existing fake-kernel behavior remain unchanged. The broader proposed Go namespace
validation and strict orphan cleanup changes were removed after review. Rust
retains its stricter validation and explicit consuming recovery. This slice does
not claim identical failure-reporting or physical reclamation across implementations.

All four new script cases pass with the reduced Go change and with Rust on both
stores, using identical assertions and successful normal tmpfs cleanup. Only the
new shared file gains `rust=ok`; it has no Rust-only label. All 172 pre-existing
scripts, including their labels, remain byte-for-byte unchanged. The eleven older
scripts above remain outside the unchanged manifest and their passes are recorded
separately. The admitted set now has 91 files per store: 58 shared Go/Rust and 33
existing Rust-only files; 26 case files expand to 80 named cases.

Evidence is in `/tmp/bpfman-tc-parity-orphans/`. `final-{json,sqlite}.log` records
the eleven unchanged Rust acceptance passes. `shared-go-{json,sqlite}.log` records
the original Go failure. The final reduced-patch matrix is
`reduced-{go,rust}-{json,sqlite}.log`. `go-reduced.stdout.log` and
`go-reduced.stderr.log` retain the passing complete Go unit suite, vet and build:
`direnv exec . make test bpfman-vet bpfman-compile` with
`EXTRA_GOFLAGS=-ldflags=-linkmode=external`. The override is required for Go's cgo
test binaries in the Nix dynamic shell; the default-link attempt failed at the
linker. Earlier exploratory logs are retained separately, and do not describe the
final reduced Go patch.

The latest full `direnv exec . make rust-check lint-make` exited zero, with all
108 unique real-kernel tests, 64 fake-kernel tests, warnings-denied Clippy/Rustdoc,
userspace/compile-fail checks and successful owned tmpfs cleanup. Both stores
passed all 91 admitted files and the four additional shared-runtime scripts.
`shared-gate.stdout.log` and `shared-gate.stderr.log` retain that run. Its kernel
stage took 106.41 seconds while Go validation was also running, not a controlled
performance comparison. Rust code and script assertions are unchanged by the
subsequent Go reduction; the final targeted matrix verifies the reduced Go binary.

Broader Go acceptance found a separate existing attach-rollback issue:
`TestTC_RejectsAttachOverForeignIngressQdisc` passes its refusal assertions but
leaves empty ingress/egress revision directories. It reproduces on JSON with an
unmodified Go build constructed through a source overlay. See
`baseline-go/overlay.json`, `baseline-go/foreign-qdisc-json.log` and
`contract-go-json.log`. Rust passes this unchanged script. That separate Go issue
is recorded and deferred; it is not fixed or labelled as passing here.

TCX, outer-filter status, general orphan reconciliation and cross-implementation
TC teardown remain outside this slice. Missing a live interface in an otherwise
accessible namespace is not treated as missing-namespace cleanup in Rust.

### Native TCX lifecycle and ordering (14 October 2026)

TCX now loads classifier selections as native SCHED_CLS rather than EXT programs.
It attaches through Aya's explicit `TcAttachOptions::TcxOrder`, without a legacy
TC dispatcher or clsact qdisc. Both stores support program and link observations,
ingress/egress, explicit namespaces, metadata, duplicate refusal, detach and
attached-program unload.

Go's `computeTCXAttachOrder` inserts before the first greater priority, otherwise
after the last existing member. Its detail queries derive position by priority
and managed ID. Original Rust's `sort_tcx_links` and
`add_and_set_tcx_link_positions` likewise use priority and previous position, with
new ties appended. The new pure planner follows ascending priority and attachment
order, including priority zero. Native packet-chain acceptance establishes actual
continuation and stopping independently of the reported managed rank. Historical
implementations remain evidence, not the correctness specification.

Insertion inspects each recorded native link on the selected hook. Confirmed
missing links and detached links do not become anchors; failed inspection or
changed program/hook identity remains an error. Pending intent requires explicit
cleanup. Duplicate program/hook requests identify the surviving managed link and
fail before touching its pin. The adapter validates and retains the anchor program
FD through attachment. The adapter's checked conversion to Aya 0.14's `ProgramId` stays inside the
existing narrow syscall module, after validating the live classifier ID and type.
Aya itself, dependencies and the legacy Rust workspace are unchanged.

TCX uses the same production pending-intent, cancellation, finalisation and
consuming compensation interpreter as tracepoint attachment. Its canonical pins
are `fs/tcx-{direction}/link_{nsid}_{ifindex}_{program}`. Filesystem adoption
validates confined traversal, pin identity, kernel link ID, program and native hook.
Removal retains the actual link FD, explicitly detaches it, removes the owned pin,
then closes the retained FD after the pin descriptors. A genuine external link
holder may preserve the object but cannot preserve execution after successful
detach: the real contract reattaches the same program before closing that holder.
Immediate managed program-ID absence is required after unload without external
holders. The held-program control requires presence until its FD closes, followed
by immediate absence. No post-unload sleeps or polling were added.

Native links provide teardown authority independently of a namespace selector.
A prepared attachment refuses a changed namespace selector before acquiring a
link. Once attached, exact adopted link descriptors allow cleanup when the stored
selector disappears or points elsewhere, with the original namespace still alive.
This differs from legacy TC's namespace-dependent filter removal: TCX needs no
invented netlink authority in an unreachable namespace. Both hooks and independent
namespaces are exercised in the same attached unload.

The native multi-link recovery contract exposed excessive dependency in the Rust
standalone unload planner: failure deleting one link record blocked unrelated
links. It now attempts each independent link once. Failed pin removal blocks only
that link's record; any unresolved link still blocks program teardown. Tracepoint
fault and pending-intent contracts now require this stronger independent progress,
retain exact receipt identities and attempt history, and prove completed pin
removal never repeats. Failed store deletion leaves record-only retry ownership;
reopening the application does not re-adopt completed pins. Go is unchanged.

SQLite retains Go schema 2 and the existing native TCX detail table. New JSON
stores use format 10; formats 1–9 retain their previous operations and refuse TCX
without implicit upgrade. Existing tracepoint target strings retain their encoded
shape; native targets use a typed object. Managed position is derived from one
consistent store snapshot, not stored as mutable teardown authority and not
presented as a live inventory of foreign kernel programs. Shared real-store
contracts cover native pending/finalised intent, duplicate refusal, foreign/stale
receipts, derived positions and preservation of loaded programs after detach.
JSON-specific contracts prove old-format refusal without changing the snapshot.

Eight existing acceptance files run unchanged against Rust on both stores:

- `TestTCX_LoadAndGet`
- `TestTCX_LinkRoundTrip`
- `TestTCX_DuplicateAttachRejected`
- `TestTCX_PriorityOrdering`
- `TestTCX_ZeroPriorityDefaultOrdering`
- `TestTCX_NetnsVethPairLinkRoundTrip`
- `TestMultiProgTCX_AllProceed_DefaultProceedOn`
- `TestMultiProgTCX_ChainStopsAtOK_DefaultProceedOn`

They remain outside the immutable label manifest. Two explicit backend-wide TCX
batches keep them in the normal full gate, alongside the existing 91 admitted
files and shared-runtime cohort. `make rust-test-tcx` selects these batches and
native kernel recovery contracts. Four additional stateful-fake tests run the
same public runtime with real SQLite and JSON stores, covering ordering, scope
isolation, failed acquisitions, publication/cleanup faults, cancellation and
reopened recovery. No existing `.bpfman` assertions or labels changed, and no new
script was introduced.

General stale-pin reconciliation and reconstructing lost in-memory cleanup reports
remain future recovery work. Managed priority does not reconcile arbitrary foreign
chain edits, and attachment does not provide an expected-revision CAS against
external privileged writers outside bpfman's lock. Legacy TC outer-filter status,
shared maps, additional program families and OCI sources remain separate work.

Parallel lifetime investigation found another source of references: the older
tracepoint attached-unload contract enumerated every live kernel program while
polling for its own release. Aya's iterator opens a program FD for each ID, so
that test could briefly hold another fixture's program. Its verification now
queries only its owned program ID and requires immediate absence. This removes
both the global observer and its post-unload polling; the separately classified
XDP global-inventory contract remains serial. The complete parallel contract stage
then passed all 108 non-corpus cases on both stores with owned tmpfs cleanup
(`parallel-contracts.log`, 43.29 seconds locally, not a benchmark).

The eight unchanged TCX scripts also passed against the existing Go binary on
both stores, including successful isolated runtime checks and owned tmpfs cleanup;
see `go-json.log` and `go-sqlite.log` in `/tmp/bpfman-tcx/`.

A later parallel run still caught a retained native link after its explicit
external-holder close. Removing the global enumerator was insufficient to establish
absence of other holders. A deterministic fork/exec control on Linux 6.18.54
confirmed that a child paused before exec retains a CLOEXEC BPF descriptor after
the parent closes it, and that the ID disappears immediately after the child
executes and exits. `fd-control.c`, `fd-control.mk` and `fd-control.log` retain
that standalone experiment; it uses a two-instruction native classifier and pipe
handshakes, without sleeps. The Rust syscall trace independently shows subprocess
`clone3` with `CLONE_VM|CLONE_VFORK` and without `CLONE_FILES`, copying the descriptor
table before exec. This supplies a concrete mechanism for temporary external
references in concurrently spawning fixtures; the trace did not reproduce the
particular failing native-link close. The instrumented diagnostic run passed TCX
but later hit a separate global XDP enumeration ENOENT, so it is not claimed as a
passing full gate (`fd-inheritance.trace`, `trace-contracts.log`).

The two native TCX lifetime contracts therefore run in each backend's existing
serial global stage, separately from concurrently spawning fixtures. Every test
remains selected, all immediate link/program absence assertions remain, and the
ordinary backend contracts retain eight workers. This establishes the no-external-
holder premise of the lifetime assertions without sleeping or changing production
teardown. The native contract explicitly verifies retained link and program holders;
production cannot revoke descriptors inherited by unrelated children.

Final validation: `direnv exec . make rust-check lint-make` exited zero. All
112 discovered kernel cases were selected and passed, as did 68 stateful-fake
cases, userspace/store/CLI and compile-fail contracts, warnings-denied Clippy and
Rustdoc, formatting, and Makefile lint. Both stores passed the 91-file admitted
corpus, four shared-runtime scripts and the eight separately selected TCX files.
All 173 existing `.bpfman` files, including their labels, were verified byte-for-byte
against HEAD. Owned fake and real tmpfs scopes passed normal cleanup. The kernel
stage took 99.84 seconds locally, with eight ordinary contract workers and the
existing development/test `aya-obj` opt-level-two override; this is not a controlled
benchmark. `/tmp/bpfman-tcx/gate.stdout.log`, `gate.stderr.log` and
`final-report.json` retain the final evidence. No Go source, dependency, lockfile
or legacy Rust source changed.
