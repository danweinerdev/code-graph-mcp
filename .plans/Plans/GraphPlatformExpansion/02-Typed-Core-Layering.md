---
title: "Typed Core Layering"
type: phase
plan: GraphPlatformExpansion
phase: 2
status: planned
created: 2026-08-08
updated: 2026-08-08
deliverable: "A typed core beneath every MCP handler, returning domain values instead of rmcp wire types, with byte-identical output and server.rs untouched."
tasks:
  - id: "2.1"
    title: "Core module scaffolding: ToolOk, ToolError, adapter, core require_indexed"
    status: planned
    justifies: "FR-01, FR-02, FR-03, FR-04. Nothing else in the phase can land without the result type and the adapter; FR-03's three-outcome requirement is a type-design decision that every later task depends on."
    verification: "cargo test -p code-graph-tools core:: — the adapter maps Ok(Value) to tool_success_json, Ok(Text) to a text success, and Err to tool_error, each byte-identical to the corresponding helper today; a test module that does not import rmcp constructs a ToolOk and reads it, proving structured results are reachable without a wire type (AC-01); make verify passes."
  - id: "2.2"
    title: "Migrate status and watch handlers"
    status: planned
    justifies: "FR-01, FR-02, AC-02. The two smallest and least entangled modules; migrating them first proves the adapter pattern against real handlers before the harder modules commit to it."
    verification: "cargo test -p code-graph-tools status:: watch:: — existing assertions pass unmodified; the existing snapshot suite passes with no rebaseline (AC-02); core::get_status and core::watch_* return typed values and are callable without rmcp."
    depends_on: ["2.1"]
  - id: "2.3"
    title: "Migrate query handlers, including the non-callable advisory"
    status: planned
    justifies: "FR-03, FR-26, AC-03, AC-28, AC-33. get_callers/get_callees carry the plain-text advisory path — the one outcome that is neither a payload nor an error — so this task is what proves ToolOk's three-outcome shape against the case it exists for."
    verification: "cargo test -p code-graph-tools query:: — core::callers_or_callees on a non-callable kind returns Ok(ToolOk::Text(_)), distinguishable from both Value and Err, and the adapter still renders the plain-text success it does today (AC-03); an unindexed call returns Err(ToolError) discriminable without serialization (AC-28); existing assertions and snapshots unchanged."
    depends_on: ["2.1"]
  - id: "2.4"
    title: "Migrate symbols handlers"
    status: planned
    justifies: "FR-01, FR-05, FR-26, AC-29, AC-33. The symbols module owns the heaviest pagination and byte-budget paths, so it is where FR-05's 'budget applies in the core, not the wire layer' is actually demonstrated."
    verification: "cargo test -p code-graph-tools symbols:: — a byte-capped core result carries truncated true and a next_offset strictly past the last record as typed fields, and re-calling at that offset resumes with no gap or repetition (AC-29); SearchSymbolsInput borrows still compile against the core; existing assertions and snapshots unchanged."
    depends_on: ["2.1"]
  - id: "2.5"
    title: "Migrate structure handlers, including the mermaid text path"
    status: planned
    justifies: "FR-03, FR-26, AC-02, AC-33. generate_diagram(format=mermaid) is the second plain-text success the spec did not name; without migrating it through ToolOk::Text the variant would be modelled around one case and break on the other."
    verification: "cargo test -p code-graph-tools structure:: — core::generate_diagram returns Ok(ToolOk::Text(_)) for mermaid and Ok(ToolOk::Value(_)) for edges; get_coupling direction=both still produces the sequential byte-budget split; existing assertions and snapshots unchanged (AC-02)."
    depends_on: ["2.1"]
  - id: "2.6"
    title: "Migrate analyze handlers and complete the guard-coverage sweep"
    status: planned
    justifies: "FR-01, FR-04, NFR-01, NFR-02, NFR-05, AC-28, AC-41. analyze is last because it is async and owns progress reporting; the sweep is what stops Decision 8's new require_indexed call sites from being silently skipped by a mechanical pass."
    verification: "cargo test --workspace — analyze and analyze_async return typed values with progress bridged through an abstract sink rather than rmcp types; a comparison of the guarded set in server.rs against core/ shows the two sets are equal; cargo tree shows no tracing dependency (AC-41, NFR-05) and no new dependency in the four core crates (NFR-02); the full snapshot suite passes unmodified (NFR-01, AC-02)."
    depends_on: ["2.2", "2.3", "2.4", "2.5"]
---

# Phase 2: Typed Core Layering

## Overview

A pure refactor with no user-visible change. Logic moves from `handlers/*.rs` into a new `core/` module returning `ToolResult<T>`; each handler keeps its exact name and signature and becomes a one-line adapter. `server.rs` is never modified. The correctness condition is "nothing changed", so the existing snapshot suite is the primary gate.

Follows phase 1 and gates phase 7; independent of phases 3 and 5.

**Phase 1's three handlers are migrated here too.** `get_symbol_at`, `find_path`, and `detect_communities` land in `handlers/symbols.rs`, `handlers/query.rs`, and `handlers/structure.rs` — the same three files tasks 2.3 through 2.5 empty into `core/`. Migrating them alongside the originals is what makes them reachable from the CLI without duplicating response shaping or touching an rmcp type (FR-17, FR-26, AC-33), and it is why phase 2 follows phase 1 instead of running beside it: two worktrees, one adding handlers to these files and one gutting them, would conflict on all three.

## 2.1: Core module scaffolding: ToolOk, ToolError, adapter, core require_indexed

### Subtasks
- [ ] Create `crates/code-graph-tools/src/core/mod.rs` with `ToolOk<T>`, `ToolError`, `ToolResult<T>`
- [ ] Implement `to_call_tool_result` — the single place that references rmcp
- [ ] Add a core `require_indexed` returning `Result<(), ToolError>` with the message verbatim from today's version
- [ ] Add a test module with no rmcp import that constructs and reads a typed value
- [ ] Register `core` in `lib.rs`

### Notes
Revision boundary: the type and the adapter exist and are tested; no handler is migrated yet, so the crate compiles and every existing test passes untouched.

`ToolError` wraps a plain `String` deliberately and does **not** derive from `thiserror`. It is the typed form of a user-visible message, not an operational error — the workspace invariant is that user-visible errors travel as `CallToolResult` with the error flag, never as `Err(McpError)`, and conflating the two is how that invariant erodes.

### Completion Evidence

Pending — not complete.

## 2.2: Migrate status and watch handlers

### Subtasks
- [ ] Move `get_status` body to `core::status`, returning `ToolResult<StatusResult>`
- [ ] Move `watch_start`/`watch_stop` bodies to `core::watch`
- [ ] Reduce the three handlers to adapter calls
- [ ] Add core-level tests asserting on the typed values
- [ ] Confirm existing module tests and snapshots pass untouched

### Notes
Revision boundary: three handlers layered, everything else unchanged.

`get_status` is ungated — it must work before an index exists, so it does **not** get a `require_indexed` call. `watch_start`/`watch_stop` are gated and do. Check this against the 16-site list rather than assuming.

### Completion Evidence

Pending — not complete.

## 2.3: Migrate query handlers, including the non-callable advisory

### Subtasks
- [ ] Move `callers_or_callees`, `find_overrides`, `get_dependencies`, and phase 1's `find_path` to `core::query`
- [ ] Map the non-callable advisory to `Ok(ToolOk::Text(advisory))`, preserving the message byte for byte
- [ ] Map the symbol-not-found did-you-mean path to `Err(ToolError)`
- [ ] Add the core `require_indexed` call at each gated function's entry
- [ ] Core tests for the advisory, the error, and the empty-envelope trichotomy

### Notes
Revision boundary: the query module is layered and the three-outcome shape is proven against the case that motivated it.

The trichotomy must survive exactly: symbol-not-found → error; non-callable kind → text success; callable with zero resolved hops → empty `Page<CallChain>`. Collapsing the middle case into either neighbour is the failure FR-03 exists to prevent, and the advisory is a *success* — agents read it as guidance.

### Completion Evidence

Pending — not complete.

### Trap
The advisory looks like an error — it fires when there are no results and it tells the caller they did something unhelpful. Mapping it to `Err(ToolError)` would compile, pass most tests, and silently flip `is_error` on the wire. Only the AC-03 test catches it.

## 2.4: Migrate symbols handlers

### Subtasks
- [ ] Move `get_file_symbols`, `search_symbols`, `get_symbol_detail`, `get_symbol_summary`, and phase 1's `get_symbol_at` to `core::symbols`
- [ ] Keep `byte_budget_take` on the core path, with limit defaults resolved before the call
- [ ] Leave `SearchSymbolsInput<'a>` where it is; import it into the core
- [ ] Add the core `require_indexed` call at each gated function's entry
- [ ] Core tests for truncation and paging resume against typed fields

### Notes
Revision boundary: the symbols module is layered with pagination semantics preserved.

`byte_budget_take` carries a `debug_assert!(limit > 0)`; release builds silently return an empty non-truncated page instead of panicking. Every core function must resolve its limit default before calling — that obligation moves with the logic, and a release-mode empty page is a very quiet bug.

The `core` → `handlers::symbols` import for the input struct inverts the direction the architecture diagram implies. That is intentional (Designs/TypedCoreLayering Decision 4); do not "fix" it by moving the struct in this phase.

### Completion Evidence

Pending — not complete.

## 2.5: Migrate structure handlers, including the mermaid text path

### Subtasks
- [ ] Move `detect_cycles`, `get_orphans`, `get_class_hierarchy`, `find_class_candidates`, `get_coupling`, `generate_diagram`, and phase 1's `detect_communities` to `core::structure`
- [ ] Map the mermaid render to `Ok(ToolOk::Text(rendered))` and the edges format to `Ok(ToolOk::Value(_))`
- [ ] Preserve `get_coupling` direction=both sequential budget allocation exactly
- [ ] Leave `GenerateDiagramInput<'a>` in place; import it
- [ ] Add the core `require_indexed` call at each gated function's entry
- [ ] Core tests for both diagram formats and the dual-page budget split

### Notes
Revision boundary: the structure module is layered; both plain-text producers in the codebase now flow through `ToolOk::Text`.

`detect_cycles` is by-count pagination only and is deliberately not byte-budgeted. Do not "harmonise" it onto `byte_budget_take` while moving it — the asymmetry is intentional and documented.

### Completion Evidence

Pending — not complete.

## 2.6: Migrate analyze handlers and complete the guard-coverage sweep

### Subtasks
- [ ] Move `analyze_codebase` and `analyze_codebase_async` bodies to `core::analyze`
- [ ] Replace the rmcp `Peer`/`ProgressToken` parameters with an abstract progress sink; supply the rmcp-backed implementation from the adapter
- [ ] Verify the guarded set in server.rs and the set of core functions calling the core `require_indexed` are equal
- [ ] Run `cargo tree` for the NFR-02 and NFR-05 checks
- [ ] Full workspace test run and snapshot verification

### Notes
Revision boundary: the layering is complete across all six modules and the guard coverage is proven.

The progress bridge is the only genuinely non-trivial piece: `run_analyze_job` forwards channel events to `peer.notify_progress`. The indexer already defines a `ProgressSink` trait, so the core takes `&dyn ProgressSink` and the adapter spawns the rmcp forwarder. Resist making the adapter fat — if the forwarding task grows past a few lines, that is a sign the sink abstraction is wrong, not that the adapter should absorb it.

The guard sweep is a task subtask rather than a separate task because it must be verified per module as each lands; deferring it to the end is how it gets skipped.

### Completion Evidence

Pending — not complete.

### Trap
It is tempting to drop the `require_indexed` call from `server.rs` once the core has one, to avoid checking twice. Don't — that edits `server.rs`, forfeiting the guarantee that makes this whole phase low-risk, and it saves one relaxed atomic load.

## Acceptance Criteria

- [ ] **AC-01**: Every tool's structured result is obtainable by calling a typed function with no rmcp type referenced (FR-01).
- [ ] **AC-02**: The full existing snapshot suite passes unmodified (NFR-01).
- [ ] **AC-03**: The non-callable advisory is representable as a third outcome and renders as a plain-text success (FR-03).
- [ ] **AC-28**: An unindexed query yields a discriminable domain error without serialization (FR-04).
- [ ] **AC-29**: Byte-budget truncation and paging resume are correct against typed fields (FR-05).
- [ ] **AC-41**: No `tracing` in the dependency graph; diagnostics use `eprintln!` (NFR-05).
- [ ] **AC-27**: `make verify` passes at every commit in the phase, not only at the end (NFR-04).
- [ ] No new dependency in `code-graph-core`, `-graph`, `-lang`, `-path-trie` (NFR-02).
- [ ] FR-01, FR-02, FR-03, FR-04, FR-05 realized; NFR-01 preserved.

## Phase Completion Evidence

Pending — not complete.
