---
title: "Tree-shaped get_class_hierarchy + cutover"
type: phase
plan: PaginationOverhaul
phase: 4
status: planned
created: 2026-05-07
updated: 2026-05-07
deliverable: "`get_class_hierarchy` accepts `max_nodes` (default 250, cap 1000) and returns `{ hierarchy, truncated, max_nodes, total_nodes_seen }`. The Graph layer's `class_hierarchy` budget counts unique node names so diamond inheritance doesn't burn the budget twice for shared ancestors. CLAUDE.md, README.md, and the MCP tool description strings are updated. All snapshots reviewed and approved."
tasks:
  - id: "4.1"
    title: "Modify Graph::class_hierarchy to accept max_nodes and return (root, total_nodes_seen, truncated)"
    status: planned
    verification: "`crates/codegraph-graph/src/algorithms.rs::build_hierarchy` (or its public wrapper in `queries.rs`) accepts a `max_nodes: u32` argument. A new global `HashSet<String>` (unique names) is threaded through both up-walk and down-walk DFS, separate from the existing per-path `on_path` cycle guard. Each newly-encountered class name is added to the set; if `set.len() >= max_nodes` before recursing into a child, recursion stops adding the child but the partial parent is still returned. The function returns `(HierarchyNode, u32 /*total_nodes_seen*/, bool /*truncated*/)`. Diamond inheritance: a class reached via N paths counts ONCE in the budget, ONCE in `total_nodes_seen`. Existing per-path cycle protection is unchanged. All existing `class_hierarchy_*` unit tests in `codegraph-graph` continue to pass with `max_nodes=u32::MAX` (effectively unbounded — proves backward compatibility of the algorithm)."
  - id: "4.2"
    title: "Add Graph-layer unit tests for max_nodes truncation + diamond"
    status: planned
    depends_on: ["4.1"]
    verification: "Two Graph-layer tests in `codegraph-graph`: (a) `class_hierarchy_max_nodes_truncates` — fixture with at least 11 classes in a hierarchy, request `max_nodes=10`, asserts `truncated=true`, `total_nodes_seen=10`, partial tree contains exactly 10 unique names. (b) `class_hierarchy_diamond_counts_unique_names` — uses a diamond fixture where the shared ancestor is reachable via 2 paths so the total visit count strictly exceeds the unique-name count (e.g. 4 unique nodes, 5 visits). Request `max_nodes=4` (= unique count, < visit count). Asserts `truncated=false`, `total_nodes_seen=4`, all four unique names appear in the tree. **A naïve visit-counting implementation would truncate at 4 visits and miss a node — this test exists specifically to catch that bug; the assertion `truncated=false` with all four names present is what discriminates correct unique-name counting from incorrect visit counting.** A regression test also confirms `max_nodes=u32::MAX` produces the same tree as today (proves the algorithm change is backward-compatible when given an unbounded budget). The zero-sentinel test does NOT live in the Graph layer — see task 4.4 for the handler-layer test that resolves zero to the default."
  - id: "4.3"
    title: "Extend GetClassHierarchyArgs with max_nodes"
    status: planned
    depends_on: ["4.2"]
    verification: "`server.rs::GetClassHierarchyArgs` gains `max_nodes: Option<u32>` with `#[serde(default)]`. The `#[tool(description=...)]` text documents the arg, default (250), ceiling (1000), and the new response shape with `truncated` and `total_nodes_seen`. `cargo build -p codegraph-tools` succeeds."
  - id: "4.4"
    title: "Rewrite get_class_hierarchy handler to wrap response"
    status: planned
    depends_on: ["4.3"]
    verification: "`handlers::structure::get_class_hierarchy` accepts `max_nodes`; resolves default 250; clamps at 1000; treats 0 as 'use default'. Calls `Graph::class_hierarchy` with the resolved budget. **The existing `class not found: <name>. Did you mean: ...?` error path is preserved unchanged** — `Graph::class_hierarchy` still returns `None` on not-found, and the handler's existing did-you-mean branch (calls `suggest_class_symbols`) continues to fire. On `Some(...)`, the handler unpacks the new `(root, total_nodes_seen, truncated)` 3-tuple and wraps it in a `ClassHierarchyResponse { hierarchy: HierarchyNode, truncated: bool, max_nodes: u32, total_nodes_seen: u32 }` struct (private to `structure.rs`, mirroring the existing pattern). `tool_success_json` it. New unit test `class_hierarchy_handler_zero_max_nodes_uses_default_250` confirms the handler resolves `max_nodes=0` to 250 before forwarding (matches the convention used by `orphans_zero_limit_uses_default`)."
  - id: "4.5"
    title: "Wire max_nodes through ServerInner::get_class_hierarchy"
    status: planned
    depends_on: ["4.4"]
    verification: "`ServerInner::get_class_hierarchy` forwards `args.max_nodes` to the handler. No default-resolution in `ServerInner`."
  - id: "4.6"
    title: "Update existing class_hierarchy snapshots (×4)"
    status: planned
    depends_on: ["4.5"]
    verification: "Four existing snapshots regenerate to the wrapped shape and are approved: `response_get_class_hierarchy_engine.snap`, `response_get_class_hierarchy_rust_trait_greet.snap`, `response_get_class_hierarchy_go_interface_reader.snap`, `response_get_class_hierarchy_python_dog.snap`. For each, confirm: `hierarchy` field contains the same tree as the prior bare-root snapshot; `truncated: false` (all four fixtures fit easily under max_nodes=250); `max_nodes: 250` echoed; `total_nodes_seen` equals the unique-name count of the tree."
  - id: "4.7"
    title: "Add new snapshot: class_hierarchy truncated"
    status: planned
    depends_on: ["4.6"]
    verification: "New test `response_get_class_hierarchy_truncated` builds a fixture with >250 classes in a hierarchy (or uses a small `max_nodes` like 5 on an existing fixture). Snapshot asserts `truncated: true`, `total_nodes_seen` equals the cap, and the partial tree is well-formed (no dangling references). Approved via `cargo insta review`."
  - id: "4.8"
    title: "Update class_hierarchy tools-list snapshot"
    status: planned
    depends_on: ["4.7"]
    verification: "`snapshot_tools_list/*get_class_hierarchy*.snap` regenerates with `max_nodes` in `inputSchema` and is approved. Description text from 4.3 surfaces correctly."
  - id: "4.9"
    title: "Update CLAUDE.md and README.md tool surface documentation"
    status: planned
    depends_on: ["4.8"]
    verification: "`CLAUDE.md` MCP Tools section documents the new args (`limit`/`offset` on get_orphans, get_file_symbols, get_callers, get_callees; `max_nodes` on get_class_hierarchy; `brief` on get_orphans). `README.md` (top-level project README) tool surface section, if it lists args, gets the same updates. Wording reflects the response envelope shape (`{results, total, offset, limit}` for paginated tools; `{hierarchy, truncated, max_nodes, total_nodes_seen}` for class hierarchy). No stale references to flat-array responses remain. **Final readability pass on tool-catalog descriptions:** read the `#[tool(description=...)]` text for all five changed tools (`get_orphans`, `get_file_symbols`, `get_callers`, `get_callees`, `get_class_hierarchy`) end-to-end and confirm each documents (a) every new arg with its default and ceiling, (b) the response envelope shape, and (c) when an agent should pick non-default values. The descriptions were edited in three different phases (2.1, 3.1, 4.3) — this is the consolidation pass that catches inconsistent wording."
  - id: "4.10"
    title: "Final cargo insta review pass — all pending approved"
    status: planned
    depends_on: ["4.9"]
    verification: "`cargo insta pending-snapshots` reports zero pending across the whole workspace. Every snapshot touched in Phases 2/3/4 is in the approved state. The full snapshot delta against `main` is reviewable in one git diff: 10 existing response snapshots regenerated (1 orphans + 3 file_symbols + 1 callers + 1 callees + 4 hierarchy), 7 new response snapshots added (3 orphans + 3 list-shaped + 1 hierarchy-truncated), 5 tools-list snapshots regenerated (orphans + file_symbols + callers + callees + class_hierarchy). **Equally important: the 10 unchanged tools-list snapshots (`analyze_codebase`, `detect_cycles`, `generate_diagram`, `get_dependencies`, `get_coupling`, `get_symbol_detail`, `get_symbol_summary`, `search_symbols`, `watch_start`, `watch_stop`) MUST show zero diff against `main`.** Confirm via `git diff --stat crates/codegraph-tools/tests/snapshots/` — only the 22 expected files change. Any unintended snapshot churn is a sign of an accidental cross-tool effect and must be investigated before approval."
  - id: "4.11"
    title: "Structural verification + UE-scale manual smoke check"
    status: planned
    depends_on: ["4.10"]
    verification: "`cargo fmt --all --check` clean. `cargo clippy --workspace --all-targets -- -D warnings` clean. `cargo test --workspace` passes (683+ tests, no regressions). `cargo insta pending-snapshots` zero. UE-scale manual smoke (optional but recommended): index a public Unreal-style codebase, exercise `get_orphans { kind: function }` (confirm one-page response under MCP token ceiling, `total` reflects full count); `get_class_hierarchy { class: UObject, depth: 2, max_nodes: 250 }` (confirm `truncated: true` if the hierarchy exceeds 250, well-formed partial tree); `get_callers { symbol: <hot UE symbol>, depth: 1 }` (confirm pagination engages on high fan-in)."
---

# Phase 4: Tree-shaped get_class_hierarchy + cutover

## Overview

The last tool to retrofit, plus all the cross-cutting cutover work. `get_class_hierarchy` is structurally different from the other paginated tools — its result is a tree, not a list, so it gets `max_nodes` (a budget) rather than `limit`/`offset` (a window). The diamond-inheritance semantics matter: UE's `UObject` hierarchy contains many shared ancestors, and counting visits instead of unique names would silently truncate diamonds far earlier than intended.

After this phase the tool surface is fully migrated and documented. The PR is ready for review.

## 4.1: Modify Graph::class_hierarchy to accept max_nodes

### Subtasks
- [ ] Locate the entry point — likely `Graph::class_hierarchy` in `crates/codegraph-graph/src/queries.rs` calling into `algorithms.rs::build_hierarchy`
- [ ] Thread a `max_nodes: u32` argument through the public API and the recursive helpers
- [ ] Add a `&mut HashSet<String>` (or equivalent) to track unique names visited globally — separate from the per-path `on_path` set
- [ ] Add the unique-name to the set *before* recursing; check `set.len() >= max_nodes` *before* recursing into each child
- [ ] When the budget is exhausted, return early from the recursive call (do not add the child to `bases` / `derived`)
- [ ] Track a `truncated: bool` flag — set whenever an early-return happens
- [ ] Return `(HierarchyNode, u32 /* total_nodes_seen = visited.len() as u32 */, bool /* truncated */)` from the public function

### Notes
The existing `on_path` cycle guard at `algorithms.rs:111-130` (or wherever it lives) must be preserved — it's load-bearing for diamond hierarchies. The new global visited set is *additional*, not a replacement. The `class_hierarchy_diamond_4_level_fixture` test must continue to pass with no semantic change when `max_nodes` is generous.

The Graph layer's existing call site (the only caller is the handler) must be updated in lockstep with this signature change.

## 4.2: Graph-layer unit tests for max_nodes + diamond

### Subtasks
- [ ] Add `class_hierarchy_max_nodes_truncates` test
- [ ] Add `class_hierarchy_diamond_counts_unique_names` test (extends the existing diamond fixture)
- [ ] Add a test confirming `max_nodes=u32::MAX` produces the same tree as today (regression guard for the algorithm change)

### Notes
The diamond test is the highest-value test in this phase — it locks in the unique-name semantics that distinguishes this implementation from a naïve visit counter.

## 4.3: Extend GetClassHierarchyArgs

### Subtasks
- [ ] Add `max_nodes: Option<u32>` with `#[serde(default)]` and a schemars description
- [ ] Update the `#[tool(description=...)]` text to document `max_nodes` (default 250, max 1000), `truncated`, `total_nodes_seen`

## 4.4: Rewrite get_class_hierarchy handler

### Subtasks
- [ ] Define a private `ClassHierarchyResponse { hierarchy: HierarchyNode, truncated: bool, max_nodes: u32, total_nodes_seen: u32 }` in `handlers/structure.rs`, derive `Serialize`
- [ ] Resolve defaults: `max_nodes = max_nodes.filter(|&n| n != 0).unwrap_or(250).min(1000)`
- [ ] Call `Graph::class_hierarchy(class, depth, max_nodes)`
- [ ] Pack the result into `ClassHierarchyResponse` and `tool_success_json` it

### Notes
Defining the response struct privately (not as part of the shared `Page<T>` family) is intentional — tree-shaped envelopes are too different from list-shaped envelopes to share a generic without contortion. The struct lives next to the handler that owns it.

## 4.5: Wire through ServerInner

### Subtasks
- [ ] Forward `args.max_nodes` to the handler

## 4.6: Update existing class_hierarchy snapshots (×4)

### Subtasks
- [ ] `cargo test -p codegraph-tools` to regenerate
- [ ] `cargo insta review` for each of the four
- [ ] Confirm `truncated: false` for all four fixtures (none of them approach 250 nodes)
- [ ] Confirm `total_nodes_seen` equals the visible unique-name count of each tree
- [ ] Approve

## 4.7: Add truncated-case snapshot

### Subtasks
- [ ] Either build a >250-class hierarchy fixture, or pick an existing fixture and pass `max_nodes=5` to force truncation
- [ ] Snapshot the response — assert `truncated: true`, `total_nodes_seen` equals the cap, partial tree is well-formed
- [ ] Approve

### Notes
The cheapest path is reusing an existing fixture with a small `max_nodes` value — no new fixture data needed, and the response is small enough to keep the snapshot file readable.

## 4.8: Update class_hierarchy tools-list snapshot

### Subtasks
- [ ] Regenerate, review, approve

## 4.9: Update CLAUDE.md and README.md

### Subtasks
- [ ] Update `CLAUDE.md` "MCP Tools" section to reflect: `get_orphans` now has `limit`/`offset`/`brief`; `get_file_symbols` now has `limit`/`offset`; `get_callers`/`get_callees` now have `limit`/`offset`; `get_class_hierarchy` now has `max_nodes` and a wrapped response
- [ ] Update top-level `README.md` if it documents tool args (check by grepping for tool names)
- [ ] Confirm no stale references to flat-array responses

### Notes
This is documentation hygiene — the contract has changed and the docs must match. CLAUDE.md is the user-visible contract for agents loaded with this codebase.

## 4.10: Final cargo insta review pass

### Subtasks
- [ ] `cargo test --workspace` (regenerates anything pending)
- [ ] `cargo insta pending-snapshots` — must report zero
- [ ] If non-zero, run `cargo insta review` and approve / reject case by case
- [ ] `git status` shows the snapshot delta cleanly: ~22 snapshot files changed, no orphan `.snap.new` files

## 4.11: Structural verification + UE-scale smoke

### Subtasks
- [ ] `cargo fmt --all --check` clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` passes — full count matches or exceeds the baseline (683+ tests)
- [ ] (Optional) Manual UE-style smoke test against a real C++ codebase — run `code-graph-mcp` via stdio, send `analyze_codebase`, then `get_orphans`, `get_class_hierarchy { class: UObject, depth: 2 }`, `get_callers { symbol: <high-fanin> }` — confirm responses are well-formed envelopes within MCP token ceiling

### Notes
The UE smoke test is the closing-the-loop check on the originally observed failure. If it works, the P0 bug is closed.

## Acceptance Criteria

- [ ] `Graph::class_hierarchy` accepts `max_nodes`; tracks unique names; returns `(root, total_nodes_seen, truncated)`
- [ ] Diamond fixture passes with no semantic change; new diamond+max_nodes test confirms unique-name counting
- [ ] `GetClassHierarchyArgs` has `max_nodes`; handler wraps response in `{hierarchy, truncated, max_nodes, total_nodes_seen}`
- [ ] Four existing class_hierarchy snapshots regenerated; one new truncated snapshot added; tools-list snapshot regenerated
- [ ] CLAUDE.md and README.md updated to reflect the new tool surface
- [ ] Workspace-wide: zero pending snapshots, clippy-clean, fmt-clean, all tests passing
- [ ] (Optional) Manual UE-scale smoke test confirms the original `get_orphans` token-limit failure is resolved
