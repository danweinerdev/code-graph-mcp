---
title: "get_class_hierarchy ref-dedupe"
type: phase
plan: ResponseShapePolish
phase: 2
status: complete
created: 2026-05-13
updated: 2026-05-15
deliverable: "`HierarchyNode` gains `ref: Option<bool>` (skip_serializing_if Option::is_none). First visit to a name emits full subtree; subsequent visits in different DFS paths emit `{name, ref: true}` stub. Cycle guard (`on_path`) still wins over ref-stub (`visited_unique`). Diamond inheritance no longer duplicates subtree serializations inline."
tasks:
  - id: "2.1"
    title: "Add ref field to HierarchyNode"
    status: complete
    verification: "`crates/code-graph-graph/src/algorithms.rs:40-47` `HierarchyNode` struct gains `#[serde(default, skip_serializing_if = \"Option::is_none\")] pub r#ref: Option<bool>`. The `r#ref` identifier uses raw-identifier syntax because `ref` is a Rust keyword. JSON field name remains `ref` via `#[serde(rename = \"ref\")]` if needed (verify by snapshot: existing `{name}` leaf nodes still emit `{\"name\": \"X\"}` with NO `ref` field). Field placement: AFTER `bases` and `derived` in the struct definition (consistent with the existing `skip_serializing_if` convention for those fields). Unit test asserts a leaf `HierarchyNode { name, bases: vec![], derived: vec![], ref: None }` serializes to byte-identical `{\"name\":\"X\"}` as today."
  - id: "2.2"
    title: "Update build_hierarchy walk with ref-stub branch"
    status: complete
    verification: "`build_hierarchy` in `crates/code-graph-graph/src/algorithms.rs:194-315` is updated. The walk currently uses `on_path: HashSet<&str>` (per-DFS-path cycle guard) and `visited_unique: HashSet<&str>` (global, for `max_nodes` budget). New behavior: AT EACH NODE VISIT, check order is (1) `on_path` first — if name is in `on_path`, emit a bare-leaf `HierarchyNode { name, bases: vec![], derived: vec![], ref: None }` (today's cycle-guard behavior preserved); (2) `visited_unique` second — if name is in `visited_unique` but NOT in `on_path`, emit a ref-stub `HierarchyNode { name, bases: vec![], derived: vec![], ref: Some(true) }` and do NOT recurse; (3) else — first visit, insert into BOTH `on_path` and `visited_unique`, recurse to build full subtree, remove from `on_path` on the way back up. `visited_unique` continues to gate the `max_nodes` budget check. The check order MUST be on_path first; reversing it would mishandle a self-cycle that's also on a different DFS path."
    depends_on: ["2.1"]
  - id: "2.3"
    title: "Test fixtures: diamond, no-diamond, cycle"
    status: complete
    verification: "Three test fixtures in `crates/code-graph-graph/tests/` (or under `algorithms.rs::tests` if that's the existing convention): (a) `hierarchy_diamond_emits_ref_stub` — fixture where D inherits from B1 AND B2, both inherit from A; call `get_class_hierarchy(\"D\", up)`; assert A appears ONCE with its full base list AND once as `{name: \"A\", ref: Some(true)}` stub; serialize the response and assert the byte-length is substantially smaller than today's duplicated form (rough check: serialized length under the no-ref baseline by at least 20%); (b) `hierarchy_no_diamond_omits_ref_field` — fixture with linear inheritance only; assert NO `\"ref\"` field appears anywhere in the JSON output (test via string contains assertion on the serialized form); (c) `hierarchy_cycle_emits_bare_leaf_not_ref` — fixture A inherits from B, B inherits from A; assert the cycle path emits `{name: \"A\"}` bare leaf at the cycle point (no `ref` field), proving the on_path guard wins over visited_unique."
    depends_on: ["2.2"]
  - id: "2.4"
    title: "Tool description update for ref-stub semantics"
    status: complete
    verification: "`get_class_hierarchy` tool description in `crates/code-graph-tools/src/server.rs` is updated to describe the new shape: \"`HierarchyNode` may carry `ref: true` for shared bases/derived in multi-inheritance (diamond) graphs. The first reachable occurrence of a name in DFS pre-order is the canonical node with full `bases`/`derived`; subsequent occurrences are `{name, ref: true}` stubs. Clients reconstructing the full tree should maintain a `name -> node` map keyed on the first non-ref occurrence.\" Cycles still emit bare leaves (no `ref` field) — documented in the same description. Tool-list snapshot for `get_class_hierarchy` regenerates; `cargo insta review` accepts deliberately."
    depends_on: ["2.3"]
  - id: "2.5"
    title: "Structural verification"
    status: complete
    verification: "`cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all --check` clean; `cargo test --workspace` green; `make snapshot-clean` passes; existing 49 corpus tests stay green (no behavior change on hierarchies without diamonds); the `total_nodes_seen` semantics from CLAUDE.md (\"unique class names walked — diamond ancestor = 1 slot, not 1-per-arm\") are preserved by the walk change."
    depends_on: ["2.4"]
tags: [mcp, pagination, ue, unreal-engine, ergonomics, hierarchy, diagrams, coupling, dependencies, fuzzy-match]
---

# Phase 2: get_class_hierarchy ref-dedupe

## Overview

`HierarchyNode` recursive serialization currently duplicates a shared base's full subtree at every diamond arm. For `UObject` at depth=3 on a generic UE project, `FTickableGameObject`'s 40+-class derived list serializes inline at every multi-inheritance point — the response balloons by 5–10× on multi-inheritance hierarchies.

This phase adds a `ref: Option<bool>` field to `HierarchyNode` so subsequent visits emit a `{name, ref: true}` stub instead of re-serializing the subtree. Clients walking the tree maintain a `name -> node` map keyed on the first non-ref occurrence; ref-stubs point back to that canonical node.

The change preserves cycle-handling: when a name is on the current DFS path (in `on_path`), the existing bare-leaf cycle guard fires — NOT the ref-stub. This keeps cycles distinguishable from legitimate diamonds in the output.

## 2.1: Add ref field to HierarchyNode

### Subtasks
- [x] Open `crates/code-graph-graph/src/algorithms.rs:40-47` (the `HierarchyNode` struct)
- [x] Add field: `#[serde(default, skip_serializing_if = "Option::is_none")] pub r#ref: Option<bool>,` after the existing `bases` and `derived` fields
- [x] Use `r#ref` raw-identifier syntax (Rust keyword); verify JSON field name is `"ref"` (serde default uses the raw identifier name stripped of the `r#` prefix)
- [x] If serde does NOT strip the `r#` automatically, add `#[serde(rename = "ref")]`
- [x] Update the struct-level doc comment to describe the new field's semantics
- [x] Smoke test: construct `HierarchyNode { name: "X".into(), bases: vec![], derived: vec![], r#ref: None }`; serialize; assert the JSON is `{"name":"X"}` — no `ref` field present (skip_serializing_if dropped it)

### Notes
The field placement matters for serialization stability. Both `bases` and `derived` have `skip_serializing_if` already; the new field follows the same pattern and ships in the same position. Reordering existing fields would be a wire-format break.

## 2.2: Update build_hierarchy walk with ref-stub branch

### Subtasks
- [x] Open `crates/code-graph-graph/src/algorithms.rs:194-315` (the `build_hierarchy` function — note: actual line range may differ; locate by function name)
- [x] Find the existing `visited_unique` check at lines ~267-272 and ~293-298 (these are the budget-gate sites per the prior researcher report)
- [x] At each visit point, restructure the conditional to a three-way branch:
  1. `if on_path.contains(name) { return HierarchyNode { name: name.into(), bases: vec![], derived: vec![], r#ref: None }; }` — cycle guard (today's behavior)
  2. `else if visited_unique.contains(name) { return HierarchyNode { name: name.into(), bases: vec![], derived: vec![], r#ref: Some(true) }; }` — NEW ref-stub
  3. `else { ... insert into both sets, recurse, return full node ... }` — first visit
- [x] Verify `on_path` is correctly maintained (inserted on entry, removed on exit) — preserve today's pattern
- [x] Verify `visited_unique` is inserted on first visit only and NOT removed (it's global)
- [x] Verify the `max_nodes` budget check still consults `visited_unique` correctly — the ref-stub branch should NOT count against `max_nodes` because the unique-name was already counted on first visit; refs are free
- [x] Update the comment block at the start of `build_hierarchy` describing the walk semantics to mention the new ref-stub branch

### Notes
The cycle-vs-diamond distinction is the load-bearing semantic. A cycle is "same name reachable along the current DFS path" (handled by `on_path`); a diamond is "same name reachable along a different DFS path that's already finished" (handled by `visited_unique`). Both produce stub nodes, but only diamonds get `ref: true`. The bare leaf on cycles signals "infinite traversal would have happened here"; the `ref: true` stub signals "this is shared, look up the canonical".

## 2.3: Test fixtures: diamond, no-diamond, cycle

### Subtasks
- [x] `hierarchy_diamond_emits_ref_stub`: build a synthetic graph in-memory with D inheriting from B1 and B2 (both `Inherits` edges), and B1/B2 inheriting from A. Call `Graph::class_hierarchy("D", direction=both, max_nodes=100)`. Walk the result and assert: A appears as a full node (with non-empty `bases`) AT MOST ONCE in non-ref form; A appears as `{name: "A", ref: Some(true)}` stub at the second visit point
- [x] Serialize the diamond response with `serde_json::to_string`; compare byte length to the response with all-full nodes (pre-fix simulation by manually setting `ref` to `None` on the stub). Assert the ref-form is at least 20% shorter on a fixture with a non-trivial subtree under A
- [x] `hierarchy_no_diamond_omits_ref_field`: linear inheritance D → B → A. Serialize; assert `!serialized.contains("\"ref\":")` — no ref field anywhere
- [x] `hierarchy_cycle_emits_bare_leaf_not_ref`: A inherits from B, B inherits from A. Call `class_hierarchy("A", direction=down)`. At the cycle return point (B's derived contains A), assert the A stub is `{name: "A"}` (no `ref` field), NOT `{name: "A", ref: true}` — pins the precedence

### Notes
The "byte length is shorter" assertion in the diamond test is the proof-of-purpose: without it, the test passes even if the ref-stub mechanism is wired but produces no actual size reduction.

## 2.4: Tool description update for ref-stub semantics

### Subtasks
- [x] Locate `get_class_hierarchy` tool description in `server.rs`
- [x] Add a paragraph describing the new behavior:
  - First DFS occurrence is canonical (full `bases`/`derived`)
  - Subsequent diamond occurrences emit `{name, ref: true}` stubs
  - Cycles emit bare `{name}` leaves (no `ref` field)
  - Client reconstruction: maintain a `name -> node` map keyed on first non-ref occurrence
- [x] Run `cargo test` — regenerate the tool-list snapshot
- [x] `cargo insta review` — accept deliberately

### Notes
The client-reconstruction guidance is the most-overlooked detail in shape changes like this. Without explicit instructions, a client walking the tree could naively treat a ref stub as a leaf and lose information.

## 2.5: Structural verification

### Subtasks
- [x] Run `cargo clippy --workspace --all-targets -- -D warnings`
- [x] Run `cargo fmt --all --check`
- [x] Run `cargo test --workspace`
- [x] Run `make snapshot-clean`
- [x] Verify the `total_nodes_seen` field on `get_class_hierarchy` responses still reports unique names (not unique-visits) — this is the existing contract per CLAUDE.md and must not change

## Acceptance Criteria
- [x] `HierarchyNode.ref` field added with correct serde attributes
- [x] `build_hierarchy` walk implements on_path → visited_unique → first-visit precedence
- [x] 3 test fixtures (diamond, no-diamond, cycle) pin the new behavior
- [x] Tool description rewritten and snapshot accepted
- [x] `total_nodes_seen` semantics unchanged
- [x] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [x] `cargo fmt --all --check` clean
- [x] `make snapshot-clean` passes
