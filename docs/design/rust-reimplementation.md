# Rust reimplementation using the Go design

## Status

Implementation in progress in the independent `rust/` workspace. Managed
listing and one local-file tracepoint load (without attachment) are executable.
See [the workspace checkpoint](../../rust/README.md) for supported options and
the focused kernel acceptance gate; full behavioural parity remains unfinished.

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
backend-independent `OpenStore`, `ProgramReader`, `CommitLoad`, and `UnloadStore`
contracts. Runtime operations are generic over the capabilities they consume;
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
appears in the contracts. Shared tests exercise both persistent backends and an
independent in-memory test implementation; SQLite remains the default selection
for Go interoperability.

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
| `bpfman-kernel-aya` | Effectful | Aya-backed program, map, link, dispatcher, netlink, tracefs, and namespace operations |
| `bpfman-image-oci` | Effectful | OCI pull, cache, authentication, and signature-policy adapters |
| `bpfman-runtime` | Interpreter | Drives core machines, routes effects, retains effect error sources, and provides the application-facing manager |
| `bpfman-proto` | Boundary | Generated protobuf vocabulary only; not the domain model |
| `bpfman-api` | Front end | gRPC request conversion, status mapping, and server implementation |
| `bpfman-csi` | Front end | CSI integration using narrow runtime capabilities |
| `bpfman` | Composition root | CLI, daemon mode, namespace-helper mode, configuration, logging, and dependency construction |

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
- `bpfman-runtime` composes effects through narrow contracts. Persistence
  implementations are selected by the binary and cannot enter runtime's dependency closure.
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

The new implementation will use SQLite and retain compatibility with the Go
schema and migration history. It will not use sled.

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

Dispatcher replacement is the most important early proof of the architecture.
The core owns:

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

The initial implementation must preserve the Go ordering:

1. observe the existing snapshot;
2. compute the complete desired member set and next revision;
3. load and pin the new dispatcher;
4. attach and pin every extension to it;
5. create or swap the live XDP link, or install the replacement TC filter;
6. atomically persist the complete new snapshot;
7. restore the old live attachment if persistence fails;
8. remove the new resources on failure; and
9. remove the old revision after successful persistence.

TC replacement must continue to persist and use the exact kernel-assigned
filter handle. XDP replacement must continue to reuse the durable dispatcher
link while changing the program it targets.

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

A private runtime `open_or_create_store` operation acquires the lock before
calling the selected backend. Inside the SQLite adapter, the pure
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
databases are not implicitly repaired or migrated. `open_or_create_store`
returns an opened store handle, not a path to reopen later, and releases the lock. Ordinary
snapshot queries remain read-only and recheck the schema within their own
transaction. CLI signal cancellation and namespace-helper launching are not
implemented in this slice.

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

## Concurrency and async runtime

The domain and core crates are synchronous and runtime-independent. They contain
no futures, executor handles, mutexes, or async traits.

Kernel operations and rusqlite are fundamentally blocking. They should run in a
controlled blocking interpreter or on an operation thread rather than acquiring
async signatures that merely hide blocking work. The gRPC server may use an
async runtime, but that runtime is a front-end and driver concern.

Avoid `async_trait` unless a demonstrated boundary requires dynamic async
dispatch. No async runtime type should appear in the domain, core, or public
effect vocabulary.

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
test suite to run at the end. The current corpus contains 131 `.bpfman`
end-to-end scripts covering program and link lifecycles, dispatcher rebuilds,
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
and JSON now run the same lifecycle, CLI, and unchanged tracepoint DSL scenarios,
with backend selection confined to setup. The runtime and pure crates did not
need backend-specific branches.

The JSON adapter publishes versioned whole-file snapshots through descriptor-
relative filesystem operations under the runtime writer. Program and private
map-set membership commit together. Deletion receipts bind the runtime, store
identity, and record generation; failed deletion retains the receipt. Publication
errors precede the atomic rename and therefore authorize compensation safely.
Interrupted staging can be reused without changing published state. Runtime state
lives under `/run`; power-loss durability is outside this contract.

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

### Phase 3: dispatcher proof

- Implement pure dispatcher ordering and configuration.
- Implement XDP first attach, replacement, detach, and rollback.
- Implement TC replacement with exact filter handles and clsact ownership.
- Add TCX native multi-program ordering.
- Admit dispatcher lifecycle, ordering, execution, rebuild, and residue scripts
  as each program family becomes available.

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
