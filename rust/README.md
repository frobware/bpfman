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

`rust-test` runs all userspace unit, integration, and documentation tests before
the serial real-kernel binary. `make rust-test-userspace` selects that first stage,
including the shared fake-kernel lifecycle tests. CLI integration targets are
discovered automatically, so newly added contracts join the normal gate.

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
| `bpfman-image-oci` | 3 | Synchronous OCI transport, extraction and cache admission |
| `bpfman-fs` | 2 | Runtime authority, bpffs preparation, owned pins, and bytecode publication/removal |
| `bpfman-store` | 3 | Backend-independent read, commit, and conditional teardown contracts |
| `bpfman-store-sqlite` | 4 | Go-compatible creation, queries, and atomic program/map-set persistence and conditional teardown |
| `bpfman-runtime` | 4 | Generic attachment lifecycle interpreters, consuming compensation, and observations |
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

## OCI bytecode

`image inspect IMAGE` reads registry metadata without creating runtime or
cache state. `program load image IMAGE --programs TYPE:NAME` accepts bare names,
tags and SHA256 digest references, selects the host's Linux platform, and uses the
existing load/attach/unload lifecycle. Stored provenance retains the original
reference and resolved platform-manifest digest.

Image loads honor `[signing] verify_enabled` and `allow_unsigned`, both true by
default: signed images must verify, while unsigned images are accepted by default.
Setting `allow_unsigned = false` requires a valid signature. Explicitly disabling
verification accepts images without checking their signatures. Invalid signatures,
registry errors and malformed evidence never count as unsigned images.

`[[signing.trusted_identities]]` accepts `certificate_identity` and
`certificate_oidc_issuer`, or their `_regexp` forms. Each rule requires both
fields; exact and regexp forms are mutually exclusive, regexps match the whole
value, and rules are ORed. An empty list accepts any valid Sigstore signer.
`image verify IMAGE` checks signatures without creating program runtime or bytecode
cache state and
always enables verification. Its `--allow-unsigned`, `--certificate-identity`,
`--certificate-identity-regexp`, `--certificate-oidc-issuer` and
`--certificate-oidc-issuer-regexp` flags override configuration.

Verification uses synchronous `sigstore-verify`, checking the signature,
certificate chain, code-signing usage, SCT,
authenticated signing time and Rekor evidence. Legacy Cosign `.sig` images and
their embedded bundles match Go's currently enabled format. Missing bundles use
bounded, redirect-free lookup at the public Rekor service. Custom trust-root
overrides and OCI 1.1 signature discovery remain unimplemented.

The CLI defaults to `[signing] trust_source = "tuf"`. A pinned, self-signed
Sigstore production TUF bootstrap authenticates sequential root rotations and
signed timestamp, snapshot and top-level targets metadata before accepting
`trusted_root.json`. Verification stays synchronous using `sigstore-tuf` with
its network features disabled and the existing bounded, redirect-free HTTP
transport. Registry credentials are never sent to TUF.

`--sigstore-cache-dir` / `BPFMAN_SIGSTORE_CACHE_DIR` selects the separate trust
cache; the default is `$XDG_CACHE_HOME/bpfman/sigstore`, or
`$HOME/.cache/bpfman/sigstore`. Complete generations publish atomically in
`sigstore-tuf-v1` under a single writer lock. Readers need no lock and reauthenticate
metadata from the pinned bootstrap on every operation. Refreshers serialize and
re-read after acquiring authority. The trust writer may perform network I/O;
it never holds the program-store or bytecode-cache writer lock.

Online operations refresh absent, incomplete, expired or 24-hour-old metadata.
A fresh authenticated cache requires no TUF network access. `Never` never refreshes
and requires unexpired cached metadata as well as an image admitted under the
selected trust material. Failed refreshes fail the operation without falling back
to bundled or expired roots. Verified root/timestamp/snapshot progress is persisted
even if a later step fails, preserving rollback floors across processes; an
incomplete generation cannot authorize target use. Deleting or replacing the
entire cache also removes its learned version floors; this cache assumes the same
local writer authority as the image cache. Root history is bounded to 512 rotations
from the bundled bootstrap, with at most 32 rotations per refresh.

Explicit `[signing] trust_source = "bundled"` selects the dependency's embedded
production roots and performs no TUF I/O. Offline fixture comparisons select this
mode explicitly; it has no metadata refresh/expiry protection. Library policies
constructed with `SigningPolicy::verify` likewise use embedded roots unless the
caller selects `with_trust_cache`. Disabling signature verification avoids all
trust-cache access. Synchronous downloads have a 30-second request deadline;
cancellation is checked between requests, reads, verification and publication.

`program load image`, `image verify`, `image inspect` and `image build` accept
`--registry-auth BASE64` or `BPFMAN_REGISTRY_AUTH`, containing base64-encoded
`username:password`. Passwords may contain colons; an explicit flag overrides the
environment. Basic challenges and authenticated bearer-token exchange are
supported. Credentials stay in memory, including retained upload-abort receipts;
cache entries and program records contain no credentials. Cache hits admitted
under the same signing policy need no authentication. Token requests refuse redirects, registry read
redirects strip authorization, and redirected hosts cannot supply authentication
challenges. The CLI caps HTTP dependency logging below trace, even under explicit
`RUST_LOG` overrides, because their raw wire dumps bypass header redaction;
application tracing remains available. Embedders must apply the same collection
policy. Ambient credential files/helpers are not implemented.

`--image-cache-dir` defaults to `/var/cache/bpfman`. Its independent writer lock
protects Rust's `images-v1` collection. A single envelope atomically publishes
metadata and bytecode. `Always` refreshes, `IfNotPresent` uses the cache or pulls,
and `Never` requires the cache without network access. Corrupt entries fail closed
for cached policies; `Always` can refresh them. Go cache entries use a separate
format. Cache eviction and abandoned-stage recovery are not implemented.

Cache admission records the signing policy and original top-level digest, including
the index digest for a multi-platform image. Policy identity includes the selected
trust-root bytes and verifier version. A policy change makes `Never` fail without
network access; `IfNotPresent` reverifies the recorded digest and atomically
updates admission without resolving a mutable tag again. Entries predating digest
tracking require a fresh pull. Failed verification and concurrent replacement
preserve the previous or newer complete generation respectively.

`direnv exec . make rust-test-image` runs local-registry cache and native lifecycle
contracts, fixed signed-release verification, offline Go/Rust public/private auth
comparison, TUF rotation/rollback/expiry/concurrency, cache revalidation races and
real signal cancellation during signature reads and trust-writer waits. The normal
`rust-check` includes these contracts without external network access. All ten
remote-image scripts selected by `external=true,image=true` and six image-build
scripts pass against both implementations with verification enabled. Go's external
comparison runs serially to avoid its existing TUF cache contention; Rust also
passes the remote-image lane with 32 slots. Earlier acceptance corrected three legacy `probe` expectations to
bpfman's distinct `kprobe`, `uprobe` and `uretprobe` metadata names. The label
vocabulary is [documented independently of loading libraries](../docs/developer-guide/image-build.md#bytecode-program-labels).
Image mode passes all 134 admitted Rust files / 188 cases using Rust directly,
with verification enabled. The earlier shared Go run passes 100 files / 107 cases, with
one existing clsact-policy skip. A shared shell null-comparison bug exposed by
image provenance has been fixed in the evaluator and static checker without
changing the lifecycle script or disabling checking. Native image building now
removes that fixture-publisher dependency; see below. See the
[broader audit](../docs/design/rust-reimplementation.md#broader-image-mode-acceptance-audit-17-october-2026)
for selectors, case counts and the shared-map BTF fix exposed by that audit.

`image generate-build-args BYTECODE [-o text|json]` and `image build IMAGE
BYTECODE` inspect and publish bytecode without a container runtime, program store,
or image-cache writes. Both accept a single host file, explicit
`linux/arch=BYTECODE` mappings, or `--cilium-ebpf-project DIR`. Every input is
validated for its requested ELF byte order before registry I/O, and the retained
bytes supply both metadata and archive contents. Explicit mappings publish an OCI
index, even with one platform; bare files publish a single host image. Canonical
program/map labels come from the first input, matching Go's build contract.
Foreign-endian objects use a checked metadata-only ELF/BTF reader in the kernel
adapter because Aya's BTF reader assumes native byte order. Actual big-endian
fixtures cover all four big-endian architecture slots and mixed-endian indexes;
the original bytecode remains unchanged.

Publication is deterministic, with epoch-zero timestamps by default and
`SOURCE_DATE_EPOCH` support. A single regular-file tar layer holds each input;
config and layer digests precede manifest publication. Anonymous bearer auth uses
the repository's pull/push scope. Upload locations stay within the destination
registry and repository, and writes refuse redirects. Failed or cancelled uploads
abort their sessions once; failed aborts retain opaque receipts for explicit
`retry_upload_cleanup`. Completed blobs and untagged child manifests belong to
registry garbage collection. A successful final manifest write wins over concurrent
cancellation; a lost response can leave the publication outcome unknown.

`make rust-test-image-build` runs publishing/CLI contracts and the six unchanged
image-build scripts, also included in `rust-check`. The admitted 134 Rust files /
188 cases pass with `BPFMAN_E2E_BYTECODE_SOURCE=image` using Rust directly for
publication and loading. Both implementations pass all ten remote-image scripts;
Go also passes the six image-build scripts. Explicit private authentication is
implemented; ambient credentials and cache eviction remain separate work.

## Persistence scope

SQLite is the only current persistent backend. Sections below retain historical
measurements and Go comparisons from the two-backend experiment; references to
JSON stores or two backend batches in that evidence describe the implementation
before its retirement. The live CLI, workspace and normal gate use SQLite.
JSON command output and trace files remain supported.

## Store backend boundary

The CLI composition root supplies SQLite implicitly, opens a runtime-bound
`ActiveStore`, and moves it into `Bpfman`. There is no `--store` flag or
`BPFMAN_STORE` binding. The experimental JSON backend and its dedicated filesystem
publisher have been retired. Existing JSON state is refused unchanged; no
conversion or automatic replacement is performed.
Shared behavioural tests construct the same application instance in setup.
Runtime depends
on `bpfman-store`, with no direct or transitive dependency on a concrete backend.
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
in these contracts. SQLite implements these operations. Tests use the real store;
fault decorators intercept selected operations without implementing persistence.

`ActiveStore` retains the handle returned by startup and clones it for each
caller without reopening the backend. Clones share backend resources, not a
transaction or frozen snapshot. SQLite retains one idle read connection; a
concurrent read opens a separate connection if that slot is occupied. Its mutex
protects only checkout/return, never query execution. Sequential reads reuse the
connection, including after a failed read. Mutation
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

`LinkStore` and `LinkReader` support attachment lifecycle operations: allocate
pending intent and its canonical pin path, finalise with the kernel link ID,
observe, and conditionally delete using opaque receipts. The shared contract
suite exercises the real store through an injected `ActiveStore`, including failed
finalisation, stale receipts, lock-free reads, and program deletion blocked by
pending or finalised links. SQLite retains Go's schema version 2. The runtime
enforces release of the managed attachment reference before consuming a
link-deletion receipt. Tracepoint unload observes pending and finalised links before
mutation and removes their pins and records under the same writer lock as program
teardown.
Pending intent is cleaned using its canonical pin path; a missing kernel ID does
not prevent cleanup. A present pin must still match the program and link type.

SQLite occupies `<runtime>/db/store.db` and retains Go schema 2. The generic
contracts, consuming receipts and backend-parameterized scenarios remain intact.
A future adapter belongs behind those contracts and must run the same lifecycle,
CLI and DSL scenarios. Only composition setup selects the implementation.
Architecture tests guard the normal dependency closures against concrete adapter
and SQL dependencies leaking into the runtime or store contracts.

Shared store errors expose portable categories and preserve private diagnostic
sources. `UnloadReport<S>` and `UnloadError<S>` retain the selected backend's
receipt types; explicit retries use the instance's backend and acquire writer
authority internally. Backends must reject foreign or stale evidence.
Failed-load cleanup needs only filesystem receipts and never reopens or retries
the store.

Tests use real SQLite through production read/unload operations and
the production load interpreter with fake kernel/filesystem effects. They check
open failures before acquisition, atomic commit failure with independent cleanup
failures, successful commit without compensation, retained receipts, unchanged
faults, and wrong-runtime rejection. Existing SQLite, exhaustive
compensation, and real-kernel compatibility gates remain in place.

Generic live-kernel tests inject failures at store operations through a test-only
`Faults<S>` decorator. They call public runtime operations with real kernel and
filesystem effects; no production failure flags or persistence edits are needed.
The same `lifecycle::exercise<S>` scenarios remain generic and run against SQLite.
CLI and unchanged DSL tests inspect returned JSON and runtime artifacts, with no
database queries. `make rust-test` and `make rust-check` build the fixtures and
run all kernel and unchanged DSL tests as part of the normal gate. Cargo's runner
uses passwordless `sudo -n` to execute the kernel test binary in a private mount
namespace. Complete gates run independent contracts on eight workers within each
backend; global inventory, corpus and Go stages remain serial. Other test binaries
run as the invoking user. Missing privileges fail the gate. The focused kernel
and observation targets select
subsets of the same suite. The harness and executable-selection tests are Rust
integration tests.

SQL fixtures and DDL/DML remain in tests explicitly testing SQLite. Adapter tests
verify transaction rollback, constraints, orphan map-set absence, invalid records,
and conditional deletion. SQL statement and malformed-row unit matrices use
connection-local in-memory SQLite with the same Go schema as file fixtures;
timestamp normalization is tested without a database. Shared file fixtures
initialize the schema/version table and seed data in separate transactions,
retaining ordinary commits while avoiding one synchronization per setup statement.
Opening, byte-preservation, WAL, independent-reader/writer and receipt contracts
continue using actual database files. `tests/sqlite_compatibility.rs` checks the
selected SQLite CLI against Go's schema. These format-specific assertions do not
belong in the generic lifecycle tests. Sharing persisted state with Go is checked separately
in `tests/kernel/go_compatibility.rs`. The division preserves the previous coverage:

| Scenario | Coverage |
| --- | --- |
| Commit, deletion, GC, and post-commit read failures with live programs | Generic `tests/kernel/lifecycle.rs`, faulting store operations |
| Foreign pin/map identity, failed output delivery | `tests/kernel/cli.rs`, public CLI and artifact observations |
| Go/Rust shared-state observations and unload in both directions | SQLite-specific `tests/kernel/go_compatibility.rs` |
| Unsupported stored relationships, noncanonical stored paths, partial writes | SQLite adapter tests using the actual Go schema |
| CLI tracepoint lifecycle | Unchanged `TestTracepoint_LoadAndGet.bpfman` and `TestTracepoint_LinkRoundTrip.bpfman`, plus `TestTracepoint_UnloadAttached.bpfman`, with SQLite |
| Typed, raw, and nested-shell executable selection | `tests/e2e_selection.rs`, unprivileged Make recipe tests |

The generic lifecycle, unchanged DSL and store-contract suites run against real
SQLite. Fault decorators intercept domain operations and delegate successful
persistence and receipt validation to that adapter. The stateful fake replaces
kernel effects only.

`tests/concurrent_store.rs` exercises four independent readers opening and reading
while a writer holds the runtime lock and publishes successive commits. Each read
sees one complete committed snapshot; separate calls may see newer commits.
Concurrent first starts recheck absence under the writer lock. SQLite uses WAL.

## Telemetry

Effectful crates emit `tracing` spans; the CLI owns collection. Set `RUST_LOG` to
enable structured JSON events on stderr. Command output remains on stdout.
Telemetry is disabled by default. Filters can select a crate or individual module:

```sh
RUST_LOG=bpfman_runtime=debug,bpfman_store_sqlite=debug,bpfman_fs=trace,bpfman_lock=trace \
  rust/target/debug/bpfman --runtime-dir /tmp/bpfman-observe \
  --trace-file /tmp/bpfman-startup.json program list -q
```

The optional timeline file opens in Perfetto or Chrome tracing and uses the same
filter. `--trace-file` enables debug spans when `RUST_LOG` is unset; lock spans are
trace level and need `bpfman_lock=trace`. Existing output files are refused. Each
file covers one CLI process. Trace guards flush on normal and error exits.

Targets include `bpfman_runtime`, `bpfman_store_sqlite`,
`bpfman_fs`, and `bpfman_lock`. Operation spans cover store opening,
creation, snapshots, commits, deletion, publication, and program operations.
`lock.wait` measures acquisition (including timeout/cancellation); `lock.held`
covers the current callback and closing its descriptor. Inherited descriptors may
retain the underlying flock after that scope. Span-close events report busy/idle
time, and acquisition events include `wait_us`. SQLite's `store.connection.open`
span records actual read-connection opening; `reader acquired` events include
`reused` so connection reuse is visible. No record contents or credentials
are logged. Model/core crates remain uninstrumented and pure.

SQLite dispatcher/link/unload writer openings emit `store.connection.open_writer`.
These spans separate adapter work from existing writer wait/ownership and kernel
operation spans; nested span durations must not be blindly added together.

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
the uprobe mount-namespace subprocess helper inherits its writer descriptor.
No privileges are needed for text/quiet listing in a writable temporary runtime.
Loading requires BPF and mount privileges; `/run/bpfman` will normally require sudo.

Listing supports managed table, quiet-ID and JSON output. Text and quiet output
use stored summaries without kernel privileges; JSON adds full records and live
kernel observations. `program get ID [-o text|json]` observes one managed program,
including its maps, statistics, and links. Link observations use the same
record/status shape as `link get` and do not take the writer lock. `--all` and
kernel link state filters remain unsupported. Unload supports the implemented program
families with private or shared maps and pending or finalised links. Dispatcher
cleanup completes before program teardown. Map sets survive while any member
remains; shared-name pins survive while any recorded user remains.

`program load file PATH` and `program load image IMAGE` parse typed requests,
including repeated/comma-separated `--programs`, metadata, globals, application,
nonzero map-owner IDs, text/JSON output requests, and image-specific pull/auth
options. Fentry/fexit/LSM variants carry required load-time targets. Invalid input
exits with status 2. Local tracepoint, XDP, TC, TCX, kprobe, kretprobe, uprobe, uretprobe, fentry, fexit and LSM batches
with private or shared maps and
metadata/application labels are executable, with Go's detailed text output or
JSON load envelope in selection order. Image loads use the same lifecycle after
registry/cache admission, including the configured signing policy.
Explicit map owners and ordinary PinByName maps are supported. PinByName
map-of-maps definitions and section/type mismatches are rejected
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
    BPFMAN_E2E_IMPLEMENTATION=rust \
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
with private maps by default. Every public ELF map is pinned under
`fs/maps/{program_id}`, including maps used by other selections in the object.
An explicit `--map-owner-id` joins an existing map set and creates no new map
container. Exact map IDs are captured in published provenance and copied into
joining programs' provenance, so owner-first unload preserves identity evidence.
Internal data maps remain private to each program and are not pinned separately.
Loading does not attach programs.

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

SQLite creates every selected program and private map set in one transaction. Records retain Go's source path, license,
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
execution against SQLite.

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
when loading; SQLite retains the overrides for later program observations.
The stored ELF remains the original input.

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

The same real-kernel scenario runs against SQLite. It proves counter
execution before detach and quiescence afterwards, replacement refusal, failed
finalisation compensation, blocked unpinning, explicit retries with preserved
history, and cancellation at store boundaries. Private interpreter tests cover
individual forward and cleanup failures, including live-descriptor release.
The CLI exposes `link attach tracepoint PROGRAM_ID GROUP/NAME [-m KEY=VALUE]`,
`link get LINK_ID`, `link list`, and `link detach LINK_ID`. Attach/get/list accept
`-o json`; JSON follows Go's record/status shapes. XDP, legacy TC, TCX and kernel probes are also implemented in the slices below;
list filters and batch detachment remain unsupported. `get_link` observes recorded
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
links, an XDP extension with or without dispatcher links, or a native DEVMAP
egress program. Each must have its
own map set, no other map-set users, and no shared-map-pin registrations.
Other program types, shared state, multiple operands, and `--ignore-missing` are
explicitly unsupported. A missing managed record returns an error without
inspecting or adopting a kernel-only program or creating a database. It does not
yet make Go's additional not-managed versus not-found distinction.

Under one writer scope, store preflight validates canonical artifact paths and
exclusive ownership. Filesystem observation opens existing objects without
creating or mounting collections. It verifies descriptor confinement, types,
hard-link counts and inode identities; a live program pin must match the ID and
tracepoint, extension, or native XDP type, and its map pins must refer to that program's maps. If the
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

Local interface XDP loads use the same preparation, atomic batch commit, compensation,
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
Native DEVMAP egress selections use the same lifecycle with a different kernel
load role; see [egress programs](#xdp-devmap-egress-programs).

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
with SQLite. Run `direnv exec . make rust-test-xdp-modes` for that suite.
Actual hardware offload is unverified.

SQLite atomically publishes the dispatcher header, managed link, and member
using Go schema version 2.
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

Two parallel `TestXDP_PASS_{Drv,Skb}_MultiBuffer` scripts cover public PASS
behaviour with SQLite, using pooled namespaces and the existing packet probe.
They require complete 8014-byte payloads, no duplicates, exact member execution
and fragment/tail/boundary-read evidence. They verify single/two-member execution,
equal-priority ordering, proceed-on stopping, mixed membership at MTU 1500,
restored jumbo operation, both survivors, stable outer identity and last detach.
Actual Go comparison with SQLite failed initial jumbo attachment with `ERANGE`
in both modes, so both scripts are Rust-only.

Run `direnv exec . make rust-test-xdp-frags` for four retained kernel contracts
with SQLite and explicitly requested driver/SKB modes. Two jumbo ping waves
per survivor scenario prove execution after attach/detach publication restoration;
the 8 KB ICMP payloads prohibit IP fragmentation and retain fragment/tail/boundary
counter evidence. Rust also checks exact snapshots, unresolved ownership,
mixed-chain ABI flags, native jumbo rejection, stable outer identity and cleanup.
The unchanged `TestLoad_XDPFragsProgram` normal-MTU script runs in the backend-wide
script batches. These are XDP multi-buffer packets, not IP fragments.
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
SQLite rechecks and publishes within one transaction using Go schema 2.

Run `direnv exec . make rust-test-xdp-store` for the shared store contracts
and backend-specific tests. Coverage includes 1 → 2 → 1 → 0 persistence with
removal of either member, growth to ten slots, capacity refusal, stable IDs,
reopening, stale/foreign receipts, invalid requests, SQLite DML aborts and ignored
writes and malformed membership.
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

The batched `direnv exec . make rust-test-scripts` gate includes these twelve
unchanged Go XDP scripts with SQLite (`rust-test-xdp-corpus` is an alias):

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
or descriptor remains detached from its owning operation. Uprobes use the separate
mount-namespace subprocess helper described below.

Namespace paths are persisted in Go-compatible SQLite columns,
and appear in link details and dispatcher runtime JSON. Rebuilds retain the
established path even when a new attach uses an alias of the same namespace.
Missing or replaced paths refuse new preparation and retained unload retries.
Restoring the original path permits explicit retry with its ownership intact.
Final detach uses owned kernel/pin identities and can clean up after a named
namespace path is removed; non-last detach and attached unload need the stored
namespace path to prepare surviving members or retained program evidence.

Run `direnv exec . make rust-test-xdp-netns` for real-kernel tests with SQLite. The unchanged `TestXDP_NetnsVethPairLinkRoundTrip` and
`TestXDP_NetnsDispatcherRebuild` scripts verify traffic, rebuilding, observations,
and counter quiescence. Shared recovery tests cover identical interface indices
in independent namespaces, alias paths, invalid namespace objects, missing/replaced
paths, publication failure, retained unload, removed namespace paths, caller
isolation, and residue-free teardown. Store contracts exercise namespace
persistence and reject changing a committed namespace during replacement. A
worker-entry failure test verifies that no effect runs or caller namespace changes.

### XDP TX and REDIRECT packet delivery

Run `direnv exec . make rust-test-scripts rust-test-xdp-delivery` for script
acceptance and real-kernel restoration tests with SQLite and
explicitly verified native and SKB ingress modes. Normal forwarding lives in the
eight `TestXDP_TX_*` / `TestXDP_Redirect_*` scripts, including jumbo variants;
the Rust matrix retains post-failure packet evidence.
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

The scripts cover both TX and direct REDIRECT, default stopping and explicit
proceed-on continuation into a DROP member, and removal of either member. When
the final member permits continuation, exhausting the shared dispatcher returns
PASS; the tests also capture this local delivery. Failed attach/detach publication
must restore both the complete snapshot and the packet path. Successful replacement
and survivor rebuilding retain the outer link ID. Last detach restores ordinary
local delivery, and final teardown leaves empty inventories and no owned artifacts.
No production changes were needed for this acceptance slice.

This establishes ordinary and multi-buffer veth frames with direct interface
redirects. The suites below also cover map-based redirects.
CPUMAP/XSKMAP redirects, physical NICs, and actual hardware offload remain
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
SQLite with explicitly observed native/SKB ingress modes. Each covers
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
egress program. The suites below cover multi-buffer forwarding,
broadcast/exclude-ingress flags, ordinary-frame egress PASS/DROP, and DEVMAP_HASH
unicast. CPUMAP/XSKMAP, physical NICs, and hardware offload remain unverified.

### XDP DEVMAP_HASH unicast

DEVMAP_HASH selects an output interface using an arbitrary 32-bit key.
`max_entries` limits how many entries exist, rather than the range of valid keys.
The same namespace-aware Go map-update fixture accepts both map types and applies
the array bounds check only to DEVMAP.

Run `direnv exec . make rust-test-xdp-devmap-hash` for eight real-kernel suites:
SQLite, explicit driver/SKB ingress, and ordinary/genuine multi-buffer frames.
They reuse the DEVMAP forwarding lifecycle with a two-entry hash map and keys
`0x80000001` and `0xffffffff`. The second key names an unused target; packets
must follow only the requested key, including PASS/DROP fallback while that key
is missing. A third insertion at full capacity must fail with `E2BIG`, leaving
both existing entries intact. Existing-key updates at capacity remain permitted.

Each suite covers stopping/continuation, live target updates, deletion and
repopulation, successful replacement, failed attach/detach publication restoration,
either survivor, and last detach. Retained map descriptors and pin IDs prove stable
identity. Exact key-set and value checks additionally preserve the unused entry.
The jumbo cases require full 8014-byte payload capture and independent fragment,
tail-read, and boundary-read counters in each executing member and receiving peer.
Unload removes the owned pin; closing the retained descriptor must allow bounded
kernel reclamation. Final inventories and owned artifacts must be empty.

This extends acceptance for the existing loader and lifecycle. No production API,
persistence schema, Rust dependency, or Aya changes were required. Hash-backed
broadcast, ingress exclusion, and ordinary/genuine multi-buffer native egress
are covered below.

The hash unicast checkpoint passed `direnv exec . make rust-check`: formatting, Clippy,
workspace tests, compile-fail contracts, Rustdoc, 38 shared fake-kernel lifecycle
tests, and all 116 real-kernel tests, with none ignored or skipped. The eight
focused hash suites, Go fixture lint, Makefile lint, and BPF fixture formatting
also passed. Aya and the lockfile remain unchanged.

### XDP multi-buffer forwarding

Run `direnv exec . make rust-test-scripts rust-test-xdp-multibuffer-forwarding`
for ordinary script acceptance and eight independently scheduled real-kernel
restoration suites: direct TX/REDIRECT and DEVMAP, each on SQLite with
explicitly observed native/SKB ingress modes. Together they
cover the direct TX/REDIRECT and DEVMAP lifecycle scenarios above with three
8014-byte Ethernet frames (8000 bytes after the Ethernet header) and MTU 9000 on
all four interfaces. Separate `xdp.frags` fixtures share the ordinary fixtures'
actions and map definitions. Receiving peers use fragment-aware native PASS
members to enable veth transmission and independently inspect forwarded frames.

The packet helper's `packets SIZE` mode fills bytes after the magic/sequence header
with an offset-dependent pattern. Capture checks exact length and every payload
byte, rejects duplicate sequence numbers, and excludes outgoing copies. Every
executing member and receiving peer must increment four exact counters: marked
frames, total length exceeding linear length, correct final-byte helper reads,
and correct two-byte reads across the linear/fragment boundary. A jumbo frame
that was linearized would fail this evidence check even if capture succeeded.
Inactive members must leave all four counters unchanged.

Scripts check default stopping and explicit continuation into DROP, either
survivor, stable outer-link identity, successful replacement, last detach and
complete cleanup. Rust adds failed attach/detach publication restoration. DEVMAP also
checks live target updates, PASS/DROP missing-entry fallback, original map identity
and contents, and eventual reclamation. Neither the runtime nor the persisted
schema needed changes, and no Rust dependency or unsafe code was added.

The verified boundary here is unicast veth forwarding on kernel 6.18.54.
The broadcast suite below establishes the additional fan-out boundary.

The unicast multi-buffer forwarding checkpoint (`f844ddef4`) passed the full `direnv exec . make rust-check`
gate: formatting, Clippy, workspace tests, compile-fail contracts, documentation,
38 fake-kernel lifecycle tests, and all 92 real-kernel tests. NixOS kernel-build
discovery and the optional `KERNEL_DEV` override are documented in
[AGENTS.md](AGENTS.md).

### XDP DEVMAP broadcast and ingress exclusion

Run `direnv exec . make rust-test-scripts rust-test-xdp-broadcast` for ordinary
and jumbo script acceptance plus sixteen real-kernel restoration/lifetime suites:
DEVMAP/DEVMAP_HASH, SQLite, native/SKB ingress, and ordinary/genuine multi-buffer
frames. `direnv exec . make rust-test-xdp-broadcast-hash` selects the eight hash-backed
suites. Eight parallel `TestXDP_{Devmap,DevmapHash}_Broadcast_{Drv,Skb}_` scripts
cover ordinary and 8014-byte frames, ingress exclusion, empty/sparse maps, live
updates, continuation, both survivors and last detach. A map/ifindex-filtered
tracepoint requires zero redirect errors except for exact jumbo cloning rejection;
fragment/tail/boundary counters prove genuine non-linear input and intact forwarding.
All eight passed with SQLite. Ordinary driver scripts are shared with Go.
The ordinary SKB scripts are Rust-only after Go attached in driver mode; the four
jumbo scripts are Rust-only after Go failed to attach the receiving fragment-aware
observer at MTU 9000 with `ERANGE` with SQLite. These are tested topology
boundaries, not a claim about all possible Go SKB/jumbo setups.
Rust retains two post-failure packet waves per scenario, reducing jumbo captures
from 72 to 8 per matrix invocation, just as for ordinary frames. Exact map
contents/identity, restoration, stable outer identity and lifetime checks remain,
including fragment and exact `EOPNOTSUPP` evidence after each publication failure.
Each topology uses three veth pairs with map entries for `in0`, `out0`, and `out1`.
Broadcast sends a copy
to each eligible output. `BPF_F_EXCLUDE_INGRESS` suppresses the `in0` target and
therefore prevents return to `source0`. Capture at both sinks, the sender, and
local ingress verifies exact sequence counts and complete payloads. Sparse maps
and live updates move delivery between sinks without rebuilding the dispatcher.
The optional `RUST_XDP_BROADCAST_FILTER` Make variable selects an individual suite.

The array fixture uses keys `0`, `1`, and `2`; the three-entry hash fixture uses
`7`, `0x80000001`, and `0xffffffff`, all beyond the capacity value. Exact populated
key-set and value checks preserve every hash entry and reject unexpected entries
through live updates, deletion/repopulation, replacement, restoration, and unload.
Both fixtures deliberately supply absent lookup key 99 and PASS fallback.
Broadcast ignores that lookup: an empty map, or a map containing
only excluded ingress, consumes the frame and returns REDIRECT rather than PASS.
Default proceed-on stops before the DROP member even in this case. Explicit
REDIRECT continuation reaches DROP and suppresses all copies; exhausting the
continuing chain returns PASS for local delivery.

Ordinary-sized frames support genuine fan-out. On Linux 6.18.54, native and SKB
broadcast explicitly reject cloning multi-buffer frames with `EOPNOTSUPP` when
more than one destination is eligible. See the
[version-matched kernel clone paths](https://github.com/gregkh/linux/blob/v6.18.54/kernel/bpf/devmap.c).
A single eligible destination still receives the complete jumbo frame and proves
fragment/tail/boundary reads. All executing members retain those fragment checks
when fan-out is rejected. The tests require zero copies and exactly three
`EOPNOTSUPP` redirect events, with no other redirect errors; they do not skip the
multi-buffer cases or claim kernel support for cloning fragments.

A managed tracepoint fixture observes `xdp/xdp_redirect_err`, filtered by the
original map ID and ingress ifindex. Before loading it, the suite verifies
field offsets/sizes against the running tracefs format. This distinguishes the
kernel's explicit restriction from an unexplained drop or a manager regression.
The ordinary source checkout at `v6.12` did not contain these stable-kernel checks;
matching the full running patch version mattered.

Across the script and Rust suites, both packet sizes cover failed attach/detach
publication restoration, successful replacement, stable outer identity and all
map entries, either survivor, map
updates after rebuilding, last detach, eventual map reclamation, and empty final
inventories. The error observer is also unloaded through the normal managed
lifecycle. The Go helper accepts explicit map keys and an additional capture
interface while retaining existing packet/map operations. No production command,
persistence change, Rust dependency, or unsafe block was added.

The next section adds DEVMAP and DEVMAP_HASH egress programs. CPUMAP/XSKMAP, physical NICs, and actual hardware offload remain
unverified.
Multi-buffer fan-out needs kernel support.
The uprobe mount-namespace helper remains a separate attachment-family boundary.

The hash-backed broadcast checkpoint passed `direnv exec . make rust-check`:
formatting, Clippy, workspace tests, compile-fail contracts, documentation, 38
fake-kernel lifecycle tests, and all 124 real-kernel tests, with none ignored or
skipped. This includes all sixteen array/hash broadcast suites and the existing
unicast and egress suites. The eight focused suites in `rust-test-xdp-broadcast-hash`,
Makefile lint, and BPF fixture formatting also passed. No production or Aya changes
were required.
NixOS kernel-build discovery and the optional `KERNEL_DEV` override are documented
in [AGENTS.md](AGENTS.md).

### XDP DEVMAP egress programs

A DEVMAP entry can also name a final XDP filter for its output device. Returning
PASS transmits the redirected packet; returning DROP discards it. These programs
require kernel type XDP and expected attach type `BPF_XDP_DEVMAP`, rather than the
EXT programs used for managed interface dispatchers.

The loader selects that role from `xdp/devmap` or `xdp.frags/devmap` in the captured
ELF and loads it through Aya's public `Xdp::load` API. Private maps, pinning,
publication compensation, observation, and unload/retry reuse the existing
lifecycle. No new store field, dependency, unsafe code, Aya patch, or production
map-editing command is needed. The managed kind remains `xdp`; the kernel kind is
`xdp` for egress and `extension` for interface members. Interface attachment refuses
native egress programs before creating a dispatcher revision, including after
store reopen. Unsupported CPUMAP sections fail preparation before runtime effects.

Eight parallel `TestXDP_{Devmap,DevmapHash}_Egress_{Drv,Skb}_{Linear,MultiBuffer}`
scripts cover observable egress behaviour with SQLite. Six positive scripts
prove ordinary PASS/DROP on both map types and genuine jumbo hash egress; two
array-jumbo scripts preserve the current rejection boundary. They check native
XDP type/ID, refusal of interface attachment after CLI/store reopening, empty-map
fallback, exact packet/counter/context evidence, live updates, invalid-update
preservation, replacement, survivor rebuilding, last detach and continued egress
after managed unload. Existing syntax and the packet probe are reused, with no
serial/exclusive pragmas. Temporary counter-map pins allow post-unload execution
counts without retaining program FDs; cleanup removes them explicitly.

Run `direnv exec . make rust-test-xdp-egress` for sixteen retained kernel
contracts. `direnv exec . make rust-test-xdp-egress-hash` selects the eight
hash-backed suites; `RUST_XDP_EGRESS_FILTER` optionally selects one suite.
The twelve positive suites retain five packet waves each: attach/detach
publication restoration, failed record deletion, explicit unload retry and
map-held DROP lifetime. The four array-jumbo contracts retain role/compatibility,
map contents, dispatcher identity and reclamation checks; their successful
unicast captures now run in scripts. This removes 120 serial captures across the
sixteen contracts. Updating a map takes a program FD; reading it returns the
program ID. The namespace-local Go fixture keeps that distinction explicit.

Map entries retain kernel program references independently of managed pins.
Unload removes the egress program's owned pins and store record while forwarding
continues through a referencing entry. This also survives failed record deletion
and explicit unload retry. Removing the entry releases that program. The second
case unloads an egress program and its map owner while a retained map descriptor
keeps the entry/program alive; closing that descriptor releases both. Bounded
kernel observations verify eventual reclamation, and final managed inventories
and artifacts must be empty.

Four array-map suites load native `xdp.frags/devmap` programs and establish the
current multi-buffer pairing boundary. Aya 0.14's `.extension(name)` override
does not preserve `BPF_F_XDP_HAS_FRAGS`, although its native XDP branch does.
Fragment helpers can still execute against the verification dispatcher, so
earlier jumbo ingress/unicast tests pass; the missing flag appears when Linux
checks DEVMAP ownership against a fragment-capable egress program. Updates fail
with `EINVAL`. Tests require rejection for empty and populated maps, preservation
of map entries and dispatcher identity, zero egress execution, and unchanged
full-payload jumbo unicast without an egress program.

Aya 0.14 exposes no extension load-flag setter or XDP-to-extension conversion;
`XdpMode` controls attachment flags and cannot fix this. TODOs in the adapter and
boundary test record what to revisit when upstream preserves extension fragment
flags. Keep Aya unchanged and retain these checks until positive multi-buffer
array-map egress execution can be implemented through a supported API.

Four additional hash-backed suites prove genuine jumbo `xdp.frags/devmap`
PASS/DROP through the complete egress lifecycle. Linux 6.18.54's
[`map_type_contains_progs`](https://github.com/gregkh/linux/blob/v6.18.54/include/linux/bpf.h#L2151)
omits DEVMAP_HASH, so the ingress extension does not initialize its ownership;
the first native egress update sets compatible fragment ownership.
[`bpf_prog_map_compatible`](https://github.com/gregkh/linux/blob/v6.18.54/kernel/bpf/core.c#L2308)
rejects later egress programs with mismatched fragment flags. The suite requires
that exact `EINVAL` failure, retained program IDs/contents, and unchanged forwarding.
Aya remains unchanged; this does not repair its extension flag handling or enable
array DEVMAP jumbo egress.

Both hash packet sizes use a two-entry map with selected key `0x80000001` and
unrelated target key `0xffffffff`. Exact populated key sets and interface/program
IDs survive updates at full capacity, failed extension/incompatible-program updates,
successful replacement, failed attach/detach publication restoration, surviving
redirect rebuild, deletion/repopulation, and managed egress unpinning/unload retry.
The spare never redirects a missing selected key. All jumbo traffic requires full
8014-byte payload capture and independent fragment, tail-read, and boundary-read
counters in executing ingress/egress and receiving programs, plus ingress/output
interface-context checks in egress.
Deleting the selected entry releases an unpinned PASS program while the unrelated
entry survives. Retaining the map descriptor keeps an unpinned DROP program alive
after map-owner unload; closing it releases both map and program. Final inventories
and managed artifacts must be empty.

Go's current loader forces all managed XDP to EXT and clears its attach type.
Actual comparison with SQLite rejects native egress loading at the context
read (`invalid bpf_context access off=20 size=4`), so all eight new scripts carry
`rust-only=true`. Native DEVMAP egress is beyond that Go path. Go still supports
more of the overall bpfman surface. DEVMAP_HASH unicast acceptance above adds coverage for existing
behavior, as do hash-backed broadcast, ingress exclusion, and ordinary/genuine
multi-buffer egress. Array-map multi-buffer egress and multi-buffer fan-out remain
separate library/kernel boundaries. TC ingress and egress attachment is described below;
multi-member replacement and attached-program unload are supported.

The hash egress checkpoint passed `direnv exec . make rust-check`: formatting,
Clippy, workspace tests, compile-fail contracts, documentation, 38 shared
fake-kernel lifecycle tests, and all 132 real-kernel tests, with none ignored or
skipped. This includes all sixteen array/hash egress suites: twelve positive
ordinary/jumbo suites and four array-map jumbo rejection suites. The eight focused
suites in `rust-test-xdp-egress-hash` and Makefile lint also passed. Upstream Aya,
dependencies, and the persistence formats remain unchanged.

### Injectable kernel lifecycle

`Bpfman<S, K>::new(store, kernel, timeout)` accepts one kernel backend alongside
its real store. Reads, loading, tracepoint attachment, XDP attachment/replacement/detach,
unload, and explicit cleanup retries use that same instance. Operations require
only the capabilities they use: `ProgramObservations`, `LinkObservations`,
`ObjectLoader`, `ProgramResources`, `ProgramLoad`, `TracepointLinks`, or
`XdpLifecycle`, or `TcLifecycle`. Associated types keep live handles and cleanup receipts opaque;
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

The Cargo test runner gives this suite and the privileged kernel binary separate
owned temporary directories on
`/dev/shm` when it is writable tmpfs. Real stores, files, locks and synchronization
calls remain enabled. If tmpfs is unavailable, the runner creates an owned
directory under the usual `TMPDIR` (or `/tmp`). Other test targets retain their
incoming temporary directory. The runner reports its fixture scope and requires
it to be empty before removal after the suite, including on failure. Residue
fails cleanup and leaves the owned directory available for inspection.

Run this focused suite with:

```sh
direnv exec . make rust-test-kernel-fake
```

To exercise the default-filesystem fallback explicitly:

```sh
direnv exec . env BPFMAN_FAKE_KERNEL_TMPFS=0 TMPDIR=/tmp make rust-test-kernel-fake
```

Real-kernel targets also prefer tmpfs inside their private mount namespace.
`BPFMAN_KERNEL_TMPFS=0` selects their disk-backed comparison lane. Both scopes
use the same cleanup and signal-forwarding helper; production persistence and
synchronization calls stay enabled. Standalone tracepoint teardown holds a
validated link FD across unpinning, then closes it after the pin descriptors,
preventing immediate reattachment from racing queued link release. XDP attached
unload closes its owned freplace links through the same direct-close ordering.
XDP/TC tests check program-ID absence immediately when no additional references
remain. A deliberately held program FD proves presence before close and immediate
absence afterward. Physical map reclamation and map-held native egress references
remain asynchronous kernel lifetime contracts.

It also runs in `rust-check`, alongside operation-level fault tests, filesystem
confinement tests, the real-kernel tests, and the unchanged admitted DSL corpus
with SQLite. The fake checks orchestration and simulated ownership; the real
kernel tests establish verifier, syscall, and kernel lifetime behaviour.


## Measuring privileged test time

Development and inherited test builds compile `aya-obj` with optimization level
two because Aya eagerly parses kernel BTF on loader construction. Workspace code
keeps its normal debug profile. Cargo's overall `unoptimized` summary still
applies to the workspace, so record dependency profile settings when comparing
timings; see the measurements in the design document.

Set `BPFMAN_KERNEL_TIMINGS=1` through the normal Make targets:

```sh
direnv exec . env BPFMAN_KERNEL_TIMINGS=1 make rust-check > /tmp/bpfman-rust-check.log 2>&1
rg 'kernel-timing' /tmp/bpfman-rust-check.log
```

Every kernel test has a monotonic timer covering its complete body and teardown.
Opt-in TSV records contain `kernel-timing`, test name, elapsed seconds and
`completed`/`failed`; they bypass libtest's captured success output. Timings do
not control concurrency. `kernel-test-start` adds process ID, test name and
monotonic seconds since that process's timing origin; `kernel-test-end` adds
the same fields and outcome. Pair by process ID and test name to reconstruct
worker occupancy and the elapsed interval, including teardown. Each backend
invocation has its own clock origin. Missing end records indicate an incomplete
run; completion telemetry does not replace the runner's exit status.
Retained TX/direct REDIRECT, DEVMAP unicast and
broadcast contracts run independent fixtures on two synchronous scoped workers.
Complete suites discover all kernel cases and run script acceptance separately
from retained contracts. Corpus, legacy Go and global dispatcher-inventory checks
remain serial. Independent retained contracts run on eight threads within each
backend; `BPFMAN_KERNEL_TEST_THREADS=4`, `2` or `1` selects a lower limit. Backend
stages remain sequential. Forwarding uses one fixture worker when top-level tests
are concurrent, so nesting does not multiply the eight-fixture limit. Focused
Make targets retain their serial outer runner and two forwarding fixture workers.
The script cohorts use one Go runner per backend and its parallel scheduler:
isolated acceptance and shared-runtime coverage overlap within 32 slots, with
shared-runtime execution additionally capped at four scripts. The existing
Make override, for example `make rust-test-scripts PARALLEL=16`, selects the
script limit independently of `BPFMAN_KERNEL_TEST_THREADS`. The privileged runner
preserves it, and the script timeline records the actual configured limit.
The corpus also emits `script-timing` rows with backend, runtime scope,
implementation and the Go runner's existing per-script elapsed results.
`script-batch-timing` records each complete runner invocation separately.
Script durations overlap because scripts run in parallel;
do not sum them as batch wall time. Build and userspace time are outside the
per-kernel-test records. A failing
process that aborts rather than unwinds cannot emit its final timing.

Ordinary XDP restoration now sends three marked Ethernet frames and waits for
all three intact frames at its private veth receiver, with a one-second failure
bound. This removes the unanswered ping timeout. Delivery, broadcast and egress
captures retain their 300 ms windows for unexpected or duplicate traffic.

Set `BPFMAN_E2E_SCRIPT_TIMELINE=/tmp/bpfman-scripts.jsonl` to append the Go
runner's optional JSONL timeline through Make or the Rust gate. `suite_start`
records actual `-test.parallel` and `GOMAXPROCS`; `script_queued`, `script_start`
and `script_end` separate scheduler waiting, execution and cleanup. `RuntimeScope`
identifies the cohort on every script marker, and end markers carry a
`passed`/`failed`/`skipped` outcome. Isolated end markers follow residue checks and
unmounting. Shared script end markers precede cohort cleanup; `runtime_cleanup`
reports inventory/artifact verification and normal unmount separately from body
exit codes. `cohort_end` includes all child tests and runtime cleanup. Isolated cleanup waits at most five seconds
for `EBUSY` mount references, using normal unmount; persistent holders fail.
`script_process` records runtime scope,
wall milliseconds, OS-reported user/system CPU milliseconds, output byte count
and exit code. It does not record command arguments or captured output.
An exit code of zero records the script body; the suite must also pass its
subsequent cleanup checks.
CPU includes waited-for descendants on Linux; it is not a count of blocked time.
CLI tracing remains separately opt-in with `RUST_LOG` and `--trace-file`.
Forwarding and broadcast contracts additionally emit `kernel-phase-timing`
records with test name, phase, seconds and outcome. TX/direct REDIRECT
(`delivery.*`) and DEVMAP unicast (`devmap.*`) separate setup, attach/detach
restoration, packet observation, survivor transitions, chain reconstruction,
managed teardown and fixture teardown. Packet observation includes counter reads
and the unchanged 300 ms capture window; fixture teardown includes namespace
deletion and the outer context's unmount/temporary-directory cleanup. These
phases do not overlap within one successful fixture, but overlap across workers.
Broadcast phases retain their combined restoration/survivor measurements and
exclude outer fixture teardown. Workers inherit the parent test name for phase
attribution; `forwarding.worker` measures each complete worker, including its
fixture teardown. Worker and fixture phase totals can exceed whole-test wall
time. Whole-test durations also overlap within concurrent backend stages.
Use each libtest stage's reported elapsed time for wall-time comparisons; do not
sum whole-test or nested worker/fixture records as elapsed time. `kernel-stage`
reports each stage's thread limit and selected case count, and
`kernel-suite-counts` verifies complete discovery/selection.

The contention/isolation checkpoint's complete timed `rust-check lint-make`
gate passed all 96 kernel tests in 540.19 s (9m00s), versus 782.36 s (13m02s)
previously. Acceptance across both stores, including the explicit shared subset,
took 126.418 s versus 309.472 s. Broadcast contracts took 82.874 s versus
130.351 s. All 84 scripts passed per isolated backend, and four shared-runtime
scripts passed per backend; every runtime's inventories/artifacts were checked.
These are local samples, not a controlled benchmark. The
[design follow-up](../docs/design/rust-reimplementation.md#script-contention-isolation-and-broadcast-fixture-reuse-9-october-2026)
records lock/CPU profiles, phase timings and the Go comparison.

The subsequent TX/direct REDIRECT and DEVMAP unicast fixture-reuse gate passed
the same complete checks and all 96 kernel tests in 467.88 s (7m48s).
All 84 isolated scripts and four concurrent shared-runtime scripts passed per
store. The 20 affected forwarding contracts took 144.647 s, versus 216.462 s;
setup is shared across both survivor choices with unchanged restoration
assertions and capture windows. Final forwarding phases attributed 69.806 s to
packet observation, 36.931 s to setup, 6.619 s to managed teardown and 0.187 s to
outer fixture teardown. These are local samples. The
[forwarding follow-up](../docs/design/rust-reimplementation.md#txdirect-redirect-and-devmap-unicast-fixture-reuse-9-october-2026)
records the complete matrix and validation evidence.

The bounded-parallelism gate passed all 96 kernel tests and both stores' complete
script batches in 368.81 s (6m09s), versus 467.88 s (7m48s). The 36 affected
forwarding/broadcast contracts fell from 227.230 s to 122.383 s. Two scoped
workers overlap independent fixtures; every restoration/lifetime assertion and
300 ms capture window remains. Both stores again passed all 84 isolated scripts
and four shared-runtime scripts. Formatting, Clippy, userspace/compile-fail
contracts, Rustdoc and Makefile lint passed. These are local samples; the
[parallel-fixture follow-up](../docs/design/rust-reimplementation.md#bounded-parallel-forwarding-fault-fixtures-9-october-2026)
records the concurrency audit, phase counts and full comparison.

The concurrent-stage/named-case gate subsequently passed all 96 discovered
kernel tests exactly once in 213.75 s (3m34s), versus 368.81 s (6m09s).
Independent contracts run four at a time within each backend; global dispatcher
inventory checks, script batches and the legacy Go runner remain serial.
Retained contract wall time was 96.87 s; acceptance was 116.83 s. Both stores
passed all 84 admitted files, every named DEVMAP case and four shared-runtime
scripts, including residue verification. All assertions and capture windows
remain. Go shell tests/lint, runner contracts and the full Rust gate passed.
An exploratory duplicate stimulus report did not recur in 18 stress contracts or
the final gate; diagnostic output and the investigation note remain. The
[stage/case follow-up](../docs/design/rust-reimplementation.md#concurrent-kernel-stages-and-named-script-cases-9-october-2026)
records the measured stages and case counts. These are local samples.

The expanded broadcast/TX/direct REDIRECT cases and 32-slot isolated acceptance
gate passed in 193.25 s (3m13s), with all 96 kernel tests, all 84 files and all
64 named cases per store. Script acceptance fell to 95.26 s; retained contracts
took 97.97 s. Shared-runtime coverage still runs four concurrent scripts per
store. Bounded normal bpffs release addresses busy references without accepting
persistent holders; 20 repetitions of both fast kernel contracts passed. Go
passed all eight cases from the four affected shared scripts. The full Rust gate,
Go lint and explicit e2e runner lint passed. See the
[expanded-case follow-up](../docs/design/rust-reimplementation.md#expanded-packet-cases-and-script-throughput-9-october-2026)
for timings, the failed-gate investigation and remaining scheduling costs.


The overlapping-cohort follow-up passed all 96 kernel tests in 163.20 s (2m43s),
versus 193.25 s (3m13s). One runner per backend overlaps isolated acceptance with
four scripts on an owned shared runtime; acceptance fell from 95.26 s to 67.36 s.
All 257 script processes and 250 owned-runtime cleanup outcomes passed, with
unchanged fault/phase counts. Rust checks, runner contracts/lint and corrected
Make lint passed. Negative fixtures proved cleanup failures remain visible after
zero script exits. See the
[cohort follow-up](../docs/design/rust-reimplementation.md#overlapping-script-cohorts-10-october-2026)
for scope ownership, telemetry outcomes, contention and validation evidence.
These are kernel-stage local timings, not total gate durations.

The eight-worker retained-contract follow-up passed the full gate with all 96
kernel tests in 150.41 s (2m30s). Retained stage time fell from 104.45 s in a
repeated four-worker baseline to 68.96 s (34%); script acceptance took 81.40 s in
this sample. All test start/end records paired and every fault/phase count was
unchanged. All 257 script processes and 250 owned-runtime cleanup outcomes
passed. Eight-worker occupancy now shows a scheduling tail around the longest
jumbo forwarding groups. See the
[worker timing follow-up](../docs/design/rust-reimplementation.md#eight-worker-retained-contracts-11-october-2026)
for the audit, baseline variation, validation and remaining scheduling opportunity.

The jumbo scheduling follow-up splits the four combined delivery/DEVMAP tests
into eight independent contracts. The full gate passed all 100 kernel tests in
139.55 s (2m20s), with the identical normalized fault/phase matrix. Matching
contract-only runs took 60.32 s before and 60.48 s after; reduced idle worker
capacity has not established a wall-time saving. Most of the full-stage change
came from script-time variation. The shared packet helper also fixes a baseline
failure caused by foreign frames queuing before interface bind: capture remains
disabled until binding its interface, preserving all assertions and windows.
Both stores' complete acceptance, four shared scripts against Go, Go lint and
the full Rust/Make gate passed. See the
[jumbo scheduling follow-up](../docs/design/rust-reimplementation.md#separately-scheduled-jumbo-forwarding-contracts-11-october-2026)
for reproduction evidence, timings and the remaining TX/direct REDIRECT tail.


## Script parity through the Go runner

Scripts admitted against Rust carry `#pragma labels={"rust":"ok"}`. There are
84 admitted scripts: 52 shared Go/Rust scripts and 32 Rust-only scripts,
out of 171 scripts in total. Eight DEVMAP files declare four named cases each;
eight broadcast and eight TX/direct REDIRECT files declare two each. The 84-file
isolated corpus therefore executes 124 script processes with SQLite: 60 plain
scripts and 64 named cases. Each case has its own runtime, pooled network, timeout
and residue checks. Direct shell execution still runs the complete matrix. See the
[case runner guide](../cmd/bpfman-shell/USER-GUIDE.md#running-and-checking-scripts).
A Rust-only script additionally declares:

```bpfman
#pragma labels={"rust":"ok","rust-only":"true"}
```

`BPFMAN_E2E_IMPLEMENTATION` explicitly declares `go` (default) or `rust`. The
runner skips Rust-only scripts for Go even when the label selector requests them.
Selecting a Rust-looking executable path does not change this admission rule.
Only shared scripts establish Go parity. Rust-only labels admit Rust coverage;
they do not by themselves establish that Go lacks the behaviour. No new DSL
syntax is required.

The eight DEVMAP array/hash scripts cover driver/SKB modes, ordinary and genuine
multi-buffer frames, PASS/DROP missing-entry fallback, continuation, live updates,
both survivors and map identity. They passed concurrently in approximately
40 seconds per backend, with empty final inventories. Private topologies reuse
pooled namespaces; no serial/exclusive label is needed. Both survivor choices
share the forwarding preamble. Publication-failure restoration remains in Rust,
with one attach-restoration probe and both detach-restoration probes per shared
fixture. TX/direct REDIRECT contracts use the same fixture reuse. Every action,
continuation mask, store, ingress mode, frame shape and map kind remains covered.
The removed first member is reattached between survivor choices while retaining
the surviving tail, outer link and, for DEVMAP, exact map identity/contents.

The CLI/delivery migration admitted three unchanged metadata/global-data scripts,
added shared `TestProgram_FileLifecycle`, and moved TX/direct REDIRECT into eight
parallel scripts using the same delivery helper. The nine new scripts passed
in about 18 s per Rust backend. File capture, complete record/get/list round trips,
quiet listing and ordinary unload replace duplicate Rust CLI assertions;
preflight effects, provenance/foreign-pin ownership and post-commit output failures
remain Rust integration contracts.

Go also passed the file-lifecycle and ordinary driver TX/REDIRECT scripts. The
other six delivery scripts are Rust-only after a direct comparison: the requested
SKB mode attached as driver, and jumbo observer attachment failed on the receiving
veth. Rust verified both modes and genuine multi-buffer forwarding. The earlier
DEVMAP labels alone do not establish a Go incompatibility. Unload's missing-ID
diagnostic and `--ignore-missing`, section-type mismatch diagnostics/unsupported
families, and the kprobe verifier-log script remain recorded parity gaps in the
[migration audit](../docs/design/rust-reimplementation.md#cli-loadunload-and-txdirect-redirect-follow-up).
Ordinary DEVMAP broadcast adds four parallel scripts: DEVMAP/DEVMAP_HASH in driver
and SKB modes. They reuse the existing fourth-interface capture support and share
the setup for both survivor choices. Both driver scripts are shared after Go
comparison; the two SKB scripts are Rust-only because Go attached in driver mode.
Four additional jumbo broadcast scripts now cover both map types and ingress
modes with full 8014-byte payloads, fragment-read counters and exact filtered
`EOPNOTSUPP` evidence for multi-target cloning. They passed on both Rust stores;
Go failed receiving-observer attachment with `ERANGE` on both, so these wrappers
are Rust-only. Native egress and jumbo PASS/mixed membership have also migrated. See the
[jumbo broadcast follow-up](../docs/design/rust-reimplementation.md#jumbo-devmap-broadcast-follow-up)
for the assertion mapping and retained Rust guarantees.
No production behaviour was changed for these testing slices.

Jumbo PASS adds two parallel driver/SKB scripts for stopping, mixed membership,
restored jumbo operation and both survivors. Both passed in about six seconds
per backend; Go failed first jumbo attachment with `ERANGE` with SQLite.
The scripts are Rust-only. Rust retains publication-restoration traffic and
internal dispatcher flags; 44 duplicate jumbo ping waves and eight ordinary
ping waves moved out of the serial contracts. See the
[PASS follow-up](../docs/design/rust-reimplementation.md#jumbo-pass-and-mixed-fragment-testing-follow-up)
for the assertion mapping and comparison limits.

Build the binaries and fixtures as the invoking user:

```sh
direnv exec . make rust-build build-e2e-scripts e2e-kmod-insmod
```

Run the tagged set in one Go-runner invocation, with its existing interface pool
and parallel scheduling, in a private mount namespace and fresh runtime:

```sh
direnv exec . sudo -n unshare --mount --propagation private -- \
  make run-e2e-scripts \
  BPFMAN_UNDER_TEST=rust/target/debug/bpfman \
  BPFMAN_E2E_IMPLEMENTATION=rust \
  BPFMAN_RUNTIME_DIR="$(mktemp -d)" \
  BPFMAN_STORE=sqlite \
  BPFMAN_E2E_BYTECODE_SOURCE=file \
  BPFMAN_E2E_CLSACT_RECLAIM=true \
  BPFMAN_E2E_SCRIPT_SELECTOR='rust=ok,!external'
```

The Go runner still uses `BPFMAN_STORE=sqlite` for its own harness. Batches run
sequentially because each runner holds the shared suite lock. Scripts within each
batch run concurrently, subject to their serial/exclusive pragmas.
The full `rust-check` gate runs the SQLite `script_corpus` test.
Each runs all 84 admitted scripts with `BPFMAN_E2E_ISOLATED_RUNTIME=1`, using
fresh per-script stores and bpffs mounts. The runner checks every script's empty
program/link/dispatcher inventories and artifact collections before unmounting,
including on script failure. Script assertions and pooled network parallelism
remain unchanged. The same runner concurrently executes an additional shared
cohort: file lifecycle, XDP fill/drain/refill, XDP lifecycle and TC lifecycle.
The four scripts share one owned runtime, checked and normally unmounted after
all four finish. This retains concurrent writer/store/dispatcher coverage in the
gate without a second invocation or suite-lock contention. Mixed cohort test
names include `/isolated/scripts/` or `/shared/scripts/`.
Normal Go `make run-e2e-scripts` remains shared by default; the same isolation
opt-in is available for either implementation. `BPFMAN_E2E_SHARED_SCRIPTS` names
comma-separated `.bpfman` basenames for an additional shared cohort when isolation
is enabled. Admission still applies; missing, duplicate or unselected names and
admitted exclusive scripts fail before execution. Without the cohort setting,
existing test names and scheduling stay unchanged. Leave isolation unset to
stress the full corpus on one store. Backend batches still run sequentially.
The old individual Rust script
wrappers have been removed. `make rust-test-scripts` runs these batches directly;
`rust-test-xdp-corpus` and `rust-test-observation` also select this acceptance gate.
Corpus parity takes priority over further Rust-specific packet scenarios.

The first migration checkpoint passed `direnv exec . make rust-check`: formatting,
Clippy, userspace and compile-fail contracts, documentation, and all 98 kernel
tests, with none failed or ignored. The kernel suite took 1420.39 seconds
(23m40s), compared with 1834.78 seconds (30m35s) at the preceding checkpoint.
The lower test count reflects replacing individual script wrappers with two
batches, each verifying all 50 selected scripts and final cleanup. These local
timings are not a controlled benchmark.

The CLI/delivery follow-up also passed the complete `direnv exec . make rust-check`
gate: formatting, Clippy, userspace/compile-fail contracts, Rustdoc and 98 kernel
tests, none failed or ignored. Both backend batches verified all 62 admitted
scripts and empty final inventories/artifacts. The kernel stage took 1358.95 s
(22m39s), compared with 1420.39 s (23m40s) at the preceding checkpoint. These are
local run timings. Makefile lint and formatting of the new DSL sources passed.

The ordinary broadcast follow-up passed the full `direnv exec . make rust-check`
gate: formatting, Clippy, userspace/compile-fail contracts, documentation and all
98 kernel tests, none failed or ignored. Each backend batch checked all 66 admitted
scripts and empty final inventories/artifacts. The kernel stage took 1180.41 s
(19m40s), compared with 1358.95 s (22m39s) at the CLI/delivery checkpoint. These are
local timings, not a controlled benchmark. Makefile lint and DSL formatting passed.

The jumbo broadcast follow-up passed the complete `direnv exec . make rust-check`
gate: formatting, Clippy, userspace/compile-fail contracts, Rustdoc and 98 kernel
tests, none failed or ignored. Both backend batches verified all 70 admitted
scripts and empty final inventories/artifacts. The kernel stage took 1124.80 s
(18m45s), compared with 1180.41 s (19m40s) at the ordinary broadcast checkpoint.
These are local run measurements, not a controlled benchmark. Makefile lint and
canonical DSL formatting also passed.

The egress testing migration passed the complete `direnv exec . make rust-check`
gate: formatting, Clippy, userspace/compile-fail contracts, Rustdoc and all 98
real-kernel tests, with none failed or ignored. Both backend batches verified
all 78 admitted scripts and empty final inventories/artifact collections. All
sixteen retained egress contracts passed. The kernel stage took 986.98 s
(16m27s), compared with 1124.80 s (18m45s) at the jumbo broadcast checkpoint.
These are local run measurements, not a controlled benchmark. Makefile lint and
canonical DSL formatting also passed.

The jumbo PASS/mixed-fragment follow-up passed the complete
`direnv exec . make rust-check` gate: formatting, Clippy, userspace/compile-fail
contracts, Rustdoc and all 98 kernel tests, none failed or ignored.
Both backend batches verified all 80 admitted scripts and empty inventories and
artifacts; all four retained PASS restoration/ABI contracts passed. The kernel
stage took 978.34 s (16m18s), compared with 986.98 s (16m27s) at the egress
checkpoint. These are local timings, not a controlled benchmark. Makefile lint
and canonical DSL formatting also passed.

The ordinary lifecycle/timing follow-up passed the full
`direnv exec . env BPFMAN_KERNEL_TIMINGS=1 make rust-check lint-make` gate:
formatting, Clippy, userspace/compile-fail contracts, Rustdoc, Makefile lint and
all 96 kernel tests, none failed or ignored. Each backend batch verified all 84
admitted scripts and empty final inventories/artifacts. Four parallel scripts
replace ordinary XDP mode/continuation/survivor traffic and TC lifecycle/CLI
acceptance. Two duplicate TC CLI kernel wrappers are removed; fault, ownership,
foreign-filter coexistence and retry contracts remain.

The kernel stage took 782.36 s (13m02s), versus this session's 1138.08 s (18m58s)
baseline. Serial contracts fell from 751.592 s to 472.738 s. Batch times were
159.025 s (JSON) / 150.447 s (SQLite), reversing their baseline ordering. An
identical isolated script took about 14.1 s / 14.5 s, while its concurrent-batch
elapsed time was 127.39 s / 133.34 s. This suggests profiling shared writer and
process/scheduling costs next; it does not establish one backend as uniformly
faster. Local timings are not a controlled benchmark. Full coverage mapping,
per-group timings and Go comparison are in the
[ordinary lifecycle follow-up](../docs/design/rust-reimplementation.md#ordinary-lifecycle-and-timing-follow-up).

`make rust-test-unit` runs library and binary unit targets without integration
targets or doctests. Its 92 tests reported approximately 0.60 seconds of aggregate
execution, excluding compilation and Cargo startup. Process-wide signal fixtures
now live in the separate `signal_policy` integration target. Pure validation and
state transitions stay in Rust; filesystem, persistence, process and kernel
contracts are integration tests and should not be described as unit tests.
The remaining migration inventory is in the
[testing audit](../docs/design/rust-reimplementation.md#testing-migration-audit-8-october-2026).

## TC ingress and egress attachment and unload

A managed TC classifier loads as EXT against an unpinned native TC dispatcher.
The first attachment on either hook stages a new dispatcher and slot-zero
freplace link, then installs a legacy netlink filter at priority 50. Member `--priority` remains a
separate operator field. Aya remains unchanged; explicit netlink options avoid its
modern-kernel TCX default. The public API returns the actual kernel-assigned filter
handle, which is stored with the dispatcher and checked against its program ID
before deletion.

```sh
bpfman program load file tc.o --programs tc:stats -o json
bpfman link attach tc PROGRAM_ID eth0 ingress --priority 50 -o json
bpfman link detach LINK_ID
bpfman program unload PROGRAM_ID
```

`--netns /run/netns/NAME` selects an explicit namespace. Retained namespace
handles and disposable synchronous worker threads keep mutation in the selected
namespace. `--proceed-on` accepts signed TC actions by name, including `unspec`,
`ok`, `shot`, `pipe`, and `dispatcher_return`; default continuation is `pipe` plus
`dispatcher_return`. The dispatcher ABI shifts every code by one, so `unspec` (-1)
uses bit zero and dispatcher-return (30) uses bit 31. An explicitly continued SHOT
returns the dispatcher's final OK; a SHOT under the default mask drops the packet.

`link get/list` report stored TC attachments and actual freplace identities.
Each ingress or egress point supports one to ten members, ordered by priority,
unattached before attached, then program name. Attach and non-last detach stage a complete
new revision and replace the native dispatcher at the existing filter handle.
Survivors retain managed link IDs and operator fields; slots and kernel IDs change
together. `dispatcher get tc-ingress NSID IFINDEX`,
`dispatcher get tc-egress NSID IFINDEX` and dispatcher list expose full stored membership. Direction is part of the typed
attach-point identity and every store, pin and filter selection, so both chains
can coexist on one interface without changing the other chain during rebuild or
drain. Aya's public `SchedClassifierLink::attached` and
`SchedClassifier::attach_to_link` perform the filter replacement without patches.

Failed publication restores the old chain before staged cleanup. Failed
restoration retains both revisions and native target handles; retirement errors
expose the committed snapshot. `retry_tc_cleanup` performs one explicit recovery
pass and retains every actual restoration/cleanup attempt. Cancellation before
publication compensates acquisitions; an in-flight commit decides its own outcome.

`program unload PROGRAM_ID` also detaches all TC links. Admission validates every
attachment before destructive effects, then consumes exact filter, dispatcher and
conditional record receipts under the same writer lock. Non-last removal rebuilds
survivors; repeated attachments of one program use the latest committed revision.
A failed member blocks later members of that dispatcher, while independent
interfaces and hooks each receive one pass. Failed prerequisites retain
ownership and block program/map/bytecode removal; `UnloadReport::tc_attempts()`
exposes each link's cleanup history. `retry_unload` resumes only unresolved work;
completed links are not detached again. Retry admission may be cancelled without
losing receipts, while an admitted pass finishes despite later cancellation. A new
attachment created between passes blocks program teardown until explicitly handled.
Retained reports can be retried using a reopened application; recovering a report
lost on process exit remains future work.

Clsact ownership lives in a confined, singly linked one-byte receipt at
`<runtime>/tc/dispatcher_<nsid>_<ifindex>_<revision>`, separate from operator metadata.
Egress evidence uses `egress_dispatcher_<nsid>_<ifindex>_<revision>` in the same
collection. Fresh revisions copy established ownership, and cleanup removes only
its own receipt. First attach on the other hook validates the existing managed
dispatcher and copies its qdisc ownership under the writer lock. Either drain order
can reclaim the shared owned qdisc when both hooks become empty. Borrowed qdiscs
remain borrowed.
Reopened detach preserves pre-existing empty clsact qdiscs. A created clsact is
reclaimed only when ingress and egress filters are both empty; foreign filters
preserve the qdisc and relinquish bpfman's ownership. Classic ingress qdiscs,
malformed ownership evidence, and foreign programs replacing the exact stored
filter are refused. The existing Go SQLite schema 2 remains unchanged. TC teardown
across implementations is not supported yet: Go attachments lack Rust's ownership
receipt.

`direnv exec . make rust-test-tc` selects store and real veth failure-recovery
contracts for both hooks with SQLite. The earlier `rust-test-tc-ingress` target
and `rust-test-tc-egress` alias run the same complete TC subset.
`make rust-test-scripts` separately runs all admitted unchanged Go TC load/link
lifecycle, chain, ordering, signed-encoding, namespace,
survivor-unload, egress traffic and ingress/egress independence scripts with SQLite.
The Rust-only bidirectional clsact script adds twelve cases for both attach/drain
orders and owned/borrowed/foreign-filter qdiscs. The Rust runner advertises
`BPFMAN_E2E_CLSACT_RECLAIM=true` so the reclaim
script executes independently of Go's disabled production policy.
The lifecycle tests require exact marked-packet counts, preserve foreign filters
at the same priority on both hooks, reopen before detach, cross publication and
cleanup faults, retain retry/cancellation history, and verify empty inventories and
runtime artifacts after detached and attached-program unload.

This ingress replacement checkpoint passed `direnv exec . make rust-check`:
formatting, Clippy, all userspace and compile-fail contracts, 60 shared fake-kernel
lifecycle tests, all 168 real-kernel tests (none ignored), and documentation. The
serial kernel suite completed in 1834.78 seconds. The focused TC target passed two
store contracts and 36 kernel/CLI/script checks. Makefile lint and the Go runner's
clsact capability unit check also passed. Userspace contracts run
before the complete kernel suite in the normal gate.

The 14 October egress checkpoint passed all twelve TC kernel contracts and the
full Rust gate: 106 real-kernel tests, all userspace/compile-fail contracts and
warnings-denied Clippy/Rustdoc, using owned tmpfs scopes with successful cleanup.
Both stores passed all 90 admitted script files, including the two unchanged TC
egress acceptance gates and twelve Rust-only shared-clsact cases. Make lint passed
after splitting its target alias declarations. See the
[implementation record](../docs/design/rust-reimplementation.md#tc-ingress-parity-audit-and-independent-egress-14-october-2026).

TC detach and attached-program unload now remove a complete dispatcher whose
explicit namespace selector is absent. Only ENOENT while opening that selector
admits this path; interface absence, permission errors and changed identities
remain failures. The adapter validates the confined revision and durable clsact
evidence, rechecks selector absence, removes owned pins, then deletes the unchanged
complete store snapshot. Failures retain partial progress for explicit retry.
Repeated members are consumed once per removed scope while independent hooks and
namespaces still get a pass. Both stores retain their existing formats/schema.

An absent name does not prove the kernel namespace was destroyed. Other holders
may keep its native filter/qdisc alive. Orphan cleanup releases the validated
managed freplace pins; it performs no filter or qdisc operation in an unverified
namespace. The kernel contracts keep the original namespace alive under another
name and require managed program-ID absence immediately after unload.

The unchanged TC acceptance scripts also cover capacity rejection, last detach,
multiple-interface independence, namespace round trips/rebuilds, signed UNSPEC,
multi-priority continuation and foreign ingress-qdisc refusal. Their results are
recorded separately from the parity manifest: script contents and labels remain
unchanged. New shared scripts require Go and Rust acceptance with SQLite;
implementation-specific orphan lifetime and recovery coverage stays in Rust tests.
The new shared `TestTC_DeletedNetnsLifecycle` exercises both hooks, repeated
members, another program's orphan membership and an independent live namespace.
It exposed a Go unload bookkeeping failure: complete dispatcher deletion consumed
later members of the original unload list. Go now reports complete removal to
that unload operation, which consumes those later members once. Unexpected missing
snapshots and failed removals still fail. Go's namespace checks, kernel adapter,
cleanup ordering and warning/GC policy remain unchanged. All four cases pass the
reduced Go change and Rust with SQLite. Rust retains its stricter orphan
validation and consuming recovery; the shared script adds no post-unload sleep.

Legacy TC outer-filter status and general orphan repair remain unsupported. Qdisc inspection/deletion
uses separate netlink requests and is not atomic against privileged tools changing
the same objects outside bpfman's writer lock.

## Native TCX links

TCX selections load as native SCHED_CLS programs rather than freplace extensions.
`link attach tcx PROGRAM INTERFACE ingress|egress --priority PRIORITY` supports
metadata and an explicit `--netns` selector. Both hooks and separate namespaces
are independent. TCX creates no legacy dispatcher or clsact qdisc.

Lower priority values execute first; zero is preserved and equal priorities retain
attachment order. Insertion uses a live native program anchor, with its descriptor
retained through the attach syscall. Missing or detached recorded links do not
become anchors; failed inspection remains an error. A pending link requires explicit
cleanup before further insertion on its hook. Duplicate program/hook attachment
is refused before touching the original pin, and the diagnostic names that link.
Stored `position` is the rank of managed intent in one snapshot, not a kernel dump
or a claim about foreign programs.

The same pending-intent, finalisation and consuming compensation interpreter serves
tracepoint and TCX attachment. Canonical TCX pins use
`fs/tcx-{direction}/link_{nsid}_{ifindex}_{program}`. Adoption validates the kernel
link ID, program, type, direction and interface. Teardown explicitly detaches the
validated native link before unpinning, retaining its FD until all owned pin
references close. A retained external link FD can keep the object alive but cannot
keep it executing after successful detach. Namespace-selector access is unnecessary
for deleting an adopted native link; its own descriptor identifies the attachment.

Standalone attached unload now attempts every independent link once per pass.
A failed unpin blocks its own record deletion; any unresolved link blocks program
teardown. Successful unpins do not repeat during record-only retries, including
through a reopened application. This also strengthens tracepoint unload, with
its fault and retry contracts updated to require independent progress.

SQLite keeps Go schema 2 and its existing native TCX detail table. Tracepoint
target strings keep their existing JSON encoding; native targets have a typed object representation.

`direnv exec . make rust-test-tcx` runs real kernel recovery and eight existing,
unchanged TCX acceptance files with SQLite. It is also part of `rust-check`.
The files cover load/get, ordinary and namespace link round trips, duplicate
rejection, priority ordering, zero/tie ordering, chain continuation and stopping.
Their labels remain unchanged and their passes are recorded separately from the
91-file manifest. Rust-specific contracts cover real store receipts, pin failures,
foreign runtime rejection, cancellation, reopened retries, and immediate owned
kernel release without post-unload sleeps or polling. Genuine external program
references remain observable until their holders close.

The parallel regression also removed a pre-existing tracepoint unload polling
loop that enumerated all kernel programs. That enumeration could itself hold
another fixture's program FD. The contract now requires immediate absence of its
own ID, preserving independent fixture lifetimes. Go passes the same eight TCX
scripts with SQLite; Go sources remain unchanged.

Immediate native lifetime controls run in the existing serial stage, separately
from concurrently spawning contracts. A pipe-synchronized fork/exec experiment
confirmed that CLOEXEC descriptors still keep programs alive in a child before
exec; the Rust trace shows subprocesses copying the descriptor table. Isolation
establishes the absence of unrelated inherited holders while retaining every
immediate assertion and parallelizing ordinary contracts. Production teardown
releases owned references; it cannot revoke an external child's descriptor.

The TCX checkpoint passes `direnv exec . make rust-check lint-make`: all 112
kernel cases and 68 stateful-fake cases, userspace/compile-fail checks,
warnings-denied Clippy/Rustdoc, formatting and Makefile lint, with successful owned
tmpfs cleanup. All 173 existing scripts remain byte-for-byte unchanged. Evidence
and the lifetime investigation are in the
[implementation record](../docs/design/rust-reimplementation.md#native-tcx-lifecycle-and-ordering-14-october-2026).

## Kernel entry and return probes

Local `kprobe:ENTRY` and `kretprobe:ENTRY` selections load native KPROBE programs.
Either kernel-probe ELF section is accepted; entry versus return attachment comes
from the stored selection, so one ELF entry can be loaded in either role. Attach
with `link attach kprobe PROGRAM_ID FUNCTION [--offset BYTES] [-m KEY=VALUE]`.
The function is syntax-validated and offsets must fit the shared persistence
range. The kernel validates symbol existence and whether an offset lies inside it.
Get/list expose Go-compatible `fn_name`, `offset` and `retprobe` details.

`Bpfman::attach_kprobe` and its cancellation variant use the same standalone
pending-intent and consuming compensation interpreter as tracepoints and TCX.
Detach keeps the program loaded; attached unload admits all pins before effects,
attempts independent links once, and retains unresolved records for explicit retry.
Reopening the application and retrying a record-only failure never removes a
completed pin again. Adopted program and link pins require confined traversal and
exact kernel identity. Perf-event teardown holds a live descriptor across unpinning
and closes it after pin descriptors. External holders retain their ordinary kernel
lifetime semantics.

SQLite keeps Go schema 2 and `link_kprobe_details`. `make rust-test-kprobe` runs
native recovery contracts and seven unchanged acceptance files with SQLite: kprobe/kretprobe load/get and link
round trips, multi-program weighted counters and invalid-offset cleanup. The full
gate includes them without changing existing labels. Stateful-fake contracts cover
acquisition/publication failures, cancellation and reopened retries on real
SQLite. Native lifetime controls run serially to exclude descriptors temporarily
inherited by concurrently forked test children.


## Map sharing and PinByName

Local loads support `--map-owner-id ID` with SQLite. `ID` identifies a
first-class map set originally created by that program, rather than any live
program that happens to use those maps. A dependent's ID is not a map owner.
The original program may unload first; later loads can still join its map set
while another member remains. The final member removes the map pins, container
and map-set record. Ordinary loads of PIN_NONE maps remain independent.

PinByName maps share a compatible kernel map through `fs/shared/{name}` and keep
per-map-set aliases for observations and explicit owner joins. Before reuse,
the adapter checks map type, key/value sizes, capacity, flags and map-extra.
Creation, reuse, reference publication and teardown all hold the runtime writer
lock. Shared maps retain BTF for tools such as bpftool; the existing private BPF
syscall boundary supplies the BTF descriptor required by Aya's public map-create
API. Aya and dependency versions are unchanged. Sharing map-of-maps definitions
is outside this slice; PinByName declarations of those maps fail before runtime
setup, and explicit reuse also refuses them. Internal-looking map names cannot
declare PinByName; admission rejects them before Aya can choose a default pin path.

Newly created name pins have consuming cleanup receipts. Existing pins never
become load-compensation targets. Failed acquisition, verifier rejection,
publication and cancellation compensate every newly owned pin, with cancellation
checked between name-pin acquisitions. Cleanup retains failures for explicit
retry. If a committed user adopts an unchanged pin before retry, cleanup validates
the receipt and relinquishes its authority while preserving that user's map.
Unload follows the same rule for map-set aliases, containers and shared names.
Changed inode or runtime evidence still refuses cleanup.

SQLite keeps Go schema 2 and uses `map_sets`, `managed_programs.map_set_id` and
`shared_map_pins`. Older artifacts without a map inventory retain
conservative program-reference validation; broader orphan adoption is separate.

`direnv exec . make rust-test-map-sharing` runs native identity/recovery contracts
and these unchanged scripts once with SQLite:

- `TestKprobe_MapOwnerIDSharedCounter`
- `TestProgramLoad_MapOwnerIDMustNameMapSet`
- `TestTC_PinByNameMapSharing`

The complete gate also runs shared store contracts and stateful-fake coverage for
faults, cancellation, owner-first unload, reopened retries and authority transfer.
Native recovery requires verifier-failure cleanup, compatible reuse with retained
BTF, incompatible-map refusal, swapped-pin preflight refusal, attached owner-first
unload and successful cleanup after a new member joins during recovery. A retained
map descriptor remains usable after managed pins disappear; physical kernel map
reclamation remains outside the synchronous teardown promise.

## Host uprobes and uretprobes

Local `uprobe:ENTRY` and `uretprobe:ENTRY` selections load native KPROBE programs.
Either userspace-probe ELF section can supply the program; its stored selection
chooses entry or return attachment. Attach a host file by symbol or absolute file
offset, optionally filtering one positive host PID:

```sh
bpfman program load file probe.o --programs uprobe:uprobe_counter
bpfman link attach uprobe PROGRAM_ID /path/to/binary --fn-name entry --pid PID
bpfman link attach uprobe PROGRAM_ID /path/to/binary --offset FILE_OFFSET
```

`-f` aliases `--fn-name`. With a symbol, offset is relative to its entry; without
one, a nonzero offset is required. PID zero or omission selects all processes.
File targets contain a directory separator; bare library names resolve through
process maps or the dynamic linker cache. `--container-pid HOST_PID` selects a
container mount namespace for an absolute file target or library name.
The target is opened nonblocking and must be a regular file. Symbol resolution
and attachment use the same retained file through `/proc/self/fd`.

Link get/list, detach and attached-program unload use the standalone pending-intent
protocol, confined pins, cancellation and consuming compensation. Independent
links make progress after another record deletion fails; record-only retries
retain their exact evidence and never remove completed pins again. SQLite keeps
Go schema 2 and `link_uprobe_details`.

`make rust-test-uprobe` runs native identity/recovery/lifetime contracts and nine
unchanged scripts per store, covering entry/return load/get, link round trips,
multi-program lifecycle, absolute offsets, positive PID filtering and dotted Go
symbols. The full gate also selects them. Native lifetime controls run in serial
global stages and require immediate owned-program absence, with an explicit
external descriptor control.


## Container uprobes and mount-namespace target lookup

```sh
bpfman link attach uprobe PROGRAM_ID /path/in/container --fn-name entry --container-pid HOST_PID
```

The hidden `__ns-helper open-uprobe-target` command re-executes the same binary
before signal handling or thread creation. The parent opens the target mount
namespace (using `/host/proc` if the host PID is absent from `/proc`) and duplicates
its held writer lock. Explicit inherited descriptor slots carry the namespace,
lock and Unix socket. The single-threaded child validates them, enters the mount
namespace, resolves any remaining library name against its cache, opens the
regular-file target nonblocking, and returns its
FD through `SCM_RIGHTS`. It performs no BPF operations.

The parent reaps the child and attaches through its own `/proc/self/fd`, retaining
the target FD through symbol resolution and attachment. Pinning, pending intent,
compensation and writer authority stay in the parent. Cancellation or the bounded
30-second helper deadline kills and reaps the child; every failure closes local
and transferred descriptors. Detach and unload work after the target namespace
has disappeared. Reusable callers inject target opening through
`Kernel::with_uprobe_target_opener`; `Kernel::default()` supports host targets.

SQLite keeps schema 2 and `link_uprobe_details.container_pid`. Container PID zero
retains host lookup and remains separate from the optional `--pid` process filter.

The focused `rust-test-uprobe` target and full gate include three unchanged
container scripts with SQLite: `TestContainerUprobe_LinkIDAndTraffic`,
`TestContainerUprobe_ExternalCLIPersistence` and
`TestContainerUprobe_PidNamespaceTraffic`. Native contracts distinguish a
namespace-only target from its host spelling for both entry and return probes,
check failed symbol lookup and missing namespaces, and reopen/tear down persisted
links after the namespace exits. Process contracts exercise malformed messages,
failed child exits, descriptor closure, cancellation and reaping. Library-name
lookup and its cache/pod/host-proc acceptance are described below.


## Bare library-name uprobe targets

```sh
bpfman link attach uprobe PROGRAM_ID libc --fn-name malloc --pid HOST_PID
bpfman link attach uprobe PROGRAM_ID libc.so.6 --fn-name malloc --container-pid HOST_PID
```

Resolution follows the Go reference: positive-PID maps first, then the dynamic linker's
`/etc/ld.so.cache` for an unresolved library name. A maps read failure is terminal.
Matching supports `libc`, `libc.so`, exact sonames such as `libc.so.6`, and the
older `libold-VERSION.so` layout. The first matching mapping/cache entry wins.
Explicit host file paths retain their existing direct lookup.

For containers, the parent reads host PID maps through the proc root that opened
the namespace (`/proc` or `/host/proc`). The helper opens that resolved path after
entering the mount namespace; unresolved bare names use the container's cache.
The helper never tries to reinterpret a host PID through container procfs. Native
path bytes survive resolution and the helper's typed pathname argument. The
stored target remains the requested name; detach/unload need no repeated lookup.

Maps/cache parsers are pure functions in `bpfman-core`. The cache parser supports
new-format glibc caches with native-endian offsets, validates the full entry table
and all NUL-terminated strings, rejects overlapping/out-of-range offsets and
non-absolute cache paths, and explicitly rejects standalone old-format caches.
No `dlopen` or library constructors run. Attachment still holds one opened regular
file through symbol resolution and BPF attachment.

SQLite keeps schema 2. Pure tests cover malformed/truncated caches, matching and
ordering; real-store/fake-kernel matrices cover raw-name persistence and recovery.
Native tests actively install synthetic host caches, test maps precedence and
fallback, refuse missing maps, and resolve host PID mappings into a separate PID
namespace with no cache. Helper lookup failures leave no pending links.

Five unchanged files join `rust-test-uprobe` and the full gate with SQLite:
`TestUprobe_LibraryNameCacheTraffic`, `TestContainerUprobe_LibraryNameTarget`,
`TestContainerUprobe_SyntheticLdSoCache`, `TestContainerUprobe_PodStyleNamespaces`,
and `TestContainerUprobe_HostProcDaemonView`. All five also pass against Go on
both stores. The host-cache file takes its existing no-cache branch on this
NixOS host; active synthetic host/container cache fixtures prove that tier.
The external `TestContainerUprobe_AppTestImage` still depends on OCI loading.


## Native fentry and fexit

Local `fentry:ENTRY:FUNCTION` and `fexit:ENTRY:FUNCTION` selections load native
TRACING programs. Each selection requires its corresponding ELF section; an
fentry selection cannot load an fexit section. The requested function overrides
the section's target and is bound at load time. Attach takes the loaded program
ID, with no replacement function argument:

```sh
bpfman program load file object.bpf.o --programs fentry:entry:do_unlinkat
bpfman link attach fentry PROGRAM_ID -m owner=example
bpfman link get LINK_ID -o json
bpfman link detach LINK_ID
bpfman program unload PROGRAM_ID
```

Use `fexit` for both load and attach when selecting function exits. Get/list retain
that family and the requested function. `Bpfman::attach_tracing` and its
cancellation variant take a typed `TracingKind`; loading and attachment preserve
the same function through SQLite and CLI process boundaries.

Resolution searches vmlinux BTF first, then kernel module BTF in kernel BTF-ID
order. Missing functions and ambiguous matches within a BTF object are errors.
The checked record scanner handles split module type IDs and string offsets
against the vmlinux base. Userspace BTF objects are excluded. Module descriptors
stay owned through loading. Pinned-program adoption verifies the actual program
ID, native type and load-bound BTF object/function before pending publication;
the kernel checks the entry/exit attach type at link creation.

Aya's public loader handles vmlinux targets. Aya 0.14 does not expose the module
BTF descriptor for FEntry/FExit loading, so module loads reuse Aya's maps and
aya-obj's public BTF/map/call relocation, with the final load in the existing
private syscall boundary. Optional newer object-BTF kinds use aya-obj's portable
sanitization. No Aya fork, private-layout access or dependency changes are needed.

Attachment, pinning, compensation, cancellation, detach and attached-program
unload use the standalone consuming interpreter. Tracing receipts retain their
entry/exit kind, validate canonical confined pins and keep a live link descriptor
across pin removal. A second attachment of one loaded tracing program receives
the kernel's refusal and compensates its pending intent, preserving the original
link. Separate loaded programs can trace the same function. Failed record cleanup
retains program ownership for an explicit reopened retry. Trampoline execution
quiescence follows kernel RCU behavior, as in Go.

SQLite retains Go schema 2, `managed_programs.attach_func` and the existing
`link_fentry_details`/`link_fexit_details` tables. Link function names must match
the corresponding program's stored load target.

`make rust-test-function-tracing` selects six unchanged fentry/fexit acceptance
files with SQLite, plus native vmlinux/module traffic and recovery contracts.
The native tests verify requested-target override, missing-target and publication
failure cleanup, cancellation, pin identity refusal, duplicate compensation,
reopened record-only retries and external descriptor lifetime controls. Immediate
lifetime contracts run in the serial global stages of the full gate.


## Native LSM hooks

Local LSM loads bind the requested hook at load time, overriding the ELF target:

```sh
bpfman program load file e2e/testdata/bpf/lsm_exact.bpf.o \
  --programs lsm:test_lsm:file_open
bpfman link attach lsm PROGRAM_ID -m owner=lsm
bpfman link get LINK_ID -o json
bpfman link detach LINK_ID
bpfman program unload PROGRAM_ID
```

`lsm:ENTRY:HOOK` requires an LSM ELF section and a hook found as
`bpf_lsm_HOOK` in vmlinux BTF. Aya's public LSM loader preserves the section's
sleepable flag and loads native `BPF_PROG_TYPE_LSM`; it is distinct from
fentry/fexit's TRACING program type. Attach takes no replacement hook. The shared
`TracingKind::Lsm` selection and `Bpfman::attach_tracing` operations drive the
existing standalone interpreter, writer authority and consuming receipts.

Pin adoption revalidates the native program type, ID and BTF target against the
stored hook. Link observation and cleanup require the LSM attach type. The
pending-intent, cancellation, compensation, detach and attached-unload paths
reuse the existing confined filesystem operations. Record-only failures retain
program ownership for an explicit reopened retry; completed pin removal never
repeats. A live link descriptor remains owned across unpinning and closes after
all pin descriptors. Separate program loads can attach to the same hook; a second
link for one loaded program is refused and compensates its pending intent.

SQLite retains Go schema 2 and its existing `managed_programs.attach_func` and
`link_lsm_details.hook_name` fields. Link JSON uses `kind: lsm` and
`details: {hook_name: HOOK}`. Hook and family must match the stored load target.
Store contracts and generic runtime consumers remain independent of SQL.

`direnv exec . make rust-test-lsm` runs the two unchanged acceptance files,
`TestLsm_LoadAndGet` and `TestLsm_LinkRoundTrip`, plus native execution and
recovery contracts. The same files pass against Go with SQLite. Native contracts
prove the requested hook overrides the ELF hook, precise execution and
post-detach quiescence, missing-hook cleanup, publication failures, cancellation,
foreign pins, reopened cleanup and external descriptor lifetime. Shared store
and stateful-fake matrices include LSM through real SQLite. SQLite adapter tests
cover atomic detail publication and malformed/mismatched hook records.

BPF LSM must be active in `/sys/kernel/security/lsm`; merely compiling the kernel
with BPF LSM support is insufficient. The native test requires it and joins the
serial global stage of `rust-check`, keeping immediate lifetime controls separate
from concurrently spawning fixtures. Existing script assertions and labels remain
unchanged. OCI loading and other attachment families remain separate work.
