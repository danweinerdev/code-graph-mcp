---
title: "generate_diagram direction + dedupe + file-leak fix"
type: phase
plan: ResponseShapePolish
phase: 3
status: complete
created: 2026-05-13
updated: 2026-05-15
deliverable: "`generate_diagram(symbol=...)` accepts a new `direction: \"callees\" | \"callers\" | \"both\"` argument (default `Both` for backwards-friendly UX). `DiagramEdge` carries a `direction` tag rendered as solid `-->` (calls) or dashed `-.->` (called_by). Unresolved call targets are filtered out — `mermaid_label` returns `Option<String>` and `None`-endpoint edges drop. Dedupe runs on rendered `(label, label, direction)` triples — collapses the user-reported triple-duplicate."
tasks:
  - id: "3.1"
    title: "DiagramDirection enum + diagram_call_graph signature update"
    status: complete
    verification: "`crates/code-graph-graph/src/diagrams.rs` defines `pub enum DiagramDirection { #[serde(rename = \"callees\")] Callees, #[serde(rename = \"callers\")] Callers, #[serde(rename = \"both\")] Both }` with serde rename attributes. `diagram_call_graph` signature changes from `(start_id, depth, max_nodes)` to `(start_id, direction, depth, max_nodes)`. The BFS body at `diagrams.rs:184-269` is updated: `adj` traversal (callees) is gated on `direction != Callers`; `radj` traversal (callers) is gated on `direction != Callees`. `Both` traverses both (preserves today's behavior). The handler in `crates/code-graph-tools/src/handlers/structure.rs` is updated to accept the new `direction` arg from MCP tool input; default is `Both` when absent."
  - id: "3.2"
    title: "mermaid_label returns Option<String> + filter unresolved endpoint edges"
    status: complete
    verification: "`mermaid_label` at `crates/code-graph-graph/src/diagrams.rs:564-578` signature changes from `fn mermaid_label(id: &str, nodes: &HashMap<SymbolId, Node>) -> String` to `fn mermaid_label(id: &str, nodes: &HashMap<SymbolId, Node>) -> Option<String>`. Returns `Some(name)` when the id resolves to a node in `self.nodes`; returns `None` for unresolved (where today the fallback returned `Path::file_name()` — the file-basename leak). At the diagram-build site (`diagrams.rs:251-267`), each edge's `from` and `to` are now `Option<String>`; if EITHER is `None`, the edge is filtered out and does NOT appear in `result.edges`. Unit test: build a graph with a known unresolved target (a `Calls` edge whose target SymbolId is not in `self.nodes`); call `diagram_call_graph`; assert no edge with a file-basename name appears in `result.edges`."
  - id: "3.3"
    title: "Dedupe on rendered (label, label, direction) triples"
    status: complete
    verification: "The dedupe set in `diagram_call_graph` (today `seen: HashSet<(String, String)>` over raw IDs at `diagrams.rs:250`) becomes `seen: HashSet<(String, String, EdgeDirection)>` over rendered labels and the per-edge direction tag (`EdgeDirection` is the variant set `Calls`/`CalledBy` defined in 3.4 — NOT the user-input `DiagramDirection` request enum). The check moves to AFTER `mermaid_label` materializes both endpoints (so two distinct IDs that reduce to the same label are deduped). The first occurrence of each rendered triple wins (no merging or tiebreak). The dedupe is intentionally lossy — explicit comment in the code: \"Two symbols with the same rendered label collapse into one diagram edge by design; clients needing ID-level fidelity should call `get_callers`/`get_callees`.\" Test pinning the user-reported triple-duplicate case: fixture with two functions named `Tick` in different files, both calling a function named `Update`; assert exactly ONE edge `Tick -->|calls| Update` survives in `result.edges`."
    depends_on: ["3.1", "3.2", "3.4"]
  - id: "3.4"
    title: "DiagramEdge.direction field + Mermaid rendering update"
    status: complete
    verification: "`DiagramEdge` struct gains `pub direction: DiagramDirection` field (serialized via the rename attributes from 3.1 — `\"calls\"` for outgoing/callees, `\"called_by\"` for incoming/callers — adjust the rename to match the wire-format strings the design specified: rendered labels are `\"calls\"` and `\"called_by\"` per design, NOT `\"callees\"`/`\"callers\"`; the enum's user-input rename is different from the edge-tag rename). Resolution: define a separate `EdgeDirection` enum with `Calls`/`CalledBy` variants for the per-edge tag, distinct from `DiagramDirection` which is the user-input request mode. `render_mermaid` (the function that walks `DiagramEdge` and produces Mermaid syntax) is updated to emit `-->|calls| n1` for `EdgeDirection::Calls` and `-.->|called by| n1` for `EdgeDirection::CalledBy` (dashed line for incoming). Test fixture with mixed direction: assert the rendered Mermaid output contains both `-->` AND `-.->` arrows."
    depends_on: ["3.1"]
  - id: "3.5"
    title: "Integration tests: direction filter, label dedupe, file-leak gone"
    status: complete
    verification: "Three integration tests in `crates/code-graph-tools/tests/` (or matching the existing diagram-test location): (a) `generate_diagram_direction_callees_only` — fixture A calls B, C calls A; call `generate_diagram(symbol=A, direction=callees)`; assert the result contains only the A→B edge (no C→A); (b) `generate_diagram_label_dedupe_pins_user_report` — the 3.3 fixture; assert single edge; (c) `generate_diagram_no_file_basename_leak` — synthetic graph with an unresolved call target (manually inject a `Calls` edge whose target SymbolId is not a graph node); call `generate_diagram(symbol=A)`; assert the result.edges does NOT contain any entry whose name matches a `*.cpp`/`*.h`/`*.rs`/etc. file basename. All three tests run on the existing test-harness pattern."
    depends_on: ["3.4"]
  - id: "3.6"
    title: "Tool description update"
    status: complete
    verification: "`generate_diagram` tool description in `server.rs` is updated to describe: (a) the new `direction` arg with its three values and default `both`; (b) the `DiagramEdge.direction` field semantics (`\"calls\"` solid, `\"called_by\"` dashed); (c) the lossy-dedupe note: \"Edges that render to the same Mermaid label collapse into one. Clients needing ID-level fidelity should call `get_callers`/`get_callees` instead.\"; (d) the unresolved-target filter: \"Calls to symbols not in the index are dropped — they no longer appear as file-basename pseudo-nodes.\". Tool-list snapshot regenerates; `cargo insta review` accepts deliberately."
    depends_on: ["3.5"]
  - id: "3.7"
    title: "Structural verification"
    status: complete
    verification: "`cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all --check` clean; `cargo test --workspace` green; `make snapshot-clean` passes; existing `generate_diagram` snapshot tests regenerate deliberately (the response shape changed by adding `direction` field on edges) — accept via `cargo insta review`."
    depends_on: ["3.6"]
tags: [mcp, pagination, ue, unreal-engine, ergonomics, hierarchy, diagrams, coupling, dependencies, fuzzy-match]
---

# Phase 3: generate_diagram direction + dedupe + file-leak fix

## Overview

Three independent fixes bundled into one phase because they all touch `diagram_call_graph` and its surrounding helpers. The user reported a triple-duplicate edge, a file-basename "Platform.cpp" appearing as a caller, and ambiguous direction mixing. Each fix is a discrete change to a different part of the same function; bundling lets a reviewer see them in a single diff.

## 3.1: DiagramDirection enum + diagram_call_graph signature update

### Subtasks
- [x] Define `DiagramDirection` enum (3 variants, serde-renamed for the user-facing JSON)
- [x] Update `diagram_call_graph` signature to accept the new arg
- [x] In the BFS body at `diagrams.rs:184-269`, gate `adj` traversal on `direction != Callers` and `radj` traversal on `direction != Callees`
- [x] Default `Both` preserves today's traverse-both behavior; `Callees` and `Callers` are the new opt-in modes
- [x] Handler update in `structure.rs`: parse the new `direction` arg from MCP input; default to `Both` when absent
- [x] Add `#[serde(default)]` on the handler's Args struct field so old client invocations (no `direction`) still work

### Notes
The `Both` default is the back-compat-friendly choice for the small set of clients that already use `generate_diagram(symbol=...)` and don't know about the new arg. New clients seeing the description should choose explicitly.

## 3.2: mermaid_label returns Option<String> + filter unresolved endpoint edges

### Subtasks
- [x] Change `mermaid_label` signature at `diagrams.rs:564-578` to return `Option<String>`
- [x] Body: `nodes.get(id).map(|n| <name extraction>)` (today's primary path); on `None`, return `None` (today's fallback returned `Path::file_name()` — DELETE this branch)
- [x] In the diagram-build loop at `diagrams.rs:251-267`, change the materialization to:
  ```rust
  let (from_label, to_label) = match (mermaid_label(&from, &self.nodes), mermaid_label(&to, &self.nodes)) {
      (Some(f), Some(t)) => (f, t),
      _ => continue, // drop edges with unresolved endpoint
  };
  ```
- [x] Update any other callers of `mermaid_label` (e.g., the `center` field on `DiagramResult` at :247) to handle the new `Option` return — likely a `.unwrap_or_else(|| ...)` with the symbol id as fallback so the center renders something useful even if its label resolution fails

### Notes
The center-resolution edge case matters: `generate_diagram(symbol=foo)` where `foo` resolves should always succeed (handler pre-checks `self.nodes.contains_key(start_id)` at :190-192). But if the symbol's `name` field is empty, `mermaid_label` returns `None`; handle this gracefully — return the SymbolId as the center label.

## 3.3: Dedupe on rendered (label, label, direction) triples

### Subtasks
- [x] Change `seen` type from `HashSet<(String, String)>` to `HashSet<(String, String, EdgeDirection)>` (the per-edge direction enum from 3.4)
- [x] Move the dedupe check to AFTER `mermaid_label` has materialized both endpoint labels — this is the key behavioral change vs today (where dedupe was over raw IDs)
- [x] Add a code comment: "Dedupe on rendered labels collapses two distinct symbols with identical (parent::name) into one edge. Acceptable lossiness for visual coherence; clients needing ID-level fidelity should call get_callers/get_callees."
- [x] Add a test fixture that produces the user-reported triple-duplicate (two `Tick` functions in different files both calling a third `Update` function)

### Notes
The lossy-dedupe is a real loss: two functions with the same parent::name in different files produce one edge. This is by design — the alternative (preserving them but rendering "Tick (file1.cpp)" / "Tick (file2.cpp)" as distinct nodes) explodes the Mermaid output. Document the trade explicitly so users who need ID-level fidelity know where to go (get_callers/get_callees).

## 3.4: DiagramEdge.direction field + Mermaid rendering update

### Subtasks
- [x] Define `EdgeDirection { Calls, CalledBy }` (distinct from `DiagramDirection` user-input enum) with serde renames `"calls"` and `"called_by"`
- [x] Add `pub direction: EdgeDirection` field to `DiagramEdge`
- [x] When pushing edges in the BFS: edges from `adj` traversal get `EdgeDirection::Calls`; edges from `radj` traversal get `EdgeDirection::CalledBy`
- [x] Locate `render_mermaid` (the function that walks `DiagramEdge` and emits Mermaid syntax) — update it to emit `n0 -->|calls| n1` for `Calls` and `n0 -.->|called by| n1` for `CalledBy` (dashed arrow for the incoming direction)
- [x] Update the function's doc comment to note the new dashed-line branch

### Notes
The two-enum split (DiagramDirection user-input vs EdgeDirection per-edge tag) is intentional. They serialize to different strings; mixing them would force one to use awkward names. Resolution: name them differently in Rust, rely on serde's per-enum renames for the wire format.

## 3.5: Integration tests: direction filter, label dedupe, file-leak gone

### Subtasks
- [x] `generate_diagram_direction_callees_only` — fixture A calls B; C calls A; `generate_diagram(symbol=A, direction=callees)`; assert only A→B edge in result
- [x] `generate_diagram_direction_callers_only` — same fixture, `direction=callers`; assert only C→A edge
- [x] `generate_diagram_label_dedupe_pins_user_report` — fixture per 3.3; assert exactly one edge in result.edges
- [x] `generate_diagram_no_file_basename_leak` — synthetic graph with an unresolved target; assert no file-basename pseudo-node appears
- [x] Each test pattern-matches the existing diagram-test idiom (look at `tests/diagram_*.rs` for the closest existing pattern)

### Notes
The four tests pin the four distinct behavioral changes (direction filter ×2 modes, dedupe, file-leak). Each is a focused regression target.

## 3.6: Tool description update

### Subtasks
- [x] Rewrite `generate_diagram` tool description to cover: new `direction` arg, per-edge `direction` field, lossy dedupe note, unresolved-target filter
- [x] Apply the Agent-facing tool descriptions lens
- [x] Run `cargo test`; `cargo insta review` for the tool-list snapshot

### Notes
The lossy-dedupe disclosure is the trickiest message — it admits that the tool can lose information by design. Get the wording precise so it doesn't read as a bug.

## 3.7: Structural verification

### Subtasks
- [x] Run `cargo clippy --workspace --all-targets -- -D warnings`
- [x] Run `cargo fmt --all --check`
- [x] Run `cargo test --workspace`
- [x] Run `make snapshot-clean`
- [x] Review and accept the regenerated diagram + tool-list snapshots deliberately

## Acceptance Criteria
- [x] `DiagramDirection` enum and signature change in place
- [x] `mermaid_label` returns `Option<String>`; file-basename fallback deleted
- [x] Dedupe over rendered labels with code comment documenting the lossy choice
- [x] `DiagramEdge.direction` field with `EdgeDirection` enum; Mermaid renderer emits dashed line for `CalledBy`
- [x] 4 integration tests pass
- [x] Tool description rewritten
- [x] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [x] `cargo fmt --all --check` clean
- [x] `make snapshot-clean` passes
