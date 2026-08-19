---
title: "Per-Language Fingerprints"
type: phase
plan: GraphPlatformExpansion
phase: 8
status: in-progress
created: 2026-08-08
updated: 2026-08-19
deliverable: "AST-backed fingerprint_symbol overrides for all six language plugins, completing LiteralInsensitive support across the supported language set."
tasks:
  - id: "8.1"
    title: "AST fingerprint override for C++"
    status: complete
    justifies: "FR-34, AC-39. C++ goes first because it is the only plugin with a preprocess pass, so it surfaces the interaction between byte-rewriting and AST fingerprinting before five other languages copy a pattern that ignores it."
    verification: "cargo test -p code-graph-lang-cpp fingerprint:: — LiteralInsensitive returns Some for C++; a symbol whose only change is a string or numeric literal yields a changed fingerprint under Normalized and an unchanged one under LiteralInsensitive (AC-39); a reformatted symbol is unchanged under both; a macro-stripped symbol fingerprints consistently across repeated calls."
  - id: "8.2"
    title: "AST fingerprint override for Rust"
    status: complete
    justifies: "FR-34, AC-39. Rust is the workspace's own language, so its override is the one exercised most often in dogfooding and the first to surface a bad shared abstraction from 8.1."
    verification: "cargo test -p code-graph-lang-rust fingerprint:: — LiteralInsensitive returns Some; literal-only change is invisible under LiteralInsensitive and visible under Normalized (AC-39); a reformatted symbol is unchanged under both; attribute and doc-comment changes behave per the mode."
    depends_on: ["8.1"]
  - id: "8.3"
    title: "AST fingerprint override for Go"
    status: complete
    justifies: "FR-34, AC-39. Completes literal-insensitive coverage for Go, without which a Go user asking the question gets an unsupported-mode response rather than an answer."
    verification: "cargo test -p code-graph-lang-go fingerprint:: — LiteralInsensitive returns Some; literal-only change invisible under LiteralInsensitive, visible under Normalized (AC-39); reformatting invisible under both."
    depends_on: ["8.1"]
  - id: "8.4"
    title: "AST fingerprint override for Python"
    status: in-progress
    justifies: "FR-34, AC-39. Python's significant indentation makes it the case where a naive whitespace-collapsing normalizer and an AST fingerprint diverge most, so it validates that the override genuinely supersedes the text default."
    verification: "cargo test -p code-graph-lang-python fingerprint:: — LiteralInsensitive returns Some; literal-only change invisible under LiteralInsensitive (AC-39); a change to indentation that alters block structure IS reported under both modes, distinguishing structural whitespace from cosmetic whitespace."
    depends_on: ["8.1"]
  - id: "8.5"
    title: "AST fingerprint override for C#"
    status: planned
    justifies: "FR-34, AC-39. Completes literal-insensitive coverage for C#."
    verification: "cargo test -p code-graph-lang-csharp fingerprint:: — LiteralInsensitive returns Some; literal-only change invisible under LiteralInsensitive, visible under Normalized (AC-39); reformatting invisible under both."
    depends_on: ["8.1"]
  - id: "8.6"
    title: "AST fingerprint override for Java, and the support matrix"
    status: planned
    justifies: "FR-34, AC-39, NFR-11. Last language plus the documentation that tells an agent which modes work where — without the matrix, an unsupported-mode response reads as a bug rather than a known boundary."
    verification: "cargo test -p code-graph-lang-java fingerprint:: plus cargo test --workspace — LiteralInsensitive returns Some for Java (AC-39); no language returns None for either mode, completing FR-34 across the supported set; CLAUDE.md carries the per-language fingerprint support matrix and the symbol_history description reflects it (NFR-11); make verify passes."
    depends_on: ["8.2", "8.3", "8.4", "8.5"]
---

# Phase 8: Per-Language Fingerprints

## Overview

Six AST-backed `fingerprint_symbol` overrides, one per language plugin, replacing the text-based default and enabling `LiteralInsensitive` everywhere. Each is independently shippable; until a language's override lands, requesting that mode for it reports unsupported rather than degrading silently.

Depends on phase 6. This phase exists as required rather than opportunistic work by explicit direction — the spec's floor of two sensitivities is met per-language, and the decision was to deliver it universally rather than leave a ragged edge.

## 8.1: AST fingerprint override for C++

### Subtasks
- [x] Implement `fingerprint_symbol` on `CppParser`, locating the symbol's subtree from its line span
- [x] Hash `(node_kind, identifier_text)` pairs in a deterministic walk order
- [x] Exclude literal node values under `LiteralInsensitive`; include them under `Normalized`
- [x] Establish the shared walk shape the other five plugins will follow
- [x] Tests per the verification field, including a macro-stripped symbol

### Notes
Revision boundary: C++ supports both fingerprint modes; the other five still return `None` for `LiteralInsensitive`.

C++ goes first because it is the only plugin with a `preprocess` pass. The fingerprint must be computed against the same bytes the parse saw — fingerprinting raw bytes while parsing preprocessed ones produces spans that do not line up. Settle that interaction here, once.

The walk must be deterministic in traversal order, the same discipline as phase 1's community detection: any iteration over a hash-ordered collection reintroduces per-run variation, and the symptom is a cache that never hits rather than an obvious failure.

**How the interaction was settled.** The history walk now passes the SAME bytes the parse saw — the post-`preprocess` form — to `fingerprint_symbol`, and the trait doc states that as the contract (`core/history.rs` passes `&cleaned`; preprocess is byte-preserving, so line spans are identical either way for the text default). The shared walk lives in `code-graph-lang`'s now-public `fingerprint` module (`locate_symbol_node` + `ast_fingerprint`, iterative and recursion-free, enter/exit structure bytes, comments invisible under both modes, literal subtrees contributing kind always and text under `Normalized` only); plugins supply only their literal/comment kind predicates. Span-locate failure degrades per mode: `Normalized` falls back to the text default (continuity for `[cpp].macro_define_function` synthesized symbols), `LiteralInsensitive` returns `None` rather than pretending to know where literals are.

### Completion Evidence

- Verified: 2026-08-19
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `eb14751edf11325962bfe464a1a81d60024d7c36`
- Identity recheck: `git rev-parse HEAD` at 2026-08-19 15:07 matched `eb14751edf11325962bfe464a1a81d60024d7c36`
- Focused review: `git show eb14751edf11325962bfe464a1a81d60024d7c36`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `eb14751edf11325962bfe464a1a81d60024d7c36`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-lang-cpp fingerprint::` | `.` | PASS (`exit 0`) | `6 tests passed in the fingerprint module: LiteralInsensitive returns Some for C++; string AND numeric literal-only changes are visible under Normalized and invisible under LiteralInsensitive (AC-39); an operator change stays visible under both modes; reformatting and comment-only edits are invisible under both (AC-38); a macro-stripped class fingerprints identically across repeated calls and across parser instances; an unlocatable span degrades per mode (Normalized text fallback, LiteralInsensitive None).` |
| `cargo test -p code-graph-tools --test symbol_history && cargo test -p code-graph-lang && make verify` | `.` | PASS (`exit 0`) | `13 history integration tests stay green after the cleaned-bytes contract change; 65 lang unit tests green; clippy -D warnings, fmt, full workspace tests, snapshots, and plugin mirrors all green.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `focused diff review` | `git show eb14751` | PASS | `4 files: the shared helpers land in the NFR-02-protected crate using only DefaultHasher + the existing tree-sitter dep; the walk is cursor-iterative (no recursion, no hash-ordered iteration) with enter/exit brackets making sibling regrouping visible; the CppParser override parses content once per call and degrades per mode on locate failure; history.rs's one-line call change carries a comment naming the settled contract; the trait doc states content = the bytes the preceding parse saw.` |

### Trap
Re-parsing the whole file per symbol per revision. It is the obvious implementation and it makes history walks quadratic on files with many symbols. Parse once per `(revision, file)` and locate subtrees within that tree; the fingerprint cache from phase 6 then makes repeat queries free.

## 8.2: AST fingerprint override for Rust

### Subtasks
- [x] Implement `fingerprint_symbol` on `RustParser` following the 8.1 shape
- [x] Decide and document how attributes and doc comments participate in each mode
- [x] Pin that the AST walk supersedes the text default's lifetime-list mis-lex (gate artifact 18 follow-up: `<'a,'b>` vs `<'a, 'b>` hash differently under the text default — a rustfmt-only commit reported `modified`; the AST walk must hash them equal under both modes)
- [x] Tests per the verification field

### Notes
Revision boundary: Rust supports both modes.

Rust is the workspace's own language, so this override gets the most incidental exercise. If the abstraction established in 8.1 is awkward, it will show here — fix it here rather than replicating it four more times.

Doc comments are comments and should be invisible under both modes; attributes are code and should not be. `#[derive(...)]` changes behaviour, so a derive change is a logic change.

**The 8.1 abstraction DID show a gap here, exactly as predicted.** The Rust suite's rustfmt-shaped reformat fixture (multiline parameters + trailing comma) exposed that the shared walk hashed comma tokens: formatters ADD trailing commas when breaking lists (rustfmt always, gofmt necessarily), so a reformat-only commit would have reported `modified` — the AC-38 false positive. Fixed in the shared walk (commas invisible; the tree structure already encodes element boundaries; enter/exit brackets stay paired), so all six languages inherit the fix rather than five copies of the bug.

### Completion Evidence

- Verified: 2026-08-19
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `63f7e7c96fb5a933d34b06cd770d1ab95aa16c3e`
- Identity recheck: `git rev-parse HEAD` at 2026-08-19 15:16 matched `63f7e7c96fb5a933d34b06cd770d1ab95aa16c3e`
- Focused review: `git show 63f7e7c96fb5a933d34b06cd770d1ab95aa16c3e`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `63f7e7c96fb5a933d34b06cd770d1ab95aa16c3e`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-lang-rust fingerprint:: && cargo test -p code-graph-lang-cpp fingerprint::` | `.` | PASS (`exit 0`) | `5 Rust tests passed: LiteralInsensitive returns Some; string AND numeric literal-only changes track the mode (AC-39); a rustfmt-shaped reformat (multiline + trailing comma) is invisible under both modes (AC-38); the artifact-18 lifetime-list mis-lex is superseded (<'a,'b> == <'a, 'b> under both modes); doc comments invisible / attributes visible under both. The 6 C++ tests stay green after the shared-walk comma fix.` |
| `make verify` | `.` | PASS (`exit 0`) | `clippy -D warnings, fmt, full workspace tests, snapshots, and plugin mirrors all green.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `focused diff review` | `git show 63f7e7c` | PASS | `2 files: the override mirrors the 8.1 shape verbatim (raw bytes — Rust has no preprocess pass — with the same per-mode locate-failure degradation); the mode-participation decisions are documented on the override itself; the shared-walk comma fix is scoped to the enter/exit pair with the rationale in a comment naming the AC-38 failure it prevents.` |

## 8.3: AST fingerprint override for Go

### Subtasks
- [x] Implement `fingerprint_symbol` on `GoParser` following the 8.1 shape
- [x] Tests per the verification field

### Notes
Revision boundary: Go supports both modes.

Mechanical once 8.1 sets the pattern. Go's grammar has no preprocessing and no significant whitespace, so this is the most straightforward of the six.

Go is also the language that makes the 8.2 shared-walk comma fix load-bearing rather than cosmetic: breaking an argument list across lines REQUIRES a trailing comma in Go, so without the fix every gofmt multiline reformat would report `modified`. The reformat test exercises exactly that shape.

### Completion Evidence

- Verified: 2026-08-19
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `4367c572e054e517a717b92344a2cf55cf703c01`
- Identity recheck: `git rev-parse HEAD` at 2026-08-19 15:19 matched `4367c572e054e517a717b92344a2cf55cf703c01`
- Focused review: `git show 4367c572e054e517a717b92344a2cf55cf703c01`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `4367c572e054e517a717b92344a2cf55cf703c01`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-lang-go fingerprint:: && cargo clippy -p code-graph-lang-go --all-targets -- -D warnings && cargo fmt --all --check` | `.` | PASS (`exit 0`) | `4 tests passed: LiteralInsensitive returns Some for Go; string AND numeric literal-only changes are visible under Normalized and invisible under LiteralInsensitive (AC-39); a gofmt-shaped reformat (multiline args + mandatory trailing comma + doc comment) is invisible under both modes (AC-38); an operator change stays visible under both. Clippy denied no warnings. Full make verify rides with task 8.6's workspace gate.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `focused diff review` | `git show 4367c57` | PASS | `1 file: the override mirrors the 8.1 shape verbatim; literal-kind list covers tree-sitter-go v0.25's value literals with iota deliberately excluded (identifier, participates as code); no shared-walk changes needed — the 8.2 comma fix carried Go's mandatory-trailing-comma case.` |

## 8.4: AST fingerprint override for Python

### Subtasks
- [ ] Implement `fingerprint_symbol` on `PythonParser` following the 8.1 shape
- [ ] Verify that structural indentation changes are reported while cosmetic ones are not
- [ ] Tests per the verification field

### Notes
Revision boundary: Python supports both modes.

Python is where the text default is weakest and the AST override earns the most: collapsing whitespace runs is exactly wrong for a language where indentation determines block structure. A re-indent that moves a statement into or out of a block is a logic change, and the AST walk gets that right where the text default cannot.

### Completion Evidence

Pending — not complete.

## 8.5: AST fingerprint override for C#

### Subtasks
- [ ] Implement `fingerprint_symbol` on `CSharpParser` following the 8.1 shape
- [ ] Tests per the verification field

### Notes
Revision boundary: C# supports both modes.

Mechanical. Note that partial classes produce one symbol per declaration, so a fingerprint covers one declaration's span, not the merged type — consistent with how every other tool treats them.

### Completion Evidence

Pending — not complete.

## 8.6: AST fingerprint override for Java, and the support matrix

### Subtasks
- [ ] Implement `fingerprint_symbol` on `JavaParser` following the 8.1 shape
- [ ] Confirm no language returns `None` for either mode
- [ ] Remove the upfront data-independent `literal_insensitive` rejection in `core::history::symbol_history` (it exists solely because no plugin supported the mode — gate artifact 18 follow-up) and align the retained in-walk `None` arm's wording with the guard it becomes (future-language defense, not a phase-8 promise)
- [ ] Add the per-language fingerprint support matrix to CLAUDE.md
- [ ] Update the `symbol_history` tool description to state both modes are supported for all six languages
- [ ] Full workspace verification

### Notes
Revision boundary: FR-34 is complete across the supported language set and documented.

Java's anonymous-class method-name collisions mean two symbols can share an id, disambiguated only by line. The fingerprint is computed from the span, so colliding symbols fingerprint independently and correctly — but the phase 6 `(name, kind)` matcher will still pick one. That is the documented exact-match limitation, not a fingerprint defect; do not try to fix it here.

The support matrix is the deliverable that closes the loop: once every language supports both modes, the unsupported-mode path becomes unreachable in practice, and the documentation should say so rather than leaving agents to discover it.

### Completion Evidence

Pending — not complete.

## Acceptance Criteria

- [ ] **AC-39**: For every one of the six languages, a literal-only change yields a changed fingerprint under the formatting-insensitive mode and an unchanged one under the literal-insensitive mode (FR-34).
- [ ] **AC-38**: Reformatting remains invisible under both modes in every language (FR-34).
- [ ] No language returns `None` for either fingerprint mode; FR-34 is complete across the supported set.
- [ ] The per-language support matrix is documented in CLAUDE.md and reflected in the `symbol_history` description (NFR-11).
- [ ] No new third-party dependency in `code-graph-lang` or any language plugin crate (NFR-02).
- [ ] **AC-27**: `make verify` passes (NFR-04).

## Phase Completion Evidence

Pending — not complete.
