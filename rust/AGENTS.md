# New Rust implementation guidelines

These conventions are adapted from `frobware/rty`'s `CLAUDE.md` and `DESIGN.md`.
The architecture and compatibility goals are in
[`docs/design/rust-reimplementation.md`](../docs/design/rust-reimplementation.md).

## Workspace and commands

- Work in the independent `rust/Cargo.toml` workspace. Do not build, link, or
  add dependencies on the legacy Rust workspace at the repository root.
- Keep Go and legacy Rust sources available as references. Go defines current
  behaviour; legacy Rust supplies low-level mechanisms, not the architecture.
- Drive builds, formatting, lint, documentation, and tests through the repository
  Make targets: `rust-build`, `rust-fmt`, `rust-fmt-fix`, `rust-lint`,
  `rust-test`, `rust-doc`, and `rust-check`. Every target must name
  `rust/Cargo.toml` explicitly. `rust-lock` refreshes only the new lockfile.
- Run those targets through `direnv exec .`. If `.envrc` selects the Go-oriented
  `.#static` shell, use `direnv exec . env NIX_LDFLAGS= make ...` for Rust: that
  shell's added static-glibc search path breaks ordinary dynamic Rust binaries.
  Do not change the user's `.envrc` or the Go build environment to work around it.
- Before handing off implementation changes, run `make rust-check` and inspect
  its exit status. Investigate failures; never skip or weaken a test to get a
  green result. Run relevant real-kernel acceptance tests when the implemented
  surface and environment support them; report untested boundaries accurately.
- `rust-test-userspace` runs all nonprivileged workspace tests, including CLI
  contracts and the stateful fake. The normal `rust-test`/`rust-check` gate runs
  this stage before the complete serial real-kernel binary, so cheap admission
  or persistence failures are found before the long packet suites. Keep automatic
  CLI test discovery in `test-userspace.sh`; do not replace it with a partial list.
- On NixOS, Go and Rust tests share `e2e-kmod-build` and
  `e2e/kmod/prepare-kdir.sh`. If `/lib/modules/$(uname -r)/build` is absent, the
  helper discovers the matching `kernel.dev` output from
  `/run/current-system/kernel` through `nix-store -q --deriver` and `--outputs`,
  then realizes it with `nix-store -r` if needed. Run the normal Make target;
  an absent store path does not by itself mean sources are unavailable.
  Nix discovery suppresses lookup stderr, so sandbox denial of the Nix daemon
  can surface as a misleading missing-build-tree error. Rerun the normal gate
  with the required sandbox escalation before declaring the prerequisite blocked.
  The helper creates the writable kbuild mirror and supplies BTF.
- `KERNEL_DEV` can explicitly select an already realized matching development
  output. The successful override recorded for kernel `6.18.54` was:

  ```sh
  direnv exec . make rust-check \
    KERNEL_DEV=/nix/store/l096hgmdqnz643qzcqjwahfyrsff5c2y-linux-6.18.54-dev
  ```

  `KERNEL_DEV` names the output root containing `lib/modules/<release>/build`,
  not the build directory itself. Check `uname -r` and that the path still exists
  before reusing it: a Nix store path can disappear after garbage collection.
  An explicit `KERNEL_DEV` bypasses automatic discovery and realization; if its
  path is gone, omit the override and let the shared helper realize the output.
  Keep using the normal Make targets and do not skip kernel tests.
- Assume passwordless sudo for real-kernel tests. Keep them in the normal
  `rust-test`/`rust-check` gate without privilege-related `#[ignore]` attributes.
  Build fixtures as the invoking user and run the kernel test binary through
  `sudo -n` in a private mount namespace; missing privileges must fail the gate.
- Local Linux sources are available for mechanism review at `~/src/linux.git`.
  Check the revision with `git -C ~/src/linux.git describe --always --dirty`
  before drawing version-specific conclusions. The checkout was at `v6.12`
  during DEVMAP acceptance work; the running test kernel was `6.18.54`.
  Continue using the shared build helper and `KERNEL_DEV` for matching kbuild
  sources; the reference checkout does not select the test kernel's build tree.
  Check the full running patch version for packet-path restrictions: Linux
  `6.18.54` explicitly rejects DEVMAP cloning of multi-buffer frames in native
  and SKB paths, unlike the older reference sources. Broadcast acceptance
  observes `EOPNOTSUPP` through a filtered redirect-error tracepoint; see the
  [verified boundary](README.md#xdp-devmap-broadcast-and-ingress-exclusion).
- Do not assume array DEVMAP's fragment-egress rejection also applies to
  DEVMAP_HASH. On Linux `6.18.54`, `include/linux/bpf.h`'s
  `map_type_contains_progs` omits DEVMAP_HASH, so ingress loading does not initialize
  hash ownership. The first native egress insertion initializes compatible
  fragment ownership; later egress updates still require matching fragment flags.
  `make rust-test-xdp-egress-hash` proves ordinary and genuine multi-buffer
  PASS/DROP, context/helper reads, rollback, and map-held lifetime in both stores
  and driver/SKB modes. Aya remains unchanged, and the array DEVMAP boundary is
  still tested explicitly. See [egress acceptance](README.md#xdp-devmap-egress-programs).

## TC ingress ownership

- Force `TcAttachOptions::Netlink` for legacy TC. Aya's ordinary `attach` selects
  TCX on modern kernels. Use its public `handle()` result; never rediscover or
  delete filters by priority alone. Validate the stored dispatcher program ID
  at the exact interface/priority/handle, ETH_P_ALL protocol and chain zero before
  deletion.
- The Aya adapter owns safe `rustix::net` route-netlink dumps and exact deletes
  that Aya's public API lacks. Managed filesystem mutations still belong in
  `bpfman-fs`; runtime and pure crates must not acquire syscall dependencies.
- Keep clsact ownership separate from operator metadata and Go schema 2. The
  confined file `<runtime>/tc/dispatcher_<nsid>_<ifindex>_<revision>` records borrowed (0)
  or created (1) ownership. Preserve borrowed qdiscs after reopening; reclaim a
  created clsact only if both ingress and egress are empty. A foreign filter
  preserves the qdisc and relinquishes ownership. Refuse classic ingress qdiscs,
  malformed ownership evidence, and changed filter identities before teardown.
- TC proceed-on shifts signed return codes by one: UNSPEC -1 is bit zero,
  PIPE 3 is bit four, dispatcher-return 30 is bit 31. Its CONFIG is 84 bytes;
  do not reuse the XDP ABI or return-code mask.
- Use Aya's public `SchedClassifierLink::attached` and `attach_to_link` for legacy
  filter replacement. Stage the complete revision first; validate the exact old
  target before switching, and retain both native handles if restoration fails.
  Never clean staged pins until restoration succeeds, or restore after publication.
  Copy clsact ownership into fresh revision evidence; retire only the old revision.
- New JSON stores use format 8 for TC replacement; format 7 retains singleton TC.
  Formats 1–7 never upgrade implicitly.
  Attached unload must adopt all TC attachments before effects, finish exact
  filter/stage/record prerequisites before program teardown, and retain blocked
  ownership for explicit retry. Reject newly acquired links between retry passes.
  `make rust-test-tc-ingress` runs both stores, including replacement and attached
  unload. Members of one dispatcher use its latest revision; a failed member blocks
  later members of that dispatcher while independent interfaces continue one pass.
- The unchanged Go clsact script has a production-policy guard. Rust's test harness
  explicitly sets `BPFMAN_E2E_CLSACT_RECLAIM=true`, forwarded by Make, to run its
  assertions against Rust without changing Go's reclaim policy.

## Visibility and crate boundaries

- Private by default. Use the narrowest visibility that an actual consumer
  needs: private, then `pub(super)`, then `pub(crate)`, then `pub`.
- `lib.rs` and `mod.rs` are facades for API documentation, module declarations,
  interface types, and exported symbols. Put implementation in private modules.
  A re-export list should show a crate's public API at a glance.
- Expose useful operations, not internal connections, storage rows, helpers, or
  representations. Do not make an item public merely to reach it from a test;
  test internals in their own module and reserve integration tests for consumers.
- Keep fields private where mutation could violate an invariant. Use constructors
  and `TryFrom` to refine untrusted data. Public fields are justified only for
  unconstrained value objects whose combinations are valid.
- `unreachable_pub` and `missing_docs` are denied in the normal lint gate.
  These are backstops, not proof that every externally reachable item is needed.
- Register each crate in the README and metadata tier tests when it is added.
  Normal workspace dependencies point strictly down through tiers. Pure crates'
  normal dependency closures and feature sets require explicit review.

## Domain modelling and SANS-I/O

- Put state-dependent data inside enum variants. Requests, heterogeneous
  records, effects, and lifecycle states use payload-bearing enums; avoid a
  discriminator plus unrelated optional fields. Derive kind from the variant.
- Data-free enums remain appropriate for parsing names, list filters, and other
  genuinely data-free vocabularies. Do not mistake them for the full domain model.
- Pure cores consume observations/events and return decisions/effects as data.
  They do not access clocks, processes, filesystems, databases, or the kernel.
  Use pure functions for complete observations and explicit state machines where
  later decisions depend on earlier effects or accumulated rollback state.
- Keep wire DTOs, persistence rows, domain values, and kernel-library types
  separate. Convert at boundaries; do not bring Clap, SQL, or Aya into the model.
- Use traits at real substitution boundaries, not as a translation of every
  Go interface. Prefer small interfaces near their consumers.
- Adapters own atomic database operations and local resource ownership. Keep
  cross-adapter compensation explicit, retaining primary and rollback failures.
- Model compensation as domain instructions carrying owned receipts, not raw
  path-based unlink/remove commands. Use consuming forward continuations for
  dependencies and a small explicit instruction set for independent cleanup.
  Success consumes a receipt in the adapter; failure returns unresolved ownership
  with its cause. Partial forward failures must also return unresolved resources.
- Attempt every independent compensation once per pass, even after failures.
  Never propagate a cleanup error early or retry one instruction inline. Retain
  the original operation error and all attempt history. Retry only unresolved
  work in a separate, caller-budgeted pass. Dependent cleanup requires explicit
  prerequisites; it must not be treated as an independent removal instruction.
- Keep injectable filesystem-effect traits narrow and near the interpreter.
  Mutating methods require runtime writer authority and typed owned receipts;
  real implementations delegate managed-object I/O to `bpfman-fs`. Fakes inject
  failures at individual domain effects, not just whole composite cleanup steps.
  They do not replace real-filesystem confinement or real-kernel tests.

## Rust, errors, and CLI

- Follow Rust standard-library conventions for spacing and layout throughout
  `rust/`, including production code, tests, and support modules. Separate
  functions, impl blocks, and logical groups of statements with blank lines;
  distinguish setup, actions, and assertions in tests. Keep closely related
  statements together rather than inserting a blank line after every statement.
  Apply this consistently to existing and new code so the workspace retains one
  style. Rustfmt does not supply all these boundaries: review spacing manually
  as well as running the formatter. Keep broad formatting passes separate from
  behavioural changes.
- Edition 2024; the workspace manifest owns the MSRV, dependencies, and lints.
- Keep the new workspace synchronous: no async/await, Tokio, async runtimes, or
  synchronous wrappers around async libraries. Use ordinary threads and scoped
  locking where concurrency is needed. Keep rusqlite for SQLite; any connection
  pooling must also be synchronous and remain inside the backend adapter.
- Forbid unsafe by default. Any exception requires a narrow, documented boundary.
- No `unwrap`, `expect`, or `panic` in production. Test-only exceptions must be
  scoped to tests. Keep failure represented in types and source chains.
- Use `thiserror` for adapter errors and `anyhow` at the binary boundary. Pure
  validation errors implement core traits directly: their external normal
  dependency closure is empty, avoiding Aya's unified `thiserror/std` feature. Error
  messages add context; sources carry causes. Do not interpolate a source into
  its parent's message and then render the chain again. The binary uses `{:#}`.
- Translate adapter failures into backend-independent categories at the runtime
  boundary. Public signatures and variant payloads must not expose concrete
  SQLite, Aya, HTTP, or other backend errors to generic consumers. Keep those
  causes private and available through `std::error::Error::source` for diagnostics;
  callers act on typed application categories, never strings or downcasts.
- Use Clap's typed subcommands, `Args`, `ValueEnum`/typed value parsers, aliases,
  delimiters, environment bindings, and conflict groups. Reject malformed input
  while parsing, before any I/O. Convert CLI types to domain types at the boundary.
- Bound help width to 80 columns. Let Clap provide root `--version`/`-V`; avoid
  redundant hand-written parsing and help. Test the command with `debug_assert`.
- Keep signal policy in the CLI and cancellation caller-owned, per operation.
  Check cancellation during admission, lock waiting, and between forward load
  effects. Never abandon owned receipts: cancellation before commit must use the
  production compensation path under the same writer lock. An in-flight commit
  determines its own outcome. After destructive unload starts, finish that pass.
  Compensation does not observe the cancelled forward token; explicit retries
  may cancel admission but must retain receipts. No automatic retries. Test
  effect-boundary cancellation and real process signals with both stores.
- Keep output and telemetry collection in the front end. Effectful libraries may
  emit `tracing` spans/events with stable operation names and crate/module targets;
  never install subscribers or write logs directly. The CLI configures `RUST_LOG`
  filters, stderr output, and optional timeline export. Separate lock waiting from
  lock ownership spans, and record durations without logging credentials, metadata
  values, bytecode, or serialized store contents. Pure crates remain free of tracing.

## Tests and compatibility

- `bpfman-fs::RuntimeLayout` owns runtime path spellings and the default root.
  Construct it at the input boundary; application operations accept the validated
  layout and obtain paths through its methods. Layout construction performs no
  I/O and does not imply filesystem readiness. Add accessors as consumers need
  them, rather than scattering joins or preemptively exposing every Go path.
- Store backend selection belongs to the binary composition root. Runtime
  depends on `bpfman-store` contracts, never on SQLite or a future file-format
  implementation. Keep format/version checks and atomic publication inside each
  backend. Use domain operations, not connections or transaction callbacks.
  Associated teardown receipts stay opaque and must validate backend/runtime
  identity on deletion and explicit retry.
- Open one active store at startup and move it into `Bpfman`; use the same
  setup in shared behavioural tests. Keep its store, runtime, and options private.
  Expose operations as methods taking domain inputs, without a separate runtime
  or caller-supplied writer. Mutation and explicit retry methods acquire scoped
  writer authority internally; admission failure must retain cleanup receipts.
  Keep interpreters and compensation drivers private. Prepared requests own
  validated inputs and must not carry a second runtime selection.
  Retain the handle returned by startup rather than discarding and reopening it.
  Reader handles clone without I/O; backend adapters own resource reuse and fresh
  snapshot acquisition. Never hold a shared connection-cache mutex across queries.
  Revalidate retained handles during mutation preflight, and validate reads within
  their snapshots. Existing-store readers must open and read without acquiring
  the giant writer lock. Initialize missing state under that
  lock after rechecking absence. Preserve early request/ELF validation before
  initialization. All read-only operations, including combined store/kernel
  observations, bypass the writer lock. Kernel and filesystem observations may
  change after the store snapshot; absence alone does not prove inconsistency.
- Store initialization observations and policy execution share one writer-lock
  scope inside the backend. The core
  decides create/use/reject from schema observations without knowing paths,
  SQLite, or resource handles. It may move opaque interpreter-owned evidence
  into a decision, but must never inspect, duplicate, or drop that evidence.
  Existing-store decisions carry that evidence rather than requiring a parallel
  optional store and a runtime check for an impossible combination.
  Observation errors are never treated as absence.
- Runtime-object creation/removal must use conceptual operations owned by
  `bpfman-fs`. Operations requiring serialisation borrow `&RuntimeWriter<'_>`
  (or are methods on it), with typed object identities. Do not accept a target
  path plus a separate lock permit or expose generic `remove(&Path)` methods. Raw removal
  calls are denied by Clippy; any eventual exception must be scoped to one
  private, audited removal module, not a whole crate. Keep lock files outside
  object cleanup. Dependency-owned temporary-file/SQLite cleanup is not an
  object-deletion API and must not be used to bypass this boundary.
- Never adopt `/` as a runtime root or remove `/`, the runtime root, bpffs mount
  root, or an object collection root. Future deletion must prove a strict owned
  descendant using descriptor-relative, non-symlink-following traversal; lexical
  prefix tests alone are insufficient. Test traversal, root aliases, siblings,
  unexpected object names/types, symlinked ancestors, and replacement races.
  Dangerous-target refusal tests must never invoke real deletion on host paths.
- Root confinement is stronger than root refusal. Every managed-object filesystem
  operation must be relative to a verified opened root, never an arbitrary
  absolute path, with no parent/symlink escape or unintended mount traversal.
  Layout validation alone does not establish this. `RuntimeDirectory` adopts
  the descriptor without acquiring a lock; `with_writer` acquires the lock
  before lending scoped, root-bound authority. Root/lock bootstrap is necessarily
  lockless; preparing the database directory and database creation require the
  writer. Bootstrap does not imply bpffs or managed-object readiness.
- Use the Go-created SQLite schema early. Runtime may create a missing
  database under the Go-compatible writer lock; store reads remain read-only.
  Never repair or migrate existing state implicitly. Embed the actual Go
  migration SQL with `include_str!` for creation and fixtures.
- SQLite queries and DML use cached prepared statements behind private, typed
  query functions. Bind named parameters completely on every execution and decode
  columns by name into persistence row types before domain validation. Keep raw
  SQL and rusqlite rows inside the adapter's query module; schema DDL remains in
  the authoritative Go migrations. These Rust signatures do not provide
  compile-time SQL/schema checking. Keep statement and transaction lifetimes
  within a connection checkout, with no cache mutex held across execution.
- Rusqlite owns database access and SQLite's journal/WAL/shared-memory lifecycle.
  The database pathname handoff is not descriptor-relative confinement against
  external filesystem replacement. Do not add a custom VFS or manipulate those
  auxiliary files through managed-object cleanup APIs.
- Mutating adapters that require serialisation borrow non-forgeable, root-bound
  `RuntimeWriter` authority. `WritePermit` remains a lower-level lock primitive,
  not sufficient application authority. Acquire the same `<runtime>/.lock` flock
  as Go. Recheck its identity before lending a writer; callers must not replace
  the lock while operations are active. Namespace helpers inherit a duplicated
  descriptor rather than acquiring by path. Close descriptors on scope exit;
  never explicitly unlock while inherited copies may still be alive.
- Compile-fail tests cover forging, cloning, and escaping writer authority and
  passing an unlocked runtime or a path-plus-permit to a mutating adapter. Pair
  them with filesystem replacement/refusal and real flock contention tests.
- Preserve the Go fake kernel's valuable scenarios. A stateful effect
  interpreter should track IDs, programs, links, pins, and dispatcher changes,
  reject invalid operations, and inject failures at explicit boundaries.
  Pair pure transition tests with runtime/fake tests for ordering and residue.
- Fault-injection tests for loading must enter the production forward interpreter
  through its private effect boundary, then exercise its production compensation
  finaliser. A test-only forward plan is not evidence that CLI orchestration
  retains partial acquisitions. Cross forward and cleanup failures; assert
  residue, blocked dependent cleanup, all outcomes, and explicit retry history.
- Generic lifecycle and outside-in tests use store contracts and public
  observations, never SQL queries or triggers to set up state or inject faults.
  Use real SQLite or JSON stores, including when the kernel is fake. Do not add
  a simulated store implementation. SQLite `:memory:` is a real backend and is
  suitable for adapter tests; use temporary files when testing multiple independent
  connections, reopening, locking, or persistent store identity.
  A test-only failure decorator may intercept
  a named operation and retain its receipt on failure; all successful persistence
  and receipt validation must delegate unchanged to the concrete backend.
  Keep persistence-format fixtures in explicitly backend-specific adapter and
  compatibility tests. Prefer Rust integration tests for the new workspace;
  continue using the unchanged Go DSL corpus for CLI acceptance.
  Every new store backend must run the existing generic lifecycle, CLI, and DSL
  scenarios by changing backend selection in test setup. Add backend-specific
  tests only for its persistence guarantees; do not duplicate behavioural suites.
- Put externally observable behavioural scenarios in `.bpfman` scripts, using
  the existing Go corpus first. Add scripts for implemented gaps; reuse existing
  syntax/helpers before introducing runner support. Keep Rust unit tests quick
  and focused on internal validation and transitions. Filesystem, persistence,
  process and kernel tests are integration contracts, not unit tests.
- `rust-test-unit` runs library/binary unit targets without integration tests or
  doctests. Keep process-wide signal fixtures in `tests/signal_policy.rs`.
- Keep real-kernel tests for guarantees the fake cannot establish: verifier,
  syscalls, namespaces, traffic, and kernel lifetime semantics.
- Reuse the unchanged `e2e/scripts/*.bpfman` corpus via the Go shell runner.
  Select a CLI using `BPFMAN_UNDER_TEST`, which sets both `BPFMAN_BIN` and `PATH`.
  Do not claim parity from decoding alone or weaken scripts for Rust.
  Passing file-bytecode scripts carry `#pragma labels={"rust":"ok"}`; add the label
  only after execution against Rust with both stores. Preserve other labels and
  assertions. Use `BPFMAN_E2E_SCRIPT_SELECTOR='rust=ok,!external'` to batch the set
  through the existing Go runner and its pooled interfaces/parallel scheduler.
  Rust-only scripts additionally declare `rust-only=true`; always select Rust
  explicitly with `BPFMAN_E2E_IMPLEMENTATION=rust` alongside `BPFMAN_UNDER_TEST`.
  The runner defaults to Go and skips Rust-only scripts even when selected.
  `rust=ok,!rust-only,!external` selects shared parity; `rust=ok,!external`
  selects all admitted Rust acceptance. The normal gate runs all admitted scripts
  per backend with `BPFMAN_E2E_ISOLATED_RUNTIME=1`: fresh per-script stores/bpffs,
  unchanged pooled interfaces and parallel scheduling. The Go runner checks each
  script's inventories/artifacts before unmounting, including after failure.
  Each backend additionally runs file lifecycle, XDP fill/drain/refill, XDP
  lifecycle and TC lifecycle together on a shared runtime. Keep this explicit
  concurrent-store coverage; normal Go Make runs remain shared by default.
  Leave isolation unset for full-corpus shared-store stress. `BPFMAN_KERNEL_TIMINGS=1`
  reports whole tests, runner batches and broadcast phases. The optional
  `BPFMAN_E2E_SCRIPT_TIMELINE` JSONL records scheduler queue/start/end, CPU and
  command completion; both it and `RUST_LOG` survive the privileged runner.
  Run backend batches sequentially because separate runners share a suite lock.
- Make each vertical slice's supported command surface explicit. Unsupported
  commands and flags fail clearly; never invent successful observations for
  functionality not yet implemented.
