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
- Before handing off implementation changes, run `make rust-check` and inspect
  its exit status. Investigate failures; never skip or weaken a test to get a
  green result. Run relevant real-kernel acceptance tests when the implemented
  surface and environment support them; report untested boundaries accurately.

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

## Rust, errors, and CLI

- Edition 2024; the workspace manifest owns the MSRV, dependencies, and lints.
- Forbid unsafe by default. Any exception requires a narrow, documented boundary.
- No `unwrap`, `expect`, or `panic` in production. Test-only exceptions must be
  scoped to tests. Keep failure represented in types and source chains.
- Use `thiserror` for library errors and `anyhow` at the binary boundary. Error
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
- Keep output and logging in the front end. Libraries return values and errors.

## Tests and compatibility

- `bpfman-fs::RuntimeLayout` owns runtime path spellings and the default root.
  Construct it at the input boundary; application operations accept the validated
  layout and obtain paths through its methods. Layout construction performs no
  I/O and does not imply filesystem readiness. Add accessors as consumers need
  them, rather than scattering joins or preemptively exposing every Go path.
- Setup observations and policy execution share one writer-lock scope. The core
  decides initialise/use/reject from schema observations without knowing paths,
  SQLite, or resource handles. Runtime setup returns an opened store, not a path
  for a subsequent reopen. Observation errors are never treated as absence.
- Runtime-object creation/removal must use conceptual operations owned by
  `bpfman-fs`. Future prepared-runtime capabilities take typed object identities
  and `&WritePermit`; do not expose generic `remove(&Path)` methods. Raw removal
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
  Layout validation alone does not establish this. Implement and test that
  capability before introducing managed-object mutation/removal APIs.
- Use the Go-created SQLite schema early. Runtime setup may initialise a missing
  database under the Go-compatible writer lock; store reads remain read-only.
  Never repair or migrate existing state implicitly. Embed the actual Go
  migration SQL with `include_str!` for initialisation and fixtures.
- Mutating adapters require a borrowed, non-forgeable `WritePermit`. Acquire
  the same `<runtime>/.lock` flock as Go. Namespace helpers inherit a duplicated
  descriptor rather than acquiring by path. Close descriptors on scope exit;
  never explicitly unlock while inherited copies may still be alive.
- Preserve the Go fake kernel's valuable scenarios. A stateful effect
  interpreter should track IDs, programs, links, pins, and dispatcher changes,
  reject invalid operations, and inject failures at explicit boundaries.
  Pair pure transition tests with runtime/fake tests for ordering and residue.
- Keep real-kernel tests for guarantees the fake cannot establish: verifier,
  syscalls, namespaces, traffic, and kernel lifetime semantics.
- Reuse the unchanged `e2e/scripts/*.bpfman` corpus via the Go shell runner.
  Select a CLI using `BPFMAN_UNDER_TEST`, which sets both `BPFMAN_BIN` and `PATH`.
  Do not claim parity from decoding alone or weaken scripts for Rust.
- Make each vertical slice's supported command surface explicit. Unsupported
  commands and flags fail clearly; never invent successful observations for
  functionality not yet implemented.
