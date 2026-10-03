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
| `bpfman-model` | 0 | Pure program specifications, stored records, and kernel observations |
| `bpfman-core` | 1 | Pure listing/store policy, load compensation, and forward unload continuations |
| `bpfman-lock` | 1 | Go-compatible writer lock and borrowed mutation capabilities |
| `bpfman-kernel` | 2 | Read-only BPF metadata and statistics with a private syscall boundary |
| `bpfman-fs` | 2 | Runtime authority, bpffs preparation, owned pins, and bytecode publication/removal |
| `bpfman-store` | 3 | Backend-independent read, commit, and conditional teardown contracts |
| `bpfman-store-sqlite` | 4 | Go-compatible creation, queries, and atomic tracepoint/map-set persistence and conditional teardown |
| `bpfman-runtime` | 4 | Local tracepoint load/unload, private Aya adapter, compensation, and observation gathering |
| `bpfman` | 5 | Typed Clap CLI, load/get/list/unload dispatch, and Go-compatible text/JSON presentation |

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

## Store backend boundary

The CLI composition root selects `bpfman_store_sqlite::Backend` and passes it to
runtime operations. Runtime depends on `bpfman-store`, with no direct or transitive
SQLite dependency. Architecture tests enforce this separation. The contract
crate defines four small, statically dispatched interfaces:

- `OpenStore` opens compatible state under writer authority and returns an owned
  reader. Backend-specific format checks and missing-state initialization stay
  inside the implementation.
- `ProgramReader` returns stored summaries or complete domain records from a
  consistent snapshot, without exposing serialized data or queries.
- `CommitLoad` atomically publishes a tracepoint and its private map-set
  membership. An error means no commit, so compensation remains safe.
- `UnloadStore` validates ownership and conditionally deletes records and map
  sets using opaque, non-cloneable backend receipts. Failed deletion returns the
  receipt for an explicit later pass.

There are no connection types, schema-version fields, or transaction callbacks
in these contracts. A future JSON-file backend could implement the same operations
using atomic file publication while preserving locking, validation, and commit
semantics. This slice provides SQLite plus an independent test-only in-memory
backend; it does not implement JSON persistence or backend selection flags.

Shared store errors expose portable categories and preserve private diagnostic
sources. `UnloadReport<S>` and `UnloadError<S>` retain the selected backend's
receipt types; explicit retries take that backend and writer authority for the
original root. Backend implementations must reject foreign or stale evidence.
Failed-load cleanup needs only filesystem receipts and never reopens or retries
the store.

Tests use the in-memory backend through production read/unload operations and
the production load interpreter with fake kernel/filesystem effects. They check
open failures before acquisition, atomic commit failure with independent cleanup
failures, successful commit without compensation, retained receipts, unchanged
faults, and wrong-runtime/backend rejection. Existing SQLite, exhaustive
compensation, and real-kernel compatibility gates remain in place.

Generic live-kernel tests inject failures at store operations through a test-only
`Faults<S>` decorator. They call public runtime operations with real kernel and
filesystem effects; no production failure flags or persistence edits are needed.
The same `lifecycle::exercise<S>` scenarios can run against another store backend.
CLI and unchanged DSL tests inspect returned JSON and runtime artifacts, with no
database queries. Privileged tests are ignored by the normal workspace test run;
the Make targets use Cargo's runner to execute them in a private mount namespace.
The harness and executable-selection tests are Rust integration tests.

SQL fixtures and DDL/DML remain in tests explicitly testing SQLite. Adapter tests
verify transaction rollback, constraints, orphan map-set absence, invalid records,
and conditional deletion. `tests/sqlite_compatibility.rs` checks the selected
SQLite CLI against Go's schema. These format-specific assertions do not belong in
the generic lifecycle tests. Sharing persisted state with Go is checked separately
in `tests/kernel/go_compatibility.rs`; a JSON backend would not need to share
Go's SQLite representation. The division preserves the previous coverage:

| Scenario | Coverage |
| --- | --- |
| Commit, deletion, GC, and post-commit read failures with live programs | Generic `tests/kernel/lifecycle.rs`, faulting store operations |
| Foreign pin/map identity, failed output delivery | `tests/kernel/cli.rs`, public CLI and artifact observations |
| Go/Rust shared-state observations and unload in both directions | SQLite-specific `tests/kernel/go_compatibility.rs` |
| Unsupported stored relationships, noncanonical stored paths, partial writes | SQLite adapter tests using the actual Go schema |
| CLI load/get/list/unload contract | Unchanged `TestTracepoint_LoadAndGet.bpfman` through the Rust test harness |
| Typed, raw, and nested-shell executable selection | `tests/e2e_selection.rs`, unprivileged Make recipe tests |

A JSON backend must run the same generic lifecycle, CLI, and unchanged DSL
scenarios, changing only backend selection in test setup. It adds tests for its
own persistence guarantees; it does not get a separate behavioural suite.
No JSON implementation is included yet, so running this matrix against two
persistent backends remains the next proof of substitutability.

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
`--program-type` and `-p` are aliases. Startup acquires `<runtime>/.lock` using
the same `flock` protocol as Go, then creates a missing
`<runtime>/db/store.db` at Go schema version 2. The private runtime operation
`open_or_create_store` acquires the writer and calls the selected backend's
`OpenStore` implementation. SQLite observes its schema and uses the core's
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
schema in a read-only transaction on that handle after releasing the writer lock. Production
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
cancellation and owned inherited descriptors; CLI signal cancellation and
namespace-helper process launching are not yet wired. No privileges are needed
for text/quiet listing in a writable temporary runtime. Loading requires BPF and mount
privileges; `/run/bpfman` will normally require sudo.

Listing supports managed table, quiet-ID and JSON output. Text and quiet output
use stored summaries without kernel privileges; JSON adds full records and live
kernel observations. `program get ID [-o text|json]` observes one unattached
managed program, including its maps and statistics. `--all`, kernel link state
filters, attach, detach, and full observation of attached programs remain
unsupported. Unload supports one unattached tracepoint with private maps.

`program load file PATH` and `program load image IMAGE` parse typed requests,
including repeated/comma-separated `--programs`, metadata, globals, application,
nonzero map-owner IDs, text/JSON output requests, and image-specific pull/auth
options. Fentry/fexit/LSM variants carry required load-time targets. Invalid input
exits with status 2. One local tracepoint with private maps and
metadata/application labels is executable, with Go's detailed text output or
JSON load envelope. Image loads, other program types, batches, global overrides,
and map-owner sharing exit with status 1 before source access or runtime effects.
ELF-level unsupported PinByName maps and section/type mismatches are rejected
before runtime setup. Credentials are not echoed in auth diagnostics or help.
Unlike Go, this parser requires the explicit file/image verb and rejects duplicate
ELF selections, extraneous load-time targets, and zero map-owner IDs.

The unchanged `e2e/scripts/TestTracepoint_LoadAndGet.bpfman` now runs against Rust
through the Go shell runner. The dedicated gate builds the required executables,
uses a temporary runtime in a private mount namespace, selects local bytecode,
and verifies that unload leaves no owned residue:

```sh
direnv exec . make rust-test-observation
```

This needs the same privileged kernel environment and bytecode fixtures as
`rust-test-kernel-load`. If the Nix Go linker needs external linking, pass
`EXTRA_GOFLAGS=-ldflags=-linkmode=external` to Make. The full corpus still includes
unsupported operations; passing this one script does not establish full parity.
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

Full store decoding, kernel reads, map-pin correlation, and selection happen
under one runtime writer scope. The store returns typed domain values from a
read-only transaction, preserving nullable timestamps and nil versus empty
global byte slices. Timestamp output matches Go's RFC3339Nano formatting,
trimming trailing fractional zeros without reducing precision. CLI conversion
owns JSON field names and the different load/get/list envelopes; the pure model
has no serialization or backend dependencies.

The `bpfman-kernel` adapter supplies fields unavailable through Aya's public
metadata API. Its private syscall module is the only unsafe exception: it
supports read-only descriptor lookup and metadata queries, bounds its buffers,
and owns descriptors until observation completes. Generated Aya ABI structs
never cross its public interface. Because Cargo cannot override an inherited
`forbid`, this crate mirrors workspace lints with `unsafe_code = "deny"` and
allows unsafe only in that module; an architecture test checks the other gates
remain identical.

Absent kernel objects are distinct from denied observations. Get reports a
stored program missing from the kernel as requiring reconciliation; list JSON
uses a null kernel field for that case. Permission and other program lookup
errors fail the command. Individual unreadable maps are omitted, as in Go,
while the program's observed map IDs remain intact. Available zero counters are
not null. Load omits statistics and map-pin presence observations; get includes
them. Map pins are correlated by kernel map ID, avoiding ambiguous matches when
names share a truncated prefix. Runtime paths alone do not assert presence.

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

`bpfman-runtime::LoadCleanup` is the narrow, injectable filesystem-effects
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
pathname; callers never supply a deletion path. Only the selected program is
loaded; private maps are pinned under `fs/maps/{program_id}`. Internal data maps
remain kernel-owned and are not pinned separately. There is no attachment.

Bytecode and provenance are written into an exclusively created staging
directory, then published to `programs/{id}` with `RENAME_NOREPLACE`. Partial
publication returns its staged ownership. Cleanup attempts each owned file and
removes the directory only after all its files have been removed. The runtime
likewise retains the owned map-directory receipt separately and attempts its
removal only when no map-pin instructions remain unresolved. Unexpected
children prevent directory removal. `LoadError::retry_cleanup` performs one
explicit pass and retains the original cause and all cleanup history.

These checks enforce descriptor-based confinement and refuse observed
replacement; they rely on cooperating writers holding `.lock`. They are not a
sandbox against privileged processes renaming entries between the final check
and a syscall. Collection directories, the lock, database bootstrap, and a newly
mounted bpffs may remain after a failed load. Crash recovery is separate work.
Non-UTF-8 source/runtime paths are rejected before load effects because this
slice persists paths as SQLite text; read-only listing still accepts native paths.

The store creates the private map set and tracepoint row in one transaction,
with Go's source path, license, metadata, UTC creation time, and null update time.
Existing rows are not overwritten. On commit, ownership transfers to stored
state; failed output delivery never compensates the successful load.

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

A JSON backend is the next check on store substitutability, followed by tracepoint
attachment/detachment to admit more unchanged lifecycle DSL scripts. Broaden
supported options incrementally; do not weaken the scripts for Rust.


## Unattached tracepoint unload

```sh
sudo rust/target/debug/bpfman program unload PROGRAM_ID
direnv exec . make rust-test-unload
```

This slice accepts one managed tracepoint with no stored links, its own map set,
no other map-set users, and no shared-map-pin registrations. Other program types,
linked/shared state, multiple operands, and `--ignore-missing` are explicitly
unsupported. A missing managed record returns an error without inspecting or
adopting a kernel-only program or creating a database. It does not yet make Go's
additional not-managed versus not-found distinction.

Under one writer scope, store preflight validates canonical artifact paths and
exclusive ownership. Filesystem observation opens existing objects without
creating or mounting collections. It verifies descriptor confinement, types,
hard-link counts and inode identities; a live program pin must match the ID and
tracepoint type, and its map pins must refer to that program's maps. If the
program pin is already absent after a partial unload, the validated private
map-set record authorizes inspection of its remaining BPF map pins. Bytecode
adoption accepts only `bytecode.o` and `provenance.json`. Unknown children and
unsafe paths fail preflight before any managed-object removal. Ordinary missing
artifacts are treated as already absent.

Unload is forward teardown of committed state, with its own pure continuations:

1. Remove the program pin. Failure stops all later effects.
2. Delete the managed record. Failure is returned, but independent bytecode
   cleanup still runs; maps and their map-set row remain untouched.
3. After record deletion, remove each private map pin once, continuing after
   individual failures. Remove the container only after all pins succeed, then
   delete the unused map-set row only after the container is gone.
4. Attempt bytecode cleanup independently of the record/map cleanup outcomes.

As in Go, post-record map/bytecode cleanup failures are warnings: the program is
unloaded even if GC leaves residue. Store deletions revalidate evidence in atomic
transactions and require the same opened runtime identity. Failed effects retain
non-cloneable receipts. Reports preserve successful and failed attempts with
stable instruction IDs; work skipped because a prerequisite failed is retained
without inventing an attempt.

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
An unchanged-fault pass must make no progress; clearing the injected faults
allows only retained work to complete. Adapter tests cover store changes,
ignored/failed deletes, wrong runtime authority, replacement, symlinks, hard
links, FIFOs, and unexpected children. The kernel gate supplies the BPF identity
and lifetime checks that these fakes cannot establish.
