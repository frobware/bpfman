# Rust bpfman-shell reimplementation

## Purpose and authority

Reimplement `cmd/bpfman-shell` in the independent `rust/` workspace. The Go
implementation remains the behavioural reference until the Rust shell passes
its language, fixture, and integration contracts. This work is separate from
the Rust `bpfman` daemon and CLI cutover. The Go shell's status as a parity
harness in [rust-reimplementation.md](rust-reimplementation.md) describes the
daemon migration; it is not the final language implementation.

The accepted language is defined by `cmd/bpfman-shell/LANGUAGE-SPEC.md`,
`GRAMMAR-REFERENCE.md`, the parser, and their tests. The grammar rationale
identifies parser accidents that are not language rules. Preserve the
argv-first lexer and expression islands. Preserve observable output, source
locations, command selection, cleanup, cancellation, and exit status.

## Phase architecture

Keep the Go phase order and make each boundary explicit in Rust:

```text
source coordinates
       ↓
syntax: tokenise → parse → AST
       ↓
driver: resolve and expand imports
       ↓
check: whole-program preflight
       ↓
lower: AST → canonical block-structured IR
       ↓
runtime: execute IR through driver-supplied effects
       ↓
driver and builtins: process, filesystem, network, bpfman CLI, output
```

`semantics` owns the closed-world origin, shape, and pure-builtin vocabulary
used by the checker and runtime. `ir` owns instructions, blocks, source spans,
and stable dumping. No phase reaches into a later phase to perform its work.
The driver orchestrates the phases and owns import I/O; parsing and checking
remain free of command side effects. Source spans are byte-based and half-open,
including in imported files and interpolations.

## Gather, plan, execute

The whole-program boundary follows gather, plan, execute:

1. Gather source bytes, imported source units, invocation options, and the
   selected `bpfman` executable identity through the driver. Import expansion
   completes before script command effects begin.
2. Parse, expand, check, lower, and validate the complete program without
   executing script commands. The checked executable IR is the static plan.
   A parse/check/lower error leaves no prior script command side effects.
3. Execute that plan through a driver-owned effect adapter. The language
   runtime never opens files, spawns processes, sleeps, or prints directly.

The IR cannot be a fixed list of all future effects: `if`, `poll`, `retry`,
`foreach`, defs, and command results select later work dynamically. Within the
execution phase, use an incremental SANS-I/O interpreter. Its state transition
yields either the next typed effect request, a finished value, or a failure.
Each request carries a private continuation whose `resume` method accepts only
that effect's response type. The driver cannot pair a command result with a
sleep request or forge a completed interpreter state. The outer driver performs
each request and returns its observation. Command execution, time/sleep,
output, cancellation and job control cross this boundary explicitly. A run
may therefore plan the next effect only after observing the previous one while
still keeping all language decisions inside the pure interpreter.

Keep effect requests and continuations private to the runtime where possible;
expose only the typed step surface the driver needs. Do not build a generic
workflow framework around the language. Tests can drive the interpreter with
scripted responses and a fake clock, asserting request order, cleanup, and
final state without real processes. Separate driver tests cover process
groups, signals, filesystem and
network effects. The Go runtime's callback-based SANS-I/O tests are the
behavioural reference for these pure transition tests.

Use separate workspace crates for source, syntax, semantics, check, IR, lower,
and runtime so dependencies enforce the direction above. A shell application
crate owns the driver, builtins, and CLI. These are architectural boundaries,
not a request to expose a large public API: each crate should export the
smallest operation and result needed by its consumer.

The syntax crate presents one ordinary entry point to the driver:
`syntax::parse(source_name, source_text) -> Result<ParsedProgram, SyntaxError>`.
The caller supplies UTF-8 source text and an identity for diagnostics; the crate creates
its lexer/parser internally. `ParsedProgram` owns an immutable AST and its
source identity. Keep `token`, `parse`, `ast`, and source formatting/inspection
as private modules, re-exporting only the AST queries needed by the checker,
the parse result, and diagnostic types. Expose `tokenise_at` as a lower-level
API for focused syntax tests and parsing interpolation fragments at an
absolute position. No driver should need to assemble a parser object or
manage mutable token cursors.

The parser now accepts simple commands, `let`/`guard` command binds,
expression-valued `let` assignments, `defer`, `return`, `break`, and `continue`.
Expression lowering covers precedence, grouping, pure builtin calls, lists,
interpolation, and threaded calls. Commands and threaded calls have nonempty
typed heads that may be computed, matching Go's dynamic head resolution. Expression
statements are parsed through the same grammar. Recursive blocks now cover
`if`/`elif`/`else`, `foreach`, definitions, and poll/retry, including nested bodies,
destructured loop bindings, typed definition parameters, and positive Go-range
poll durations. Assertions now retain expression versus command-status clauses
as distinct AST variants. Record fields, matches rows (value, predicate, or
nested block), and bind-collect loops now have typed AST forms. Bind-collect
requires a final producer command in its foreach body at parse time, matching
the Go syntax phase. `let` destructuring now validates positional name lists,
including discard slots. The accepted Go statement forms have Rust AST variants;
parser error and formatting parity remain test work. The separate checker
combines its pure analyses at the preflight boundary.

The import boundary now has a pure two-step API in the checker crate:
`prepare_imports(parsed, base_dir)` validates all import sites and returns the
resolved direct paths to gather; the driver supplies their source text to
`expand_imports(plan, gathered)`. Expansion parses each library, requires
top-level definitions, rejects duplicate visible definitions, and splices
those definitions in source order while retaining library source spans.
`ExpandedProgram` is a distinct unchecked type. The scope pass can inspect
either parsed or expanded statements. The call pass uses hoisted definitions to check command and
thread argument counts, while keeping dynamic heads open. It rejects pure
builtin names on `<-` binds unless a definition shadows that name. The syntax
crate exposes one builtin-name and arity lookup so parsing and this pass share
the same accepted vocabulary. The call pass also applies the semantics crate's
narrow literal contracts for `u32le`, `u64le`, and `range`; nonliteral arguments
remain runtime checked. It preserves Go's base-prefix and leading-zero integer
forms for quoted literal arguments and reports the argument span.
The call pass checks the native job commands' positional arity after resolving
user definitions. For `kill`, it excludes only literal `--signal=` and
`--grace=` arguments, including those with a fixed flag prefix and an
interpolated value. Go's checker excludes every `--` word, which can miss a
flag typo that the Go runtime then treats as an extra positional argument.
Literal `kill --signal=` values use the Go job-signal set and normalization.
Literal `kill --grace=` values use the shared Go-style duration parser, including
zero and signed values; negative durations are reported before execution because
the Go runtime rejects them even though its preflight checker does not.
Annotated definition parameters now check literal call arguments in command
and threaded positions after arity succeeds. The syntax crate's JSON number
validator is shared with this check; quoted values assert string, and variable
or computed arguments remain runtime checked. Imported definition annotations
are checked at their call sites after import expansion.
The semantics crate now holds an origin and structural-shape vocabulary. A
separate pure shape pass in the checker propagates proved record, list, native
bind, and bind-collect shapes through lexical bindings and reports invalid
fields only on sealed parents. Unknown and bpfman program/link payloads stay
open pending verification against Rust's JSON projection. The same pass checks
comparison operands using their lexical shapes. Known records can be compared
to null for equality and inequality. Rust also diagnoses record-to-record
comparisons during preflight when it has a sealed record shape. Go leaves
literal records' origin unknown and diagnoses this at runtime. Unknown operands
defer to runtime. A bare definition name in a comparison gets a call-site hint
at the operand span. Binary arithmetic checks each literal operand against
the JSON-number grammar, including operands inside interpolation. Variable
operands use lexical binding evidence, including known nonnumeric literal
assignments and proved nonnumeric kinds. Dynamic operands remain runtime
checked. Definition bind shapes are inferred on demand from hoisted bodies in
isolated lexical frames. Matching return shapes propagate when every path
returns, including through nested binds and collect producers. Disagreeing
returns, possible fallthrough, and recursive inference without a proved shape
stay open. Definitions without a return produce the ordinary command result
shape. The job pass follows Go's name-based check for guarded native starts.
It recognizes waits, kills, and deferred cleanup, and treats direct returns
from a definition as ownership transfers. Hoisted definitions shadow the
native commands. The runtime still owns path-sensitive job cleanup. Lexical
shape checking validates job operands and distinguishes a namespace pair from
its endpoints for network commands. Job checks use the selected field's shape,
so a job stored in an outcome's `value` field is accepted. The call pass also
suggests a definition call when an unquoted definition name is assigned with
`=`, or when an unquoted, unregistered bind head is close to a known definition.
The pure `check(ExpandedProgram)` entry point returns diagnostics from the
scope, call, shape, and job passes or constructs a `CheckedProgram` with the
original source spans. Driver-level side-effect and `--no-check` contracts
remain part of the lowerer and driver work.

The first IR and lowering slice supports straight-line commands, value
assignments, expression statements, and defers with words, quoted strings,
variable paths, and lists. The IR owns these operands independently of syntax.
Blocks carry their terminator separately, and program construction validates
successor indices and block-local temporary definitions and uses. The initial
dump matches Go's straight-line output. `lower(&CheckedProgram)` is the normal
entry point, and `lower_unchecked(&ExpandedProgram)` is the explicit bypass.
Both verify their output and report unsupported syntax at its source span.
Conditional lowering now emits a typed branch, a lexical frame for each
selected body, trace events, and a shared continuation. Blocks keep Go's
publication order even when nested bodies allocate temporaries before a
previously published block is filled. A nested conditional fixture compares
the dump with Go. Ordinary foreach lowering identifies each iterator by its
header block. Break and continue target the innermost iterator and carry the
contract to unwind its iteration frame. Verification checks reachable loop
contexts, and a nested-loop fixture matches Go's dump, including the
unreachable blocks emitted after explicit loop control. Command-form binds
now distinguish capturing an outcome from guarding its primary value. Each
program-scope guard gets a defer-cleanup block, including guards inside
conditionals and loops. Verification checks that the applied result came
from the same argv and that the failure target drains defers before propagating
the error. Every current syntax expression now lowers to IR-owned operands,
including interpolation, adapters, records, nested matches, calls, and logical
operators. The expression fixture checks Go's dump and preserves source spans
in the IR. Hoisted definitions now have independent block and temporary
tables, typed parameters, and a shared return epilogue. Definition guards
drain their local deferred commands before propagating failure. A Go fixture
covers forward dispatch, returns from loops, and definition-local cleanup.
The IR also distinguishes expression assertions from command-status clauses
and retains positional destructuring names and discard slots. Bind-collect
shares the iterator model and tracks guarded versus capturing aggregation.
The producer retains both the trailing command span and the enclosing bind
span. A Go fixture covers both policies and explicit loop control. Poll
lowering carries a positive schedule, attempt-local cleanup, and distinct
success and timeout continuations. Retry identifies either the lexical poll
or the dynamically enclosing poll of a caller. The complete Go
`language.lowered` golden now matches after import expansion and preflight.
The runtime now has a first SANS-I/O execution slice for straight-line
commands, expression output, basic values, conditional branches, command
binds, and program-scope defers. The driver supplies command and output
observations through consuming, type-specific continuations. Numbers retain
their exact text and the interpreter shares syntax's pure JSON-number
classifier. Unit tests drive the machine without processes or a clock and
check LIFO capture and guard-failure cleanup. Ordinary and collecting loops
now execute through the same iterator state, with lexical frame unwinding,
destructuring, and partial collection publication on break. Definition calls
resolve before driver dispatch, bind typed parameters, and run with their
own temporary slots and defer boundary. Tests exercise nested returns,
both cleanup scopes, and an error in a deferred definition while another
error is already active. Local polls request monotonic time and delay
through separate continuations. Fake-clock tests cover cleanup before retry
and deadline handling. The Rust CLI gathers stdin, script files, and direct
imports, supports checked and explicitly unchecked lowering, and drives
the initial print, subprocess, output, and clock effects. Effect coverage is
partial. The interpreter now evaluates nested paths, interpolation, unary
predicates, short-circuit operations, and compatible scalar comparisons.
Integer comparisons and integral addition, subtraction, multiplication, and
remainder are exact. Division and mixed arithmetic use finite floating point,
as in Go. The interpreter alone admits the pure arithmetic closure
`num-bigint` 0.4.6, `num-integer` 0.1.47, and `num-traits` 0.2.19, all
MIT or Apache-2.0. Deterministic expression builtins now cover range, zip,
little-endian conversion, containment, and presence predicates. Structural
matches evaluate nested and exhaustive rows. Expression and command-status
assertions distinguish nonfatal diagnostics from requirements that unwind
the program and report located details.
The shell can invoke the CLI selected by `BPFMAN_BIN`, preserve exact JSON
integers, and project program and link records to IDs for later command
arguments. A retry from a helper now unwinds each callable's defers and the
enclosing attempt before it asks the driver for another clock reading.
The driver also supports caller-owned temporary files and directories,
structured rendering, and subprocess-scoped `file:$value` adapters.
The `path-exists` expression requests a typed filesystem observation. Multiple
queries in one expression resume in order, with observations reused only
until that instruction completes. The jq effect distinguishes literal JSON
text from already typed values and preserves zero, one, or multiple results.
An in-process jq adapter is available for expression, thread and standalone
calls. Thread expressions can also call local definitions or dispatch an
invocation to the driver. They preserve the caller's observations across
definition frames and use the command envelope when no primary value is
returned. Foreground commands run in owned process groups. SIGINT and
SIGTERM interrupt the current wait or poll and let the evaluator drain
defers. `start` issues an opaque job handle and a driver reaper owns the
process group and adapter files. `wait` observes and caches the exit
envelope. Each script has one job registry, which reports and cleans up
unmanaged jobs after defers. `kill` resolves signals through the same typed
names used by preflight. The reaper sends signals while it still owns the
process. `jobs` reads status without marking jobs managed, and `reap` hides
completed jobs while existing handles retain their outcomes. Cancellation
starts during whole-program input. An initial signal interrupts an input
or process wait and runs registered defers. A second signal exits with
status 1 even if a defer is still running. The Go harness still defaults to
the Go shell. The Make runner now selects the shell independently
via `BPFMAN_SHELL_UNDER_TEST` because the Rust daemon's directory can also
contain the Rust shell. A temporary directory exposes just the selected daemon
first on script PATH. The selected shell's directory follows it so scripts
that look up the shell's fixture helpers find the same executable.

The Rust shell and Rust daemon passed the tracepoint load fixture and the
metadata and global-data fixture. The latter verified that elements of
`program load`'s `programs` list retain the program ID capability when passed
back to `bpfman`. The first broader isolated run exposed missing network
topology and kernel-function adapters, which account for most early failures.
The kernel-function adapter uses the same lock files, slot ordering and
provenance format as the Go shell. The driver keeps the locked slot and the
recorded lease as separate types so a slot can be released only after its
claim has been written. A real-kernel smoke test verified three
events on a leased slot. `linkinfo` reads the common kernel link identity
through the existing Aya adapter. The tracepoint round-trip and unload
fixtures passed with the Rust shell and Rust daemon, including checks that
a removed link can no longer be opened.
The isolated veth fixture uses the same TEST-NET-2 address pool as the Go
shell. A pair can release its lease, while its typed endpoints can run or
start commands in their own namespace. Setup records each created resource
for rollback and writes release provenance only after teardown. The Rust
daemon's XDP namespace round-trip and the Go daemon's TC and TCX namespace
round-trips passed. A smoke test ran a namespace job, exchanged packets, and
verified both namespaces were removed.
Host-side veth pairs accept complete explicit name and address groups or
allocate them automatically. The shared TEST-NET-2 routing rule is installed
only at an unoccupied preference and an existing rule must match the harness
shape. Setup rolls back only resources it created, which protects existing
names from being deleted on a collision. The Rust shell and Rust daemon
passed the XDP link round-trip, TC ingress/egress independence, and TC egress
traffic counting fixtures.
The whole isolated `rust=ok,!external` subset now passes with the Rust shell
against both daemons. The Rust shell also launches `fire kill` and
`fire unlinkat` workers through its own fixture entry point, with tests for
multiple numbered acknowledgements. The private LSM open-worker entry point
is available for the LSM adapter.
The Rust executable exports both fixture symbols directly. Two scoped export
attributes give the probes their fixed ELF names, and the workspace law
confines the exception to those attributes. `uprobe target` resolves the
file-backed offset through the existing ELF reader. Actual uprobe, uretprobe,
dotted-symbol and PID-filter traffic cases passed against the Go daemon.
`proginfo` reads program identity by ID or through an open bpffs pin. It
verifies that a pin refers to a program before reading its metadata. Four
program inventory fixtures passed against the Go daemon, including the
kernel-only rows created outside bpfman.
LSM probes carry a typed handle to the driver's marker and owned temporary
directory. The Rust worker opens the target under that marker. The Go
daemon's LSM round-trip observed exactly 50 opens and no additional events
after detach.
The container fixture helpers now aggregate child PIDs across every task and
produce the same native-endian dynamic-linker cache bytes as the Go shell.
All seven non-external container uprobe cases passed with the Rust shell and
Go daemon, including mount and PID namespace resolution.
The registry adapter mints reserved alias references, resolves them at the
bpfman invocation boundary, and uses either the harness override or a
process-owned loopback registry. Build, inspect, load and attach passed with
both backends against the Go daemon. A regression test checks that a digest
mismatch leaves the upload available for retry.
In image-bytecode mode, the registry adapter builds a repository-scoped
image once per script and rewrites file loads to use that image. The Rust
shell and Go daemon passed all 158 non-external image-mode fixture entries.
The Rust daemon does not yet supply the image build and load commands that
this mode requires.

The isolated file-mode matrix passed for all four shell and daemon
combinations. With the Go daemon, each shell passed 158 fixture entries
under `!external`. With the Rust daemon, each passed 170 under
`rust=ok,!external`. External-image scenarios and the CLI behavior fixtures
are still outside that result.

`--symbols` reports local bindings and direct imported definitions without
executing the program. Syntax owns the binding walk, and editor mode shares
the checker's import validation. A missing import is reported in the JSON
while valid imports and local symbols remain available. The stdin JSON was
compared byte for byte with the Go shell.
`--ast` renders the public tree from typed syntax after direct imports are
expanded. Parent expression spans retain consumed closing delimiters, while
inner nodes keep their own byte ranges. The complete Go language fixture
and all 191 checked-in end-to-end scripts matched the Go dump byte for byte.
`fmt`, `--fmt` and `fmt -w` share a pure formatter that preserves imports,
comments and command continuation groups. Before any file write, Rust
reparses the result and compares the syntax with source positions omitted.
All 191 checked-in end-to-end scripts produced byte-identical canonical
source in both shells.

`--list-scripts` walks script paths and evaluates a typed Kubernetes label
selector. Header parsing bounds input to the first twenty lines, merges
the three label forms, and validates parameterized cases before filtering
scripts. Ordinary execution accepts one source file or stdin.
The runtime lists hoisted local and imported definitions in sorted order
with `defs`. Direct calls to `range`, `zip`, `u32le` and `u64le` use
the same pure implementation as their expression counterparts.
`--trace` and `trace on/off` use typed output events that carry resolved
arguments and values. The driver renders the events before the next effect.
A defer records its argv and source location and observes the tracing
setting again when it fires. Successful deferred prints and subprocesses
publish their output.
The runtime reports assertion failures at the source statement and
structural mismatches at individual rows or nested exhaustive blocks.
It evaluates assertion operands once, retains ordinary assertion failures
through later statements, and rejects assertions in helpers called under
a poll.
Command failure reports preserve resolved argv, captured output and the
enclosing statement location. Deferred failures are reported while
draining, and job leaks cite the declaration that created the job.
Diagnostics retain the innermost definition call context or the outer
guard statement as appropriate. Poll timeout reports carry the schedule,
attempt count and last retry reason.
Command-status assertions can inspect a shell definition's guard outcome
after its local defers drain. The signed-release fixture exercises the
actual failed load through its helper, including the wrong-signer
rejections for each pull policy. The signed fixture passed on rerun with the
Rust shell and Go daemon, completing the eleven external fixture cases.

The userspace test gate runs all 136 Go workflow fixtures against the
Rust executable. It reuses the existing YAML expectations, normalizers
and output assertions, and checks exit status and the reported assertion,
defer and job-leak counts.

## Typestate and internal invariants

The normal driver path carries distinct `Parsed`, `Expanded`, `Checked`, and
`Executable` program types. The functions which produce these states are the
only public constructors. `execute` accepts only `Executable`. `--no-check`
remains a deliberate, named unchecked lowering path; it must not be an
implicit conversion from `Parsed` or `Expanded`. Parse-only inspection modes
end at the appropriate state and cannot start effects.

Keep AST and IR node variants as enums. A parsed bind has exactly one producer
(command or collect), and an interpolated segment has exactly one kind (literal
or expression). An IR block has exactly one terminator, with successors in the
same program or def unit. Block and temporary IDs are scoped to their owning
unit. Validate the complete IR once during construction, then give the runtime
an immutable executable program. Preserve the deterministic block emission and
`--lowered` dump, even if Rust uses indexed IDs rather than Go pointers.

Runtime `Value` and command `Arg` must preserve the Go distinctions that affect
behaviour: absent versus explicit JSON null; literal versus quoted versus
resolved scalar versus structured value; origin kind and source span. JSON
numbers must retain exact integer precision. The checker may infer only what
it can prove; an open shape stays open at runtime. Program and Link shape
metadata should be derived from, or checked against, Rust's actual JSON
projection rather than copied into an unrelated handwritten registry.

Typestate is for phase transitions and resource ownership. Branches, frames,
polls, jobs, and deferred commands are dynamic script state and stay private
to the interpreter. Use owned guards as a backstop for process resources, but
run DSL defers explicitly in their defined order, including their output and
failure accounting. Cancellation still gives deferred cleanup its bounded
drain and reports unmanaged jobs.

## Test migration and acceptance

Port the Go unit-test contracts by phase, retaining their cases and expected
semantics. Keep Go tests passing while the Rust port grows.

| Phase | Required existing contracts |
| --- | --- |
| Source and syntax | Byte positions, token boundaries, quotes, interpolation, AST nodes, parse errors, formatting and inspection |
| Check | Scope and shape inference, imports, command signatures, control-flow placement, and preflight rejection before any prior side effect |
| IR and lower | Instruction and block invariants, control-flow matrices, source spans, and `testdata/language.lowered` golden |
| Runtime | Exact values and number handling; dispatch, bind/guard, defs, frames, defer, poll/retry, jobs, assertions, trace, and cancellation matrices with injected effects and clock |
| Driver and builtins | Whole-program stdin, import expansion, process/signal behaviour, CLI modes, and each checked-in `testdata/<feature>/<case>/main.bpfman` plus `expect.yaml` |

Run the unchanged `e2e/scripts` corpus as a two-by-two matrix after the shell's
own contracts pass:

| Shell | bpfman binary | Purpose |
| --- | --- | --- |
| Go | Go | Existing reference |
| Go | Rust | Existing daemon compatibility check |
| Rust | Go | Independent shell fidelity check |
| Rust | Rust | Combined replacement check |

Both shells select the bpfman executable through the same explicit contract:
the typed builtin uses `BPFMAN_BIN`, and raw or nested `exec bpfman` resolves
the selected binary through `PATH`. Selection tests must prove both paths for
each binary; invoking the Rust shell must never silently select Rust bpfman.
Use isolated runtime roots and the existing suite lock so Go and Rust processes
do not mutate one store concurrently. Compare observable results and residue
per cell, retaining documented Go/Rust daemon policy differences. The Rust
shell plus Rust daemon cell alone cannot prove either implementation faithful.
Exact-output tests cover diagnostics, formatting, text, stderr and exit codes
where structured decoding of JSON would otherwise hide differences.

## Implementation order

1. Establish source coordinates and typed syntax tokens/AST. Port the lexer and
   parser unit cases before adding the runtime. Preserve the parser's argv-first
   rules; do not substitute a general shell or expression grammar.
2. Port import expansion and the static checker. Prove the preflight gate with
   the existing side-effect control, including the explicit `--no-check` arm.
3. Port the canonical IR, lowerer, verifier and golden dump. Keep execution
   absent until lowering contracts pass.
4. Port the SANS-I/O runtime with injected command, clock, output and cleanup
   effects. Move runtime unit matrices one feature at a time.
5. Port driver, builtins, CLI and behaviour fixtures. Run the complete cross
   implementation matrix on the unchanged e2e corpus, then switch the test
   runner's shell executable and retire the Go implementation only after parity
   is established.

The first implementation unit is the source-coordinate crate. It supplies
validated, half-open byte spans to every later phase without exposing mutable
position fields.

## Go library dependency replacement

The Rust shell has no Go runtime or binary dependency. Go remains an external
test oracle only. The Go shell's direct third-party production imports are
Kong, cilium/ebpf, gojq, vishvananda/netlink and netns, and x/sys/unix. Port
their *used operations*, not their entire APIs:

All new shell dependencies, including transitive and native linked libraries,
must offer a permissive license. GPLv2 and GPLv3 are excluded. Check the
selected Cargo feature closure and native link inputs at each dependency
admission; a candidate crate's own MIT/Apache declaration is insufficient if
one of its dependencies has a disallowed license. Record the exact chosen
versions and licenses before adding them to `rust/Cargo.lock`.

| Go use | Rust approach and parity risk |
| --- | --- |
| Kong CLI parsing | Existing workspace `clap`; compare help, modes, errors and exit codes |
| cilium/ebpf program and link info | Existing workspace `aya` where its public info APIs suffice; keep a private BPF syscall adapter for any missing ID/pin operation |
| netlink interface lookup | A targeted RTM_GETLINK request through `rustix` or `rtnetlink`; match Go's named-namespace lookup, ifindex and absent/error behaviour |
| netns handle and ID | Confined namespace path open/stat and a scoped namespace observation through Linux APIs; preserve named-path and namespace identity semantics |
| x/sys/unix process, lock and terminal calls | Existing `rustix`/`libc` boundaries for flock, signals, process groups, terminal handoff and unlinkat |
| gojq embedded filters | Embed `jaq-core`, `jaq-std` (with regex support), and `jaq-json` behind one shell adapter; admit the versions only after corpus parity and transitive license checks |

The shell and scripts also deliberately run external commands such as `ip`,
`ping`, and arbitrary `exec` targets. These remain process effects defined by
the language, not Go implementation dependencies. Do not replace a builtin
with a Go subprocess or hide a Go invocation in a fixture helper.

The `jq` builtin is an in-process data transformation, despite its driver
registration as an I/O category. The Go adapter accepts exactly `jq FILTER
VALUE`, parses the filter with `gojq`, converts one argument to a JSON-shaped
value, runs the filter, and collects its result stream. It accepts no jq CLI
flags. User-written scalar tokens are intended to decode as one JSON value;
resolved shell values retain their original type. Zero results become an
absent shell value in a direct expression and the successful command envelope
when threaded. One result remains a single value, and multiple results become
a list. An explicit JSON null stays present. Boolean and scalar result
origins are preserved; ordinary integer results are converted to
`json.Number`. The Rust adapter must own these boundary rules, rather than
exposing `jaq` values to the rest of the shell.

The Go semantic registry calls `jq` a *pure builtin* to give it expression
syntax and an open result shape; that name does not prove every jq filter is
deterministic. Audit ambient filters such as time, environment, input, and
diagnostic output against gojq before making evaluation an interpreter
transition. Keep any observable clock, environment, or output access at the
driver effect boundary (or pass it in as an explicit observation).

Use the library crates directly so the shell can compile and run filters
without a subprocess or the CLI's file and multi-format machinery. The
checked-in scripts require more than projections: `fromjson`, `tojson`,
`tonumber`, `map`, `select`, grouping and sorting, string interpolation, and
regex `test`, `capture`, `sub`, `scan`, and `splits`. Build the full jq standard
definitions/functions and enable `jaq-std`'s regex feature. Parse user-written
JSON strictly before converting to `jaq-json::Val`; its parser accepts a JSON
superset, whereas the Go shell's input boundary accepts JSON only. Preserve
large integers with the `serde_json` arbitrary-precision feature and
jaq's integer representation.

The shell driver pins `jaq-core` 3.1.2, `jaq-std` 3.0.4 and `jaq-json`
2.0.3. Cargo resolved the updated lockfile for the workspace's Rust 1.87
minimum. The three crates declare Rust 1.69 or 1.70 and MIT. The new
transitive crates have permissive license options including MIT, Apache-2.0,
and Zlib. The dependency law restricts the engine to the shell driver.

The Go shell accepted `1]` and `1}` as the number `1` at the jq input
boundary. `Decoder.More()` treated those delimiters as the end of a container
even at top level. The Go boundary now requires EOF after decoding one value
and has regression tests for both unmatched delimiters and a second value.
The Rust adapter will enforce the same single-value boundary.
Both query forms preserve the argument origin. A quoted token goes through
JSON decoding, while a shell variable containing the same text goes through
as a string. The runtime tests cover both paths.
Both engines computed the exact 41-digit result of squaring `10^20`.
The Go wrapper previously passed the resulting `*big.Int` through and
the shell could not print it as a scalar. It now normalizes large integers
to `json.Number`, including inside objects and arrays, with regression
tests for both output shapes.

The pinned jaq probe confirmed two remaining adapter requirements. Go
rejects `"1 2" | fromjson` while jaq emits two values. Go's `tojson`
sorts object keys while jaq preserves their insertion order. The adapter
needs compatible implementations for both filters before admission.

A temporary probe using jaq-core 3.1.2, jaq-std 3.0.4 and jaq-json 2.0.3
compiled all 197 distinct literal filters found by tokenizing the checked-in
`.bpfman` files. It used a strict JSON decoder for `fromjson` and sorted
keys for `tojson`. Against gojq, 1,176 of 1,182 comparisons on six synthetic
inputs agreed on the result stream or on whether evaluation failed. The six
differences were negative indexing on strings and objects. The two
literal filters that use negative indexes both access the array produced by
`splits` in `TestLink_Presentation.bpfman`, and array indexing agreed.
An instrumented Go shell recorded both literal and computed queries while
running the scripts against Go and Rust daemons. The shared Go run had
fixture failures. The isolated Rust run with the `rust=ok,!external`
selector passed. Across both captures, all 3,482 distinct filter/input pairs
produced the same result streams in gojq and the candidate. Exact error text
and the four-way shell/daemon acceptance matrix remain open.

The Go embed returns an empty object from `env`, allows `now`, rejects
`input`, and exposes neither `debug` nor `stderr`. The probe replaces
jaq's process environment access with an empty object and removes its
direct diagnostic output functions. Clock access stays in the driver.
