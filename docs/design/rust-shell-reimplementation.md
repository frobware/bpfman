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
parser error and formatting parity remain test work. The separate checker is
being built as incremental pure analyses. None of these analyses yet produces
the `Checked` state or authorizes execution.

The import boundary now has a pure two-step API in the checker crate:
`prepare_imports(parsed, base_dir)` validates all import sites and returns the
resolved direct paths to gather; the driver supplies their source text to
`expand_imports(plan, gathered)`. Expansion parses each library, requires
top-level definitions, rejects duplicate visible definitions, and splices
those definitions in source order while retaining library source spans.
`ExpandedProgram` is a distinct type but remains unchecked. The scope pass can
inspect either parsed or expanded statements; full semantic preflight and the
`Checked` constructor are still pending.
The next independent call pass uses hoisted definitions to check command and
thread argument counts, while keeping dynamic heads open. It rejects pure
builtin names on `<-` binds unless a definition shadows that name. The syntax
crate exposes one builtin-name and arity lookup so parsing and this pass share
the same accepted vocabulary. The call pass also applies the semantics crate's
narrow literal contracts for `u32le`, `u64le`, and `range`; nonliteral arguments
remain runtime checked. It preserves Go's base-prefix and leading-zero integer
forms for quoted literal arguments and reports the argument span.
The call pass checks the native job commands' positional arity after resolving
user definitions. For `kill`, it excludes only literal `--signal=` and
`--grace=` arguments. Go's checker excludes every `--` word, which can miss a
flag typo that the Go runtime then treats as an extra positional argument.
The semantics crate now holds an origin and structural-shape vocabulary. A
separate pure shape pass in the checker propagates proved record, list, native
bind, and bind-collect shapes through lexical bindings and reports invalid
fields only on sealed parents. Unknown and bpfman program/link payloads stay
open pending verification against Rust's JSON projection. This pass is still
partial: definition return shapes and the full preflight gate are pending.

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
absent shell value, one result remains a single value, and multiple results
become a list. An explicit JSON null stays present. Boolean and scalar result
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
large integers without a `serde_json::Number` or `f64` round trip.

`jaq` is the chosen candidate, not yet an admitted dependency. Gather every
checked-in filter from the shell unit tests, behaviour fixtures and e2e
scripts. Compare Go and Rust evaluation on the same typed inputs for
parse/runtime errors, zero/one/multiple results, explicit null, strings,
regex, object ordering, and large integer arithmetic. In particular, `jaq`
3 accepts multiple JSON values in `fromjson` where jq rejects them; check
the Go behaviour and bridge any difference in the adapter. The shell's
`tojson` comparisons also make key ordering observable. Admit exact versions
only after checking the selected Cargo feature closure for permissive licenses
and the workspace's Rust 1.87 minimum. Record each difference as a regression
test. Do not weaken scripts or call out to jq or the Go binary.

The Go scalar decoder currently checks `Decoder.More()` after its first
decode. That does not reject every trailing token at top level; another
runtime JSON decoder already uses a second `Decode` for this reason. Record
the observed Go behaviour in a differential test, then fix the Go boundary
and make the Rust boundary strict if this is confirmed as an unintended bug.
Also check whether gojq's arbitrary-size integer results are converted by
the existing wrapper before treating large-integer output as compatible.
