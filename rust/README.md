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
| `bpfman-model` | 0 | Pure domain vocabulary, payload-bearing program specifications, and stored summaries |
| `bpfman-core` | 1 | Pure listing/store policy and single-program load/compensation continuations |
| `bpfman-lock` | 1 | Go-compatible writer lock and borrowed mutation capabilities |
| `bpfman-fs` | 2 | Runtime authority, bpffs preparation, owned pins, and bytecode publication/removal |
| `bpfman-store-sqlite` | 3 | Go-compatible creation, queries, and atomic tracepoint/map-set persistence |
| `bpfman-runtime` | 4 | Local tracepoint loading, private Aya adapter, compensation, and observation gathering |
| `bpfman` | 5 | Typed Clap CLI, supported load dispatch, and text/quiet presentation |

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
`open_or_create_store` owns this sequence. Under the lock, it observes the
existing schema, asks the core's `plan_store_open` whether to create/use/reject,
applies that decision, and returns an opened read-only `Store`.
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
for listing in a writable temporary runtime. Loading requires BPF and mount
privileges; `/run/bpfman` will normally require sudo.

Listing supports managed table and quiet-ID output only. Stored names
are used directly (no kernel-name fallback). `--all`, JSON listing, kernel link
state filters, get, attach, detach, and unload are not implemented and are rejected.
`program load file PATH` and `program load image IMAGE` parse typed requests,
including repeated/comma-separated `--programs`, metadata, globals, application,
nonzero map-owner IDs, text/JSON output requests, and image-specific pull/auth
options. Fentry/fexit/LSM variants carry required load-time targets. Invalid input
exits with status 2. Only one local tracepoint with text output, private maps,
and metadata/application labels is executable. Image loads, other program
types, batches, global overrides, map-owner sharing, and JSON output exit with
status 1 before source access or runtime effects. ELF-level unsupported
PinByName maps and section/type mismatches are rejected before runtime setup.
The supported load currently prints a compact listing table; Go's detailed load
presentation and JSON result shape remain future compatibility work.
Credentials are not echoed in auth diagnostics or help. Unlike Go, this parser
requires the explicit file/image verb and rejects duplicate ELF selections,
extraneous load-time targets, and zero map-owner IDs. OCI reference resolution
and object-file validation remain execution responsibilities, not parser I/O.
Consequently the typed DSL's automatic JSON requests cannot yet use this CLI;
the full behavioural corpus is a later acceptance gate. Once the relevant
commands are ready, build the Go shell/test runner and select the new binary:

```sh
make run-e2e-scripts BPFMAN_UNDER_TEST="$PWD/rust/target/debug/bpfman"
```

This run-only target requires prebuilt fixtures and an appropriate privileged
test environment. It sets both `BPFMAN_BIN` and `PATH` inside `sudo` so typed
DSL commands, raw `exec bpfman`, and nested shells agree. The selected executable
must be named `bpfman`. Keep Go and Rust suite runs separate on a clean runtime.

`make test-e2e-selection` tests this wiring without sudo or kernel effects,
using fixture executables and the real Make recipe. CLI process tests also
exercise the real Rust executable. Behavioural parity is not yet claimed.

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
loader, Go can observe and unload the program, failed publication and persistence
remove owned resources, and output-delivery failure preserves the committed load.
It also checks unsupported ELF inputs before runtime creation. This gate does
not claim full CLI or unchanged-DSL parity.

For a manual load (use the Go CLI to unload until Rust unload is implemented):

```sh
sudo rust/target/debug/bpfman program load file \
  e2e/testdata/bpf/tracepoint_counter.bpf.o \
  --programs tracepoint:tracepoint_kill_recorder --application rust-slice
```

Next, add compatible load JSON, get/JSON-list, and unload, then tracepoint
attachment/detachment to admit the unchanged lifecycle DSL scripts. Broaden
supported options incrementally; do not weaken the scripts for Rust.
