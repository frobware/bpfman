# New Rust workspace

This independent workspace implements the
[Rust reimplementation design](../docs/design/rust-reimplementation.md).
The repository-root Cargo workspace is legacy reference material. Always name
this workspace's manifest explicitly:

```sh
direnv exec . make rust-check
direnv exec . make rust-test
```

Rust 2024, minimum Rust 1.87 (required by Aya 0.14). Build output and the lockfile belong to this
workspace. `make rust-build`, `rust-test`, `rust-fmt`, `rust-lint`, and `rust-doc`
all select it explicitly. `rust-fmt` checks formatting; to apply formatting,
run `make rust-fmt-fix`. `make rust-lock` refreshes the new lockfile.
Development conventions live in [AGENTS.md](AGENTS.md).

If `.envrc` selects the Go-oriented `.#static` shell, clear its extra linker
flags for Rust commands only:

```sh
direnv exec . env NIX_LDFLAGS= make rust-check rust-build
```

That shell adds static glibc to the library search path, which can produce
crashing executables when used for ordinary dynamic Rust builds. Leave `.envrc`
unchanged; SQLite still uses rusqlite's bundled library.

## Implemented crate registry

| Crate | Tier | Responsibility |
| --- | --- | --- |
| `bpfman-model` | 0 | Pure program specifications, stored records, kernel observations, and bounded XDP configuration |
| `bpfman-core` | 1 | Pure listing/store policy, load/link compensation, unload continuations, and XDP replacement planning |
| `bpfman-lock` | 1 | Go-compatible writer lock and borrowed mutation capabilities |
| `bpfman-kernel` | 3 | Backend-independent observation and lifecycle capabilities with opaque ownership |
| `bpfman-kernel-aya` | 4 | Aya loading, Linux observations, attachment handles, and private BPF syscall boundaries |
| `bpfman-fs` | 2 | Runtime authority, bpffs preparation, owned pins, and bytecode publication/removal |
| `bpfman-store` | 3 | Backend-independent read, commit, and conditional teardown contracts |
| `bpfman-store-sqlite` | 4 | Go-compatible creation, queries, and atomic program/map-set persistence and conditional teardown |
| `bpfman-store-json` | 4 | Versioned whole-file snapshots, atomic publication, and conditional teardown |
| `bpfman-runtime` | 4 | Generic tracepoint/XDP lifecycle interpreters, compensation, and observations |
| `bpfman` | 5 | Typed program/link CLI, load/get/list/unload and attach/detach dispatch, and presentation |

The model and core library targets are `no_std`. Workspace tests enforce that normal edges
point down through tiers, pure normal dependency closures are explicitly
reviewed, and local dependencies (including build, dev, optional, and
target-specific declarations) stay inside `rust/`. Pure crate features require
review too. Architecture tests may use the standard library and JSON decoding;
those development dependencies do not enter the shipped core.

`ProgramType` is a data-free discriminator for parsing and filtering.
`ProgramSpec` is the payload-bearing domain selection now shared by CLI parsing
and load policy: Fentry/Fexit require a target and LSM requires a hook. Its kind
is derived from the variant, never stored as a second discriminator. `Symbol`
validates names but is explicitly **not** filesystem path authority. Native
source paths, image credentials, presentation options and Clap remain outside
the pure model.

The pure crates have no external normal dependencies. Their small validation
errors implement `core::fmt::Display` and `core::error::Error` directly. Aya's
object parser enables `thiserror/std`; using that same dependency in the model
would let Cargo feature unification introduce std into its dependency closure.
The architecture gate therefore admits no external pure dependencies. Adapter
errors still use `thiserror`, with `anyhow` only at the binary boundary.

The architecture tests inspect the resolved dependency graph. They do not
prove arbitrary dependency code is free of I/O: admission to the pure closure
requires source review. `no_std` additionally keeps ordinary standard-library
I/O APIs out of these libraries.

## Application API

`bpfman-runtime::Bpfman<S>` is an instance of bpfman bound to its store and runtime.
The CLI and library consumers use the same operations:

```rust,ignore
let store = ActiveStore::open(backend, &layout, lock_timeout)?;
let bpfman = Bpfman::new(store, lock_timeout);
let programs = bpfman.list(&filter)?;
let observed = bpfman.get(id)?;
let loaded = bpfman.load(request)?;
let report = bpfman.unload(id)?;
```

Prepare a local ELF with `PreparedProgram::new` and a typed `ProgramSpec`
before startup to reject bad input without creating runtime state. The request owns validated bytes and has
no runtime path; the instance supplies its adopted runtime for execution.
`list` returns summaries without kernel privileges; `list_entries` adds live
kernel observations. Read methods take `&self` and no lock parameter. Mutations
and explicit cleanup retries acquire the configured writer lock internally.
Opened reader handles are thread safe; a shared instance supports concurrent
readers when its backend is also thread safe.

Dependencies, interpreter modules, compensation drivers, and request fields
are private. No store getter or public operation accepting a separate runtime
is exposed. Compile-fail examples check construction and field visibility.
Backend contracts remain available to adapter implementers, while application
callers use typed requests, domain results, and opaque errors/reports.

`retry_unload`, `retry_unload_cleanup`, and `retry_load_cleanup` perform one
explicit pass over retained work. Lock acquisition failure preserves receipts
and previous effect history. Unload returns the retained report in its error;
load retains its original error and exposes the acquisition failure through
`retry_lock_error`. The application never automatically retries effects.

The crate owns no CLI formatting or telemetry subscriber. This is a reusable
library boundary today; packaging and publishing remain a separate step.

## Store backend boundary

The CLI composition root selects SQLite (the default) or JSON with `--store`
or `BPFMAN_STORE`, opens a runtime-bound `ActiveStore`, and moves it into `Bpfman`.
Shared behavioural tests construct the same application instance in setup.
Runtime depends
on `bpfman-store`, with no direct or transitive dependency on either backend.
Architecture tests enforce this separation. The contract
crate defines small, statically dispatched interfaces:

- `OpenStore` opens existing state for independent readers without the writer
  lock, or initializes missing state with writer authority. Only absence permits
  initialization; format checks remain inside each backend.
- `ProgramReader` returns stored summaries or complete domain records from a
  consistent snapshot, without exposing serialized data or queries.
- `CommitLoad` atomically publishes a nonempty program batch and every
  private map-set membership (an empty batch is a no-op). An error means no commit, so compensation remains safe.
- `UnloadStore` validates ownership and conditionally deletes records and map
  sets using opaque, non-cloneable backend receipts. Failed deletion returns the
  receipt for an explicit later pass.
- `LinkStore` allocates pending intent, finalises attachment, and conditionally
  deletes unchanged records using owned receipts.
- `LinkReader` reads stored link intent in a consistent snapshot, including
  pending records needed for recovery.
- `XdpStore` atomically publishes a first-member dispatcher snapshot and
  conditionally deletes it using owned evidence; `XdpReader` reads that complete
  snapshot without the writer lock.

There are no connection types, schema-version fields, or transaction callbacks
in these contracts. SQLite and JSON implement the same operations. Tests use these real stores;
fault decorators intercept selected operations without implementing persistence.

`ActiveStore` retains the handle returned by startup and clones it for each
caller without reopening the backend. Clones share backend resources, not a
transaction or frozen snapshot. SQLite retains one idle read connection; a
concurrent read opens a separate connection if that slot is occupied. Its mutex
protects only checkout/return, never query execution. Sequential reads reuse the
connection, including after a failed read. JSON shares the opened directory
handle and reads the currently published snapshot on each call. Mutation
preflight revalidates the retained handle, and reads check compatibility inside
their own snapshots.

SQLite's private query functions use `prepare_cached`, named parameter bindings,
and typed persistence rows decoded by column name. All parameters are rebound on
each call. Statements return to their connection's cache after use, so the
retained reader also retains prepared statements without holding a transaction or
snapshot. Writes remain scoped to separate connections and explicit transactions.
Rust checks the query function inputs and result fields; SQL/schema compatibility
and stored column types are checked at runtime, with domain validation afterwards.
Creation still executes the authoritative Go schema DDL. Neither SQL nor query
row types appear in the generic store contracts or behavioural tests.

JSON version 4 adds complete single-member XDP dispatcher snapshots to private
tracepoint/XDP programs and standalone tracepoint links. Versions 1–3 retain
their existing operations: tracepoint programs from version 1, tracepoint links
from version 2, and XDP loads from version 3. XDP attachment requires a separately
initialized version 4 or newer runtime. Version 5 adds multi-member XDP
replacement. New stores use version 6, which also persists explicit network
namespace paths. Versions 4 and 5 reject namespaced attachments without upgrading;
version 4 also rejects replacement. Existing stores never upgrade implicitly.
The filesystem adapter writes the pending snapshot beneath a verified directory
descriptor and atomically renames it into place under the runtime writer lock.
Failed publication leaves the old state intact; interrupted staging files are
reused after validation. This is runtime state under `/run`; power-loss recovery
is outside the contract. JSON format/version checks and generation-based deletion
evidence remain inside the JSON adapter. Unknown fields and invalid relationships
are rejected without repair.

`LinkStore` and `LinkReader` support attachment lifecycle operations: allocate
pending intent and its canonical pin path, finalise with the kernel link ID,
observe, and conditionally delete using opaque receipts. The shared contract
suite exercises both backends through an injected `ActiveStore`, including failed
finalisation, stale receipts, lock-free reads, and program deletion blocked by
pending or finalised links. SQLite retains Go's schema version 2. The runtime
enforces release of the managed attachment reference before consuming a
link-deletion receipt. Tracepoint unload observes pending and finalised links before
mutation and removes their pins and records under the same writer lock as program
teardown.
Pending intent is cleaned using its canonical pin path; a missing kernel ID does
not prevent cleanup. A present pin must still match the program and link type.

Both formats occupy `<runtime>/db/store.db`. The filename is historical; selecting
another backend refuses the existing incompatible contents rather than creating
a second inventory. There is no implicit conversion. Use a separate runtime to
try JSON alongside an existing SQLite runtime:

```sh
sudo rust/target/debug/bpfman --store json --runtime-dir /run/bpfman-json program list
```

Keep the same store selection for later commands against that runtime.

Shared store errors expose portable categories and preserve private diagnostic
sources. `UnloadReport<S>` and `UnloadError<S>` retain the selected backend's
receipt types; explicit retries use the instance's backend and acquire writer
authority internally. Backends must reject foreign or stale evidence.
Failed-load cleanup needs only filesystem receipts and never reopens or retries
the store.

Tests use both real stores through production read/unload operations and
the production load interpreter with fake kernel/filesystem effects. They check
open failures before acquisition, atomic commit failure with independent cleanup
failures, successful commit without compensation, retained receipts, unchanged
faults, and wrong-runtime rejection. Existing SQLite, exhaustive
compensation, and real-kernel compatibility gates remain in place.

Generic live-kernel tests inject failures at store operations through a test-only
`Faults<S>` decorator. They call public runtime operations with real kernel and
filesystem effects; no production failure flags or persistence edits are needed.
The same `lifecycle::exercise<S>` scenarios run against both SQLite and JSON.
CLI and unchanged DSL tests inspect returned JSON and runtime artifacts, with no
database queries. `make rust-test` and `make rust-check` build the fixtures and
run all kernel and unchanged DSL tests as part of the normal gate. Cargo's runner
uses passwordless `sudo -n` to execute the kernel test binary serially in a
private mount namespace; other test binaries run as the invoking user. Missing
privileges fail the gate. The focused kernel and observation targets select
subsets of the same suite. The harness and executable-selection tests are Rust
integration tests.

SQL fixtures and DDL/DML remain in tests explicitly testing SQLite. Adapter tests
verify transaction rollback, constraints, orphan map-set absence, invalid records,
and conditional deletion. `tests/sqlite_compatibility.rs` checks the selected
SQLite CLI against Go's schema. These format-specific assertions do not belong in
the generic lifecycle tests. Sharing persisted state with Go is checked separately
in `tests/kernel/go_compatibility.rs`; JSON does not need to share
Go's SQLite representation. The division preserves the previous coverage:

| Scenario | Coverage |
| --- | --- |
| Commit, deletion, GC, and post-commit read failures with live programs | Generic `tests/kernel/lifecycle.rs`, faulting store operations |
| Foreign pin/map identity, failed output delivery | `tests/kernel/cli.rs`, public CLI and artifact observations |
| Go/Rust shared-state observations and unload in both directions | SQLite-specific `tests/kernel/go_compatibility.rs` |
| Unsupported stored relationships, noncanonical stored paths, partial writes | SQLite adapter tests using the actual Go schema |
| CLI tracepoint lifecycle | Unchanged `TestTracepoint_LoadAndGet.bpfman` and `TestTracepoint_LinkRoundTrip.bpfman`, plus `TestTracepoint_UnloadAttached.bpfman`, each with SQLite and JSON |
| Typed, raw, and nested-shell executable selection | `tests/e2e_selection.rs`, unprivileged Make recipe tests |

Both persistent backends run the same generic lifecycle, CLI, unchanged DSL,
and unprivileged store-contract scenarios, changing only backend selection in
test setup. JSON adds focused persistence tests for malformed state, publication
failure, invalid relationships, and stale generations. It has no separate
behavioural suite.

`tests/concurrent_store.rs` exercises both backends with four independent readers
opening and reading while a writer holds the runtime lock and publishes successive
commits. Each read sees one complete committed snapshot; separate calls may see
different generations. Concurrent first starts recheck absence under the writer
lock. SQLite uses WAL; JSON readers retain an opened inode even if publication
unlinks it before validation. Staging validation still requires exactly one link.

## Telemetry

Effectful crates emit `tracing` spans; the CLI owns collection. Set `RUST_LOG` to
enable structured JSON events on stderr. Command output remains on stdout.
Telemetry is disabled by default. Filters can select a crate or individual module:

```sh
RUST_LOG=bpfman_runtime=debug,bpfman_store_json=debug,bpfman_fs::snapshot=trace,bpfman_lock=trace \
  rust/target/debug/bpfman --store json --runtime-dir /tmp/bpfman-observe \
  --trace-file /tmp/bpfman-startup.json program list -q
```

The optional timeline file opens in Perfetto or Chrome tracing and uses the same
filter. `--trace-file` enables debug spans when `RUST_LOG` is unset; lock spans are
trace level and need `bpfman_lock=trace`. Existing output files are refused. Each
file covers one CLI process. Trace guards flush on normal and error exits.

Targets include `bpfman_runtime`, `bpfman_store_json`, `bpfman_store_sqlite`,
`bpfman_fs::snapshot`, and `bpfman_lock`. Operation spans cover store opening,
creation, snapshots, commits, deletion, publication, and program operations.
`lock.wait` measures acquisition (including timeout/cancellation); `lock.held`
covers the current callback and closing its descriptor. Inherited descriptors may
retain the underlying flock after that scope. Span-close events report busy/idle
time, and acquisition events include `wait_us`. SQLite's `store.connection.open`
span records actual read-connection opening; `reader acquired` events include
`reused` so connection reuse is visible. No record contents or credentials
are logged. Model/core crates remain uninstrumented and pure.

## CLI parity harness

The production Go CLI remains the default under test. Build the new CLI with
`make rust-build`. Go can load programs into a runtime, then Rust can list the
managed records without kernel observation (with access to the runtime files):

```sh
rust/target/debug/bpfman --runtime-dir /run/bpfman program list
rust/target/debug/bpfman program list -q --type xdp,tcx --application demo
```

The runtime defaults to `/run/bpfman` and accepts `BPFMAN_RUNTIME_DIR`.
Clap constructs a validated `RuntimeLayout` from the native OS path. The layout
owns the default root and the `root()`, `lock_path()`, and `database_path()`
accessors; runtime operations take `&RuntimeLayout`, not an arbitrary path.
Construction rejects empty/relative roots and filesystem-root aliases, and normalises paths lexically like
Go, preserving non-UTF-8 names without filesystem I/O or symlink resolution.
It describes locations, not proof that runtime setup has happened.
`--type` accepts repeated, comma-separated, case-insensitive types;
`--program-type` and `-p` are aliases. After request and ELF validation, startup
opens the selected active store. Existing state needs no runtime lock. Only
missing state takes `<runtime>/.lock`, rechecks absence, and initializes the store
before releasing the lock. SQLite creates `<runtime>/db/store.db` at Go schema
version 2 in WAL mode. Its initialization path uses the core's
`plan_store_open` decision inside the adapter, returning an opened read handle.
Runtime orchestration never receives a schema version or database path.
`StoreObservation<T>` and `StoreOpenPlan<T>` retain opaque interpreter-owned
evidence: `UseExisting` carries the opened store, so it cannot disagree with a
separate optional handle. Rejection also returns the evidence, keeping resource
cleanup in the interpreter. Policy neither inspects nor drops that evidence;
the core has no filesystem or SQLite dependencies and performs no I/O.
The adapter's `create_if_missing` requires a borrowed `RuntimeWriter` and derives
its target from that writer; an unrelated lock cannot authorise a supplied path.
Creation builds a complete temporary database and publishes it without
overwriting existing state. New database files are owner-readable/writable.
Existing databases are never repaired or migrated. Subsequent reads check the
schema in a read-only transaction without the runtime writer lock. Production
creation and test fixtures embed the Go migration SQL with `include_str!`.

Direct file/directory removal calls are denied by the lint gate outside the
private `bpfman-fs::removal` module. Its operations consume typed ownership
receipts and require the runtime writer. It performs no recursive deletion. `RuntimeDirectory::open_or_create` opens/creates the
root without acquiring the lock. Its `with_writer` method acquires the lock,
checks that the lock inode has not changed, prepares the database directory,
then lends a non-constructible, non-cloneable `RuntimeWriter` to the callback.
That authority cannot escape the callback. Nested operations borrow the same
writer rather than reacquiring it.

Directory and lock operations use Linux `openat2`, reject symlink traversal,
and forbid unintended mount crossings below the adopted root. Load preparation
explicitly adopts or mounts the `fs` bpffs child; subsequent operations cannot
cross another mount below it. Kernels without `openat2`
fail closed. Runtime adoption may cross mounts such as `/run`. Lock entries must
be singly linked regular files. Tests cover symlinked roots/ancestors, lock and
database-directory replacement, stale lock descriptors, and cross-process flock
contention. Participants must keep the lock inode stable during callbacks.
This is not proof that bpffs is mounted or that managed objects are ready.

SQLite remains a separate boundary: rusqlite owns database and auxiliary-file
I/O. Its pathname handoff is not protected against external filesystem
replacement by our directory descriptor. There is no custom VFS, and managed
object cleanup must never manipulate SQLite's journal, WAL, or shared-memory files.

`--lock-timeout` / `BPFMAN_LOCK_TIMEOUT` accepts durations such as `30s` or
`500ms`; the default is 30 seconds and `0` waits indefinitely. It bounds only
acquisition, never work under the lock. The lock adapter supports cooperative
cancellation and owned inherited descriptors. CLI signal cancellation is wired;
the planned uprobe mount-namespace subprocess helper is not yet implemented.
No privileges are needed for text/quiet listing in a writable temporary runtime.
Loading requires BPF and mount privileges; `/run/bpfman` will normally require sudo.

Listing supports managed table, quiet-ID and JSON output. Text and quiet output
use stored summaries without kernel privileges; JSON adds full records and live
kernel observations. `program get ID [-o text|json]` observes one managed program,
including its maps, statistics, and links. Link observations use the same
record/status shape as `link get` and do not take the writer lock. `--all` and
kernel link state filters remain unsupported. Unload supports a tracepoint with private maps and pending or finalised
standalone links, or an unattached XDP extension with private maps.

`program load file PATH` and `program load image IMAGE` parse typed requests,
including repeated/comma-separated `--programs`, metadata, globals, application,
nonzero map-owner IDs, text/JSON output requests, and image-specific pull/auth
options. Fentry/fexit/LSM variants carry required load-time targets. Invalid input
exits with status 2. Local tracepoint and XDP batches with private maps and
metadata/application labels are executable, with Go's detailed text output or
JSON load envelope in selection order. Image loads, other program types,
and map-owner sharing exit with status 1 before source access or runtime effects.
ELF-level unsupported PinByName maps and section/type mismatches are rejected
before runtime setup. Credentials are not echoed in auth diagnostics or help.
Unlike Go, this parser requires the explicit file/image verb and rejects duplicate
ELF selections, extraneous load-time targets, and zero map-owner IDs.

The unchanged `e2e/scripts/TestTracepoint_LoadAndGet.bpfman`,
`TestTracepoint_LinkRoundTrip.bpfman`, `TestTracepoint_UnloadAttached.bpfman`,
and `TestMultiProgTracepoint_LoadAttachDetachUnload.bpfman` run through the Go shell runner,
once per store. The gate builds the required executables and kernel module,
loads the module if absent, and uses temporary runtimes in a private mount
namespace. It selects local bytecode and verifies that unload leaves no owned
residue:

```sh
direnv exec . make rust-test-observation
```

This needs the same privileged kernel environment and bytecode fixtures as
`rust-test-kernel-load`. The Go DSL fixture uses external linking automatically,
including in the Nix development shell. The full corpus still includes
unsupported operations; passing these scripts does not establish full parity.
For manual selection with prebuilt binaries:

```sh
make run-e2e-scripts BPFMAN_UNDER_TEST="$PWD/rust/target/debug/bpfman" \
    TEST='TestBPFManScripts/scripts/TestTracepoint_LoadAndGet[.]bpfman$'
```

This run-only target requires prebuilt fixtures and an appropriate privileged
test environment. It sets both `BPFMAN_BIN` and `PATH` inside `sudo` so typed
DSL commands, raw `exec bpfman`, and nested shells agree. The selected executable
must be named `bpfman`. Keep Go and Rust suite runs separate on a clean runtime.

`make test-e2e-selection` tests this wiring without sudo or kernel effects,
using fixture executables and the real Make recipe. CLI process tests also
exercise the real Rust executable. The kernel gate compares Go and Rust get
text/JSON and list JSON for the same live tracepoint.

## Program observations

Full store decoding, kernel reads, map-pin correlation, and selection require
no runtime writer lock. The store returns typed domain values from a consistent
snapshot, preserving nullable timestamps and nil versus empty
global byte slices. Timestamp output matches Go's RFC3339Nano formatting,
trimming trailing fractional zeros without reducing precision. CLI conversion
owns JSON field names and the different load/get/list envelopes; the pure model
has no serialization or backend dependencies.

The `bpfman-kernel-aya` adapter supplies fields unavailable through Aya's public
metadata API. Its private syscall module is the only unsafe exception: it
supports read-only descriptor lookup and metadata queries, bounds its buffers,
and owns descriptors until observation completes. Generated Aya ABI structs
never cross its public interface. Because Cargo cannot override an inherited
`forbid`, this crate mirrors workspace lints with `unsafe_code = "deny"` and
allows unsafe only in that module; an architecture test checks the other gates
remain identical.

Absent kernel objects are distinct from denied observations. Get reports a
program recorded in the store snapshot but absent during the later kernel
observation; list JSON uses a null kernel field for that case. Concurrent unload
can cause this observation, so it does not itself prove inconsistency. Permission and other program lookup
errors fail the command. Individual unreadable maps are omitted, as in Go,
while the program's observed map IDs remain intact. Available zero counters are
not null. Load omits statistics and map-pin presence observations; get includes
them. Map pins are correlated by kernel map ID, avoiding ambiguous matches when
names share a truncated prefix. Pins are inspected through opened descriptors;
pins removed before opening are omitted. Runtime paths alone do not assert
presence. Store, kernel, and pin observations are not one atomic snapshot.

After successful load persistence, observation and output failures cannot roll
back committed state. An observation failure reports the program ID and that it
remains loaded. Kernel tests inject a store read failure after commit
and use a failing output destination to check this boundary, alongside the existing
pre-commit compensation cases. No observation retry runs automatically.

## Single-program load policy and execution

The first load-policy increment follows Go's `manager/load.go`: load/pin,
publish bytecode, then atomically persist the program and map-set membership.
There is no pending database row and no attachment in this operation.

`LoadProgram` yields `PublishBytecode` only after reported kernel success;
publication yields `PersistProgram`. Each transition consumes its continuation.
Private fields and compile-fail tests prevent forging, reusing, or advancing a
continuation out of order. These are policy types, **not** substitutes for the
runtime writer capability or proof the interpreter actually performed I/O.

Rollback is a small instruction set, not a generic VM: `RemoveBytecode`,
`RemoveProgramPin`, and `RemoveMapPin`. Instructions carry opaque ownership
receipts, not paths or integer IDs authorising deletion. Each map pin is a
separate instruction. Bytecode cleanup handles owned staging as well as published
artifacts. A partial forward failure returns its original cause and unresolved
acquisitions in `EffectFailure`, so cleanup is not just an implicit adapter
promise. Borrowed/shared pins never produce owned cleanup receipts.

`LoadRollback::next` yields one instruction and a consuming continuation whose
failure type accepts only that resource's receipt type. Success consumes the
receipt in the adapter; failure returns it. Either result resumes the pass.
Failed instructions are accumulated separately, never retried inline. Bytecode
artifacts are removed first, then the program pin, then map pins in reverse
acquisition order. These operations must be independently safe; dependent
directory cleanup and shared-reference release are not in this initial vocabulary.

The terminal `LoadFailure` retains the original error, the complete attempt
history, and only unresolved receipts. Instruction IDs are local to the load
and stable across retries, linking failures to remaining work. `retry()` starts
an explicit new pass over unresolved work only; earlier errors remain in the
history even after recovery. No receipt or error is cloned or dropped by policy.
Persistence must report failure only when the transaction has not committed.
After a successful commit, output-delivery failure must not roll back the load.

The private `LoadCleanup` trait is the narrow, injectable filesystem-effects
interface. `compensate_load` drives the instructions through it, with a
`RuntimeWriter` required both by the driver and every mutating method. The loop
does not propagate cleanup errors early: it attempts every instruction in the
pass even when every cleanup fails. The real implementation delegates to
`bpfman-fs`, which checks receipt/root identity, reopens parents without following
symlinks, and compares parent and artifact inodes. Raw unlink/remove operations
remain forbidden outside the private filesystem boundary. SQLite auxiliary
files remain rusqlite's responsibility.

The runtime tests exercise the production compensation driver with an injected
stateful fake under real writer scopes. They cover all 32 combinations of failure
across five instructions, partial forward work, repeated total failure, retry
history, wrong-runtime refusal, preservation of unrelated/shared state, and
non-cloneable receipt/error drop counts. Real-filesystem confinement tests remain
in `bpfman-fs`; the fake cannot prove confinement or kernel lifetime semantics.
Crash recovery and cancellation budgets are not yet implemented for loading. A hanging/panicking adapter or process termination can
prevent progress; Rust cannot prevent dropping/forgetting a continuation.
`must_use`, compile-fail tests and interpreter tests complement type-level ordering.

The load interpreter also has a private `LoadEffects` substitution boundary.
The CLI and unit tests call the same forward orchestration and compensation
finaliser; the fake does not implement a parallel load plan. Individual effects
cover store opening, filesystem preparation, kernel loading, program pinning,
map-directory creation, each map pin, publication, persistence, and cleanup.
Every mutating effect borrows the scoped runtime writer. These are internal
interfaces, with no fault-injection flags or environment hooks in the CLI.

The interpreter tests cross 19 forward failure points with all 64 subsets of
six cleanup failures (1,216 scenarios). Failures before and after acquisition
exercise partial program/map pins, map directories, staging, publication, and
uncommitted persistence. Assertions check exact effect order, preserved
unrelated/shared state, resource residue against unresolved receipts, no visible
partial database state, stable instruction IDs, and exactly-once receipt/error
drops. Explicit retries attempt unresolved work only and preserve all earlier
outcomes, including successes. Separate tests cover repeated total failure,
wrong-runtime retries, empty map sets, successful commit with hostile cleanup,
and blocked map-directory removal followed by failing and successful retries.
Map-directory attempt history now records successes as well as failures;
blocked work is retained without recording an attempt that never happened.

Run these tests and a readable failure/retry trace without root:

```sh
direnv exec . make rust-test-load-compensation
```

The trace injects a pre-commit load failure plus failed program-pin and second
map-pin removals. Bytecode and the other map pins are cleaned up, the map
directory stays blocked, and an explicit second pass removes only the retained
pins and directory. The fake tests prove orchestration contracts; filesystem
confinement and actual kernel lifetime remain separate adapter/kernel gates.

The executable slice reads a regular ELF once, validates its selected section
and maps, then uses those same bytes for Aya loading and bytecode publication.
It holds one Go-compatible writer scope through runtime preparation, program
and map pinning, publication, and store commit. Pins use `/proc/self/fd` paths
anchored at verified directory descriptors because Aya's pin API accepts a
pathname; callers never supply a deletion path. Each selected program is loaded
with private maps. Only non-internal maps referenced by that loaded program
are pinned under `fs/maps/{program_id}`; kernel map IDs establish membership,
without relying on truncated kernel names. Internal data maps remain
kernel-owned and are not pinned separately. Loading does not attach programs.

Bytecode and provenance are written into an exclusively created staging
directory, then published to `programs/{id}` with `RENAME_NOREPLACE`. Partial
publication returns its staged ownership. Cleanup attempts each owned file and
removes the directory only after all its files have been removed. The runtime
likewise retains the owned map-directory receipt separately and attempts its
removal only when no map-pin instructions remain unresolved. Unexpected
children prevent directory removal. `Bpfman::retry_load_cleanup` performs one
explicit pass and retains the original cause and all cleanup history.

These checks enforce descriptor-based confinement and refuse observed
replacement; they rely on cooperating writers holding `.lock`. They are not a
sandbox against privileged processes renaming entries between the final check
and a syscall. Collection directories, the lock, database bootstrap, and a newly
mounted bpffs may remain after a failed load. Crash recovery is separate work.
Non-UTF-8 source/runtime paths are rejected before load effects because this
slice persists paths as SQLite text; read-only listing still accepts native paths.

SQLite creates every selected program and private map set in one transaction;
JSON publishes one complete snapshot. Records retain Go's source path, license,
metadata, UTC creation time, and null update time.
Existing rows are not overwritten. On commit, ownership transfers to stored
state; failed output delivery never compensates the successful load.

For multiple selections, call `PreparedProgram::with_additional_programs`
with the remaining `ProgramSpec` selections, then `Bpfman::load_batch`. The prepared batch keeps
one captured ELF and validates every symbol before runtime initialization.
Each member passes through the same forward interpreter as a single load.
Kernel/filesystem work finishes for all members before the single store commit.
A later failure compensates the current member and all earlier members in
reverse order, attempting every independent cleanup even when another fails.
The original cause, every cleanup attempt, and all unresolved receipts survive
in `LoadError`; `retry_load_cleanup` retries only outstanding work. Cancellation
before commit uses this same path. Post-commit observation failure names the
committed IDs and output failure leaves the entire batch loaded.

Shared store tests reject later collisions and duplicate IDs without publishing
earlier rows or map sets. Batch fault tests cross later forward failures with
earlier cleanup failures and cover cancellation and explicit retry. Kernel tests
cover second-program verifier rejection, batch commit failure, retained cleanup,
private map ownership, CLI ordering, and post-commit observation/output failure.
The unchanged three-program tracepoint DSL tests staggered detach and counter
execution against both SQLite and JSON.

Build and run the focused real-kernel acceptance gate with:

```sh
direnv exec . make rust-test-kernel-load
```

It builds the local fixtures and both CLIs, then runs in a private mount namespace
with an isolated temporary runtime. It proves Rust-created pins outlive the
loader, Go can observe the live program, and Rust list/unload removes its owned
state. It also covers Go-created tracepoints, unload refusal before mutation,
injected record-deletion and map-set-GC failures, preservation of unrelated
programs, failed-load cleanup, and output-delivery failure after load commit.
Unsupported ELF inputs are checked before runtime creation. This gate does
not claim full CLI or unchanged-DSL parity.

For a manual load:

```sh
sudo rust/target/debug/bpfman program load file \
  e2e/testdata/bpf/tracepoint_counter.bpf.o \
  --programs tracepoint:tracepoint_kill_recorder --application rust-slice
```

`-g NAME=HEX` overrides globals after checking their names and byte lengths
against the captured ELF, before creating runtime state. Aya applies the bytes
when loading; both stores retain the overrides for later program observations.
The stored ELF remains the original input. Empty JSON-store overrides are
omitted, preserving existing snapshots without a version change.

## Tracepoint attachment and detachment

`Bpfman::attach_tracepoint` takes a `TracepointAttach` containing a managed
program ID, validated `group/name` target, and metadata. Under one writer scope,
it adopts the existing program pin, commits pending intent, attaches the kernel
program, pins the link, and finalises the record with its kernel link ID. Loading
a program alone still does not attach it. The current Aya tracepoint adapter
opens its perf event on CPU 0, matching the current Go adapter.

Before finalisation, failure or cancellation compensates the acquired resources.
An unpinned link owns a live descriptor; a pinned link owns a filesystem receipt.
Cleanup releases that reference before deleting the pending record. Failed
release or unpin retains both receipts and blocks record deletion without
inventing an attempt. Reports preserve the original failure and every successful
or failed cleanup attempt. `retry_link_cleanup` performs one explicit pass over
unresolved work; cancelled lock admission preserves the report. An in-flight
successful finalisation wins over cancellation.

`Bpfman::detach` observes the stored identity and pin under writer authority,
then removes the pin and conditionally deletes the record, keeping the program
loaded. It rejects a replacement link even when it belongs to the same program.
Once destructive teardown starts, cancellation does not interrupt that pass.
Removing our pin releases our managed reference; unrelated external link
descriptors may keep the kernel attachment alive. `list_link_records` reads a
fresh store snapshot without the writer lock and includes pending intent; it
does not claim current kernel presence.

The same real-kernel scenario runs against SQLite and JSON. It proves counter
execution before detach and quiescence afterwards, replacement refusal, failed
finalisation compensation, blocked unpinning, explicit retries with preserved
history, and cancellation at store boundaries. Private interpreter tests cover
individual forward and cleanup failures, including live-descriptor release.
The CLI exposes `link attach tracepoint PROGRAM_ID GROUP/NAME [-m KEY=VALUE]`,
`link get LINK_ID`, `link list`, and `link detach LINK_ID`. Attach/get/list accept
`-o json`; JSON follows Go's record/status shapes. Other attachment kinds except the XDP slice below, list
filters, and batch detachment are not implemented. `get_link` observes recorded
kernel identity and a descriptor-confined pin without the writer lock; missing
kernel state is reported as absence, while denied or inconsistent observation
fails. Output failure after finalisation leaves the committed link manageable.
Program unload cleans pending and finalised standalone links before touching
program resources.


## Program unload

```sh
sudo rust/target/debug/bpfman program unload PROGRAM_ID
direnv exec . make rust-test-unload
```

This slice accepts a managed tracepoint with pending or finalised standalone
links, or an XDP extension with or without dispatcher links. Each must have its
own map set, no other map-set users, and no shared-map-pin registrations.
Other program types, shared state, multiple operands, and `--ignore-missing` are
explicitly unsupported. A missing managed record returns an error without
inspecting or adopting a kernel-only program or creating a database. It does not
yet make Go's additional not-managed versus not-found distinction.

Under one writer scope, store preflight validates canonical artifact paths and
exclusive ownership. Filesystem observation opens existing objects without
creating or mounting collections. It verifies descriptor confinement, types,
hard-link counts and inode identities; a live program pin must match the ID and
tracepoint or extension type, and its map pins must refer to that program's maps. If the
program pin is already absent after a partial unload, the validated private
map-set record authorizes inspection of its remaining BPF map pins. Bytecode
adoption accepts only `bytecode.o` and `provenance.json`. Unknown children and
unsafe paths fail preflight before any managed-object removal. Ordinary missing
artifacts are treated as already absent.

XDP unload first removes all of the program's dispatcher memberships under the
same writer lock, rebuilding surviving members or synchronously detaching the
last member. All dispatcher and program artifacts are preflighted before effects.
Program teardown remains deferred until dispatcher work, including old-revision
retirement, succeeds. `UnloadReport::xdp_attempts()` preserves nested detach,
restoration, and cleanup history; `attempts()` records the subsequent program
and standalone-link effects.

A failed restoration retains both revisions and blocks program teardown. A
successful rollback recovery ends that pass; a separate explicit retry may
resume the forward detach. Earlier completed detachments are never recreated.
Retries validate retained managed-program evidence and logical member identity;
the canonical program pin must remain present while dispatcher work is pending.
Foreign runtimes/kernel instances and moved or replaced pins are refused. New
links added while recovery is retained block program teardown until explicitly
handled.

After dispatcher prerequisites, unload is forward teardown of committed state,
with its own pure continuations:

1. For each link, remove its pin and then delete its record. A failure stops
   later links and all program teardown; successful detachments remain complete.
2. Remove the program pin. Failure stops all later effects.
3. Delete the managed record. Failure is returned, but independent bytecode
   cleanup still runs; maps and their map-set row remain untouched.
4. After record deletion, remove each private map pin once, continuing after
   individual failures. Remove the container only after all pins succeed, then
   delete the unused map-set row only after the container is gone.
5. Attempt bytecode cleanup independently of the record/map cleanup outcomes.

As in Go, post-record map/bytecode cleanup failures are warnings: the program is
unloaded even if GC leaves residue. Store deletions revalidate evidence in atomic
transactions and require the same opened runtime identity. Failed effects retain
non-cloneable receipts. Reports preserve successful and failed attempts with
stable instruction IDs; work skipped because a prerequisite failed is retained
without inventing an attempt.

Failed attachment requests compensate all resources acquired by that request;
failed cleanup retains the original error, receipts, and successful/failed history.
Unload is destructive forward teardown: successful detachments are not recreated
if a later effect fails. A failed prerequisite preserves dependent records and
blocks further teardown. Pending intent does not change these dependency rules.

No retry runs automatically. The library exposes an optional explicit pass over
retained receipts, including after successful unload with warnings. This does
not make failures transient: an unchanged failing condition still fails. The
caller must decide whether external conditions justify another attempt. The CLI
runs one pass and reports warnings/errors; it does not persist a recovery queue.
Repeating the CLI command can finish a partial unload while the program record
still exists. Once the record has gone, repeating by ID reports not found;
remaining GC residue requires separate repair, which this slice does not add.

The stateful fake enters the production unload interpreter and checks all 256
subsets of failure across eight individual effects, including exact order,
status, successful history, blocked work, residue, and unrelated resources.
Additional cases inject failures and cancellation at every link effect. The same
real-kernel scenario runs against both stores, failing the first or second link
record deletion, observing partial teardown, and retaining history through
cancelled and explicit retry passes. `TestTracepoint_UnloadAttached.bpfman`
exercises a live attachment and direct program unload through the shared DSL,
against Go and both Rust stores. SQLite interchange tests also compare attached
program text/JSON and unload programs loaded and attached by the other implementation.
The shared `tests/kernel/pending.rs` scenarios recover failed attachments through
unload, including a remaining live pin, intent whose kernel acquisition never ran,
and a successfully removed pin followed by failed record cleanup. Both stores
exercise cancellation, blocked teardown, and explicit retry history.
An unchanged-fault pass must make no progress; clearing the injected faults
allows only retained work to complete. Adapter tests cover store changes,
ignored/failed deletes, wrong runtime authority, replacement, symlinks, hard
links, FIFOs, and unexpected children. The kernel gate supplies the BPF identity
and lifetime checks that these fakes cannot establish.

## XDP extension load checkpoint

Local XDP loads use the same preparation, atomic batch commit, compensation,
observations, and unload interpreter as tracepoints. A selection is stored as
`xdp` while its kernel type is `extension`. The Aya adapter verifies it against
`prog0` of an unpinned one-slot XDP dispatcher, matching Go. The Makefile builds
the shared `dispatcher/bpf/xdp_dispatcher_v2.bpf.c` object for Rust compilation;
the independent workspace has no dependency on legacy Rust.

Both stores run the unchanged `TestXDP_LoadAndGet.bpfman` and
`TestLoad_NamedProgramSkipsBrokenSibling.bpfman`. Kernel tests also cover
`xdp.frags` section loads, temporary dispatcher release, later-member verifier
failure, commit failure, unload retry, and refusal of tracepoint attachment to
an XDP program. SQLite interchange tests load with either Go or Rust, compare
get/list observations, and unload with the other implementation.

## XDP attachment and replacement

`link attach xdp PROGRAM_ID INTERFACE --priority N [-m KEY=VALUE]` attaches
a member of a dispatcher in the current network namespace, or in the namespace
selected by `--netns /absolute/path`. Up to ten links may share an interface,
including multiple links for one program. `--proceed-on`
accepts comma-separated or repeated `aborted`, `drop`, `pass`, `tx`, `redirect`,
and `dispatcher_return`; the default is `pass,dispatcher_return`. The command
supports text and JSON output. `link get`, `link list`, and `program get` expose
stored XDP details and actual tracing-link observations. `dispatcher get xdp
NSID IFINDEX -o json` reads the complete stored snapshot without a writer lock.
`dispatcher list [--type xdp] [--nsid N] [--ifindex N] -o json` lists summaries
from one validated store snapshot, also without that lock. Zero or omitted
namespace/interface filters select all; only XDP and JSON output are supported.

The runtime resolves the interface, validates the managed EXT program, loads a
one-slot dispatcher, and pins its program, extension link, and outer interface
link using Go's path layout. It selects the interface's `xdp_mode` from
`/etc/bpfman/bpfman.toml`, or from `--config FILE` / `BPFMAN_CONFIG`; the default
is `drv`. Supported values are `drv`, `skb`, and `hw`. A failed `drv` or `hw`
first attach retries with `skb`; an `skb` attach is attempted once. Replacement
reuses the existing pinned outer link, so the configured mode only affects
creation of a new link. Configuration errors are reported before runtime state
is opened. A foreign occupied attach point is refused. Managed membership
changes use complete revision replacement; there is no netlink fallback.

For example, configure a hardware-first attach for one interface with:

```toml
[interfaces.enp1s0]
xdp_mode = "hw"
```

The mode choices and fallback follow the legacy Rust implementation. Current Go
has no per-interface mode configuration. Missing default configuration uses
`drv`; an explicitly selected missing file, an unreadable file, malformed TOML,
or an invalid mode for the selected interface fails before store initialization.
Only XDP attach reads this configuration; read-only commands do not need it.
Real-kernel tests use veth to verify native driver and SKB modes, hardware-request
fallback, packet counters, replacement/restoration, and either surviving member
on both stores. Run `direnv exec . make rust-test-xdp-modes` for that suite.
Actual hardware offload is unverified.

SQLite atomically publishes the dispatcher header, managed link, and member
using Go schema version 2. JSON supports that snapshot from version 4; newly
created JSON stores use version 6. Explicit namespaces require format 6.
A successful commit ends compensation, including when cancellation arrives late.

Last-member `link detach LINK_ID` synchronously detaches the outer link before releasing
its pin. This stops the interface attachment even if another observer holds a
link descriptor. Extension and dispatcher pins are then removed independently;
the empty revision directory and unchanged store snapshot follow only after
their prerequisites succeed. A missing outer pin with a still-live kernel link
is refused before teardown. Wrong identities, unexpected revision children,
and changed snapshots are also refused. All acquisitions and removals remain
beneath verified descriptors; the filesystem adapter's private syscall module
is the narrow unsafe boundary for link creation, inspection, pinning, and detach.

Failed forward effects retain their original cause and every cleanup attempt.
`retry_xdp_cleanup` runs one explicit pass over unresolved ownership. Cancellation
can stop admission or forward attachment before commit; an admitted cleanup pass
finishes without observing cancellation. The CLI performs one pass and reports
failure; it does not persist a recovery queue.

Both stores run unchanged `TestXDP_LinkRoundTrip.bpfman` and
`TestDispatcher_LifecycleAfterLastDetachXDP.bpfman`. The kernel suite also checks
commit failure, occupied attachment refusal, mismatched pins, retained outer
link descriptors, store deletion failure, cancelled retry, reattachment, and
residue-free teardown. Shared store tests cover atomic snapshots, stale receipts,
and foreign runtime authority; production-interpreter fakes cross acquisition,
cancellation, and individual cleanup failures.

Attached-program unload removes all XDP links through the same protocol.

### XDP multi-buffer packets

`xdp.frags` programs execute on native and generic SKB multi-buffer packets. Each
extension is verified against an unpinned dispatcher matching its ELF declaration.
Before first attach and each membership replacement, the Aya adapter reads the selected
program's published ELF through `RuntimeWriter::read_bytecode`. This read uses
the retained runtime descriptor, refuses symlinks, mount crossings, special files,
extra hard links, and inputs over 64 MiB. Missing or malformed bytecode fails the
operation; it does not silently mean no fragment support. Extension instructions
are reused from their pins, not reloaded from the ELF.

The pure `XdpConfig` encoder writes each member's `BPF_F_XDP_HAS_FRAGS` flag and
enables the dispatcher's fragment flag only when every member declares support,
following [libxdp's chain policy](https://github.com/xdp-project/xdp-tools/blob/master/lib/libxdp/libxdp.c).
Make builds linear and `xdp.frags` variants from the same dispatcher source;
the default shared object used by Go remains linear. Rust selects the appropriate
variant and Aya supplies its kernel load flag. No store/schema changes or new
recovery protocol are required. Mixed chains work at normal MTU; if the native
driver rejects a non-fragment dispatcher at jumbo MTU, replacement leaves the
old fragment-aware chain active.

Run `direnv exec . make rust-test-xdp-frags` for six tests on both stores: the
unchanged `TestLoad_XDPFragsProgram` normal-MTU script and shared veth acceptance
in separate sender/receiver namespaces with explicitly requested driver and SKB
modes. Interface observations assert the selected mode before traffic. The
multi-buffer tests send 8 KB ICMP payloads with IP fragmentation prohibited.
Probe counters prove total XDP length exceeds the
linear buffer, and `bpf_xdp_load_bytes` reads both the final payload byte and
across the linear/fragment boundary. Tests cover two-member execution, proceed-on
stopping, attach/detach publication rollback, incompatible jumbo membership,
mixed-chain ABI flags, re-enabling fragments, either survivor, stable outer
identity, last detach, and residue-free unload. These are XDP multi-buffer
packets, not IP fragments. This acceptance covers native and generic SKB veth
with PASS, including replacement, rollback, mixed membership, and either survivor.
Jumbo mixed-chain refusal is asserted for native veth; it is a driver constraint,
not a generic SKB requirement. Physical NICs remain unverified.

### Dispatcher replacement policy

The runtime uses the pure replacement policy for every nonempty membership change.
`plan_xdp_membership` assigns contiguous slots using Go's stable ordering:
priority, new before existing at equal priority, then program name. Exact ties
retain input order. It validates capacity and duplicate link identities, checks
revision overflow, and selects last-member removal without loading an empty
dispatcher. `XdpConfig` encodes every supported capacity using the existing ABI;
the current single-member loading path uses the same encoder.

`XdpReplacement` carries opaque old/staged revision ownership through switch,
publication, and restoration. Rejected switches permit staged cleanup; failures
after switching require successful restoration first. Failed restoration retains
both revisions, the original cause, and every attempt for explicit retry.
Successful publication returns old-revision cleanup ownership and committed new
ownership separately. `XdpCleanup` records stable instruction IDs so multiple
extension failures remain distinguishable across retries.

Run `direnv exec . make rust-test-xdp-core` for the policy, ABI, and compile-fail
tests; runtime and real-kernel acceptance are described below.

The persistence portion is also implemented. `XdpDispatcherReader` returns a
validated `XdpDispatcherSnapshot` containing one to ten members in contiguous
slot order. Slots are carried in ordinary XDP link observations and canonical
pin paths. `XdpReplacementStore` publishes a complete desired membership only
while the observed snapshot and runtime/store identity still match. Surviving
managed link IDs, program identities, metadata, creation timestamps, priorities,
and proceed-on masks are preserved. Revision, dispatcher ID, slots, extension
kernel links, and membership change atomically; the durable outer link stays
unchanged. Failures return the receipt for an explicit retry and commit nothing.
SQLite rechecks and publishes within one transaction using Go schema 2. JSON
publishes a complete file in its existing format (5 or 6 for replacement), and
never implicitly upgrades older state.

Run `direnv exec . make rust-test-xdp-store` for the six shared store contracts
and backend-specific tests. Coverage includes 1 → 2 → 1 → 0 persistence with
removal of either member, growth to ten slots, capacity refusal, stable IDs,
reopening, stale/foreign receipts, invalid requests, SQLite DML aborts and ignored
writes, JSON publication obstruction, malformed membership, and format-4 refusal.
The runtime uses complete-snapshot operations for replacement. Singleton store
entry points remain restricted to first attach and last detach.

The kernel boundary now supplies `bpfman_kernel::XdpReplacement`: complete
configuration loading, selected revision creation, arbitrary validated slots,
complete artifact adoption, and owned switching/restoration. The Linux adapter
uses `BPF_LINK_UPDATE` with `BPF_F_REPLACE` to atomically check the expected old
program. The outer link and pin retain their identities. Filesystem operations
remain beneath validated runtime descriptors; moved pins and revision parents
are refused. A failed post-switch observation returns restoration evidence,
including owned descriptors for both targets. Failed restoration retains that
evidence. Explicit retry can recognize a completed restoration without updating
an unrelated target.

Run `direnv exec . make rust-test-xdp-switch` for two real-kernel adapter suites,
one per store. They stage and publish 1 → 2 → 1 → 0 revisions, remove either
member, inspect real packet counters, and prove that the first slot's proceed-on
mask controls the second slot's execution. They also cover rejected updates,
post-update observation failures, failed restoration, explicit retry, foreign
runtime writers, moved pins/parents, undeclared slots, and residue-free cleanup.
These are direct adapter contracts, separate from runtime acceptance.

The public runtime now observes the complete snapshot under one writer lock,
plans the desired membership, stages every member, switches the outer link,
and publishes atomically. Failed publication or cancellation after switching
requires restoration before staged cleanup. `XdpError::restoration_attempts`
retains every restoration outcome. Failed restoration blocks all revision cleanup;
`retry_xdp_cleanup` admits one explicit restoration/cleanup pass. Successful
recovery reports retain the original failure and prior attempts.

After successful publication, only the old revision can be retired. A retirement
failure exposes `committed_snapshot()` on the error and any later successful retry
report; CLI diagnostics explicitly say the replacement committed. Cancellation
during publication cannot turn committed state into compensation authority.
`get_xdp_dispatcher` and `dispatcher get xdp` return all members in slot order.

Shared fake-kernel tests exercise attach and non-last detach across staging,
switch, publication, restoration, cleanup, cancellation, and foreign-runtime retry.
Real runtime tests use packet counters to verify both members, proceed-on masks,
either survivor, and restoration after failed attach/detach publication. Both
stores also run the unchanged Go priority-ordering, slot-reuse, ten-slot capacity,
configuration-after-detach, and default-proceed-on rebuild scripts.

Run `direnv exec . make rust-test-xdp-unload` for attached-unload acceptance on
both stores. The unchanged `TestXDP_UnloadDispatcherMemberRebuildsSurvivor.bpfman`
script verifies CLI survivor rebuilding. Real packet tests unload either member,
including duplicate links for the removed program, and verify surviving execution,
stable outer identity, failed-publication restoration, moved-pin refusal on retry,
last-member deletion failure, and complete teardown. Shared fake tests also cover
multiple interfaces, foreign kernel instances, failed retirement, cancellation,
partial progress, and new links added during retained recovery.

Run `direnv exec . make rust-test-xdp-corpus` for twelve additional unchanged Go
scripts, each registered separately on SQLite and JSON (24 real-kernel tests):

| Coverage | Unchanged scripts |
| --- | --- |
| Ten-slot traffic and four fill/drain/refill peaks | `TestXDP_DispatcherChainExecution`, `TestXDP_DispatcherFillDrainRefill` |
| Exact weighted counters through staggered detach | `TestMultiProgXDP_AllProceed_DefaultProceedOn` |
| Custom DROP continuation and PASS/DROP stopping | `TestMultiProgXDP_AllProceed_CustomProceedOn`, `TestMultiProgXDP_ChainStopsAtDrop_DefaultProceedOn`, `TestMultiProgXDP_ChainStopsAtPass_CustomProceedOn` |
| Proceed-on masks, single actions, combinations, and defaults | `TestXDP_ProceedOnPassEncoding`, `TestXDP_ProceedOnEncodingMatrix` |
| Priority zero and equal-priority ordering | `TestDispatcher_ZeroPriorityDefaultOrderingXDP`, `TestXDP_DispatcherPriorityTieBreakByName` |
| Fragment-aware attachment and normal-MTU traffic | `TestLoad_XDPFragsProgram` |
| Independent interface membership | `TestDispatcher_MultipleInterfacesIndependentXDP` |

The traffic scripts inspect real BPF maps and packet delivery. Encoding tests
establish the stored masks; separate packet-delivery tests below establish TX
and direct REDIRECT delivery.
Every DSL run must execute its selected script and finish with empty program,
link, and dispatcher inventories and no owned program, map, link, bytecode,
staging, or XDP revision artifacts. No script changes or new production behavior
were needed for the original eleven-script corpus. The added fragments script
uses the supported namespace and fragment-aware dispatcher paths.

### Explicit XDP network namespaces

`link attach xdp ... --netns PATH` accepts an absolute network-namespace path;
omitting it selects the calling thread's namespace. The kernel adapter opens
and validates the namespace descriptor, resolves the interface there, and retains
its device/inode identity. First attach uses that same retained descriptor, so
it cannot silently re-resolve a changed namespace path between admission and
outer-link creation. Identical interface names/indices in different namespaces
remain independent attach points.

Like Go's XDP implementation, only namespace-sensitive operations switch
namespaces. Rust runs interface lookup and first outer-link creation on dedicated
worker threads that enter once and exit. The caller never switches or restores
its namespace; synchronous joins keep the existing writer lock held. The worker
returns an owned link descriptor. Pinning, persistence, conditional dispatcher
switching, restoration, and removal stay with the caller under the same runtime
authority. Entry/spawn failures return errors before attachment, and no worker
or descriptor remains detached from its owning operation. This does not implement
the separate planned mount-namespace subprocess helper for uprobes.

Namespace paths are persisted in Go-compatible SQLite columns and JSON format 6,
and appear in link details and dispatcher runtime JSON. Rebuilds retain the
established path even when a new attach uses an alias of the same namespace.
Missing or replaced paths refuse new preparation and retained unload retries.
Restoring the original path permits explicit retry with its ownership intact.
Final detach uses owned kernel/pin identities and can clean up after a named
namespace path is removed; non-last detach and attached unload need the stored
namespace path to prepare surviving members or retained program evidence.

Run `direnv exec . make rust-test-xdp-netns` for six real-kernel tests on both
stores. The unchanged `TestXDP_NetnsVethPairLinkRoundTrip` and
`TestXDP_NetnsDispatcherRebuild` scripts verify traffic, rebuilding, observations,
and counter quiescence. Shared recovery tests cover identical interface indices
in independent namespaces, alias paths, invalid namespace objects, missing/replaced
paths, publication failure, retained unload, removed namespace paths, caller
isolation, and residue-free teardown. Store contracts exercise namespace
persistence and reject changing a committed namespace during replacement. A
worker-entry failure test verifies that no effect runs or caller namespace changes.

### XDP TX and REDIRECT packet delivery

Run `direnv exec . make rust-test-xdp-delivery` for four real-kernel tests:
SQLite and JSON, each with explicitly verified native and SKB ingress modes.
A private namespace contains two veth pairs. Marked Ethernet frames enter `in0`
from `source0`; TX returns them to `source0`, while `bpf_redirect(ifindex, 0)`
sends them through `out0` to `sink0`. Receiving peers have native PASS dispatchers
to enable the veth receive path for native XDP transmission.

The Go test fixture `e2e/testdata/xdp-delivery/main.go` opens all three raw capture
sockets before sending three frames. It excludes outgoing copies, validates frame
length/payload, and refuses duplicate sequence numbers. Captures prove the exact
delivery location; per-member BPF counters independently prove execution. Go and
its existing `x/sys` dependency are already part of the test toolchain; the fixture
adds no Rust dependencies, unsafe blocks, or production commands.

Each test covers both TX and direct REDIRECT, default stopping and explicit
proceed-on continuation into a DROP member, and removal of either member. When
the final member permits continuation, exhausting the shared dispatcher returns
PASS; the tests also capture this local delivery. Failed attach/detach publication
must restore both the complete snapshot and the packet path. Successful replacement
and survivor rebuilding retain the outer link ID. Last detach restores ordinary
local delivery, and final teardown leaves empty inventories and no owned artifacts.
No production changes were needed for this acceptance slice.

This establishes normal-sized veth frames with direct interface redirects.
The DEVMAP suite below covers map-based redirects. CPUMAP/XSKMAP redirects,
multi-buffer TX/REDIRECT, physical NICs, and actual hardware offload remain
unverified. The kernel's
[redirect documentation](https://docs.kernel.org/bpf/redirect.html) explains why
invoking the helper alone does not prove transmission: the returned action and
the driver's redirect/flush path must complete too.

### XDP DEVMAP forwarding

A DEVMAP is an address book for output interfaces. For example, entry `0` can
name `out0`: the BPF program calls `bpf_redirect_map` with key `0`, and the kernel
sends the frame through that interface. User space can change the entry while
the program runs. An empty entry returns the fallback action chosen by the
program, such as PASS to continue normal processing or DROP to discard the frame.
See the kernel's [DEVMAP documentation](https://docs.kernel.org/bpf/map_devmap.html).

Run `direnv exec . make rust-test-xdp-devmap` for four real-kernel suites:
SQLite/JSON with explicitly observed native/SKB ingress modes. Each covers
PASS and DROP lookup fallback, default REDIRECT stopping or explicit continuation
into a DROP member, and removal of either member. The same marked-frame captures
and exact execution counters used for direct delivery prove forwarding to
`sink0`, a live target change returning frames to `source0`, and suppressed
forwarding when the dispatcher continues. Changing the map does not rebuild
the dispatcher.

The original DEVMAP pin ID and retained descriptor must observe the same target
after successful replacement, failed attach/detach publication restoration,
survivor rebuilding, and last detach. Missing PASS entries reach the next member;
missing DROP entries stop before it. Entries can be deleted and repopulated after
rebuilding, including while the redirect program is the only survivor.

The shared Go fixture populates/deletes key `0` using the existing Cilium library.
It runs inside the private namespace because DEVMAP updates resolve interface
indices in the updating process's namespace. It handles four-byte ifindices and
Aya's eight-byte values with an unused egress-program FD. This is test setup;
it adds no production map-editing command, shared-map protocol, or Rust dependency.
The local Linux reference checkout at `~/src/linux.git` confirmed the namespace
lookup and deferred program/map release paths; see [AGENTS.md](AGENTS.md).

Unload removes the owned pin. After closing the test's retained descriptor, a
five-second bounded observation must see the map ID disappear; Linux releases
program-held map references after deferred reclamation. Other observation errors
fail the test. Final inventories and owned artifacts must be empty. No production
changes were needed for this slice.

This covers unicast `BPF_MAP_TYPE_DEVMAP` with normal-sized veth frames and no
egress program. DEVMAP_HASH, broadcast/exclude-ingress flags, attached egress
programs, CPUMAP/XSKMAP, multi-buffer forwarding, physical NICs, and hardware
offload remain unverified.

Next: multi-buffer TX/REDIRECT forwarding. The uprobe mount-namespace helper
remains a separate attachment-family boundary.

The DEVMAP checkpoint passed the full `direnv exec . make rust-check`
gate: formatting, Clippy, workspace tests, compile-fail contracts, documentation,
38 fake-kernel lifecycle tests, and all 88 real-kernel tests. NixOS kernel-build
discovery and the optional `KERNEL_DEV` override are documented in
[AGENTS.md](AGENTS.md).

### Injectable kernel lifecycle

`Bpfman<S, K>::new(store, kernel, timeout)` accepts one kernel backend alongside
its real store. Reads, loading, tracepoint attachment, XDP attachment/replacement/detach,
unload, and explicit cleanup retries use that same instance. Operations require
only the capabilities they use: `ProgramObservations`, `LinkObservations`,
`ObjectLoader`, `ProgramResources`, `ProgramLoad`, `TracepointLinks`, or
`XdpLifecycle`. Associated types keep live handles and cleanup receipts opaque;
failed acquisitions and removals return unresolved ownership.

The CLI selects `bpfman_kernel_aya::Kernel`. `PreparedProgram::new(&kernel, ...)`
validates captured ELF input before runtime initialization. Aya, aya-obj, concrete
program/map/link handles, and BPF syscalls stay inside `bpfman-kernel-aya`.
`bpfman-fs` retains descriptor-relative path authority and removal. Its pinning
bridge lends non-forgeable `PinTarget` and `PinSource` values to opaque kernel
handles for individual syscalls. Kernel errors expose portable categories and
retain concrete diagnostic causes through source chains.

`tests/kernel_lifecycle.rs` runs the public application with a shared stateful
fake kernel and each real store. It covers load and batch commit, tracepoint and
XDP lifecycles, partial acquisitions, blocked cleanup, cancellation, post-commit
observation errors, retained link handles, and retries through foreign kernel
instances. Bytecode publication, runtime locking, and persistence remain real;
no bpffs mount or kernel privileges are needed for these tests. Store fault
injection uses the existing forwarding `Faults<S>` decorator.

Run this focused suite with:

```sh
direnv exec . make rust-test-kernel-fake
```

It also runs in `rust-check`, alongside operation-level fault tests, filesystem
confinement tests, all 88 real-kernel tests, and the unchanged admitted DSL corpus
on both stores. The fake checks orchestration and simulated ownership; the real
kernel tests establish verifier, syscall, and kernel lifetime behaviour.
