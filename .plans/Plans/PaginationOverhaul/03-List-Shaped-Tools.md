---
title: "List-shaped tools: get_file_symbols, get_callers, get_callees"
type: phase
plan: PaginationOverhaul
phase: 3
status: complete
created: 2026-05-07
updated: 2026-05-07
deliverable: "Three more tools return `Page<T>` envelopes. `get_file_symbols` paginates over symbols (limit=100). `get_callers` and `get_callees` paginate over `CallChain` rows sorted `(depth, symbol_id)` ascending (limit=100). Existing empty-file error wording on `get_file_symbols` is preserved exactly."
tasks:
  - id: "3.1"
    title: "Extend GetFileSymbolsArgs / GetCallersArgs / GetCalleesArgs with limit + offset"
    status: complete
    verification: "`crates/codegraph-tools/src/server.rs`: `GetFileSymbolsArgs` gains `limit: Option<u32>` and `offset: Option<u32>`. `GetCallersArgs` and `GetCalleesArgs` each gain the same pair (kept as separate structs for `JsonSchema` description independence — explicit decision per design doc). All new fields have `#[serde(default)]`. The `#[tool(description=...)]` text on each tool's `ServerInner` method is updated to document the new args, the default limit (100), the ceiling (1000), and the response envelope shape. `cargo build -p codegraph-tools` succeeds."
  - id: "3.2"
    title: "Rewrite get_file_symbols to return Page<SymbolResult>"
    status: complete
    depends_on: ["3.1"]
    verification: "`handlers::symbols::get_file_symbols` accepts `limit` and `offset` parameters. Order of operations: empty-`file` arg check (existing); read raw `file_symbols` from graph; if **raw** set is empty return the existing `\"no symbols found in file: <file>\"` tool error (wording unchanged); apply `top_level_only` filter; record `total = filtered.len() as u32`; stable sort by `symbol_id` ascending; slice; map to `SymbolResult` with the existing `brief`; wrap in `Page<SymbolResult>`. Defaults: limit=100, offset=0. Clamp limit at 1000. The error-vs-empty-envelope distinction is preserved: empty raw set → error; empty post-filter or post-slice → envelope with `results: []` and the appropriate `total`."
  - id: "3.3"
    title: "Rewrite get_callers and get_callees to return Page<CallChain>"
    status: complete
    depends_on: ["3.1"]
    verification: "`handlers::query::get_callers` and `get_callees` accept `limit` and `offset`. After the BFS completes, sort the `Vec<CallChain>` by `(depth, symbol_id)` ascending — depth first so page 1 holds the closest callers, then symbol_id as the stable tiebreaker. `total` is the pre-pagination length. Slice, wrap in `Page<CallChain>`, return. Defaults: limit=100, offset=0. Clamp at 1000. The existing `depth` parameter (controls BFS scope) is unchanged."
  - id: "3.4"
    title: "Wire new args through ServerInner methods"
    status: complete
    depends_on: ["3.2", "3.3"]
    verification: "`ServerInner::get_file_symbols`, `get_callers`, `get_callees` each forward `args.limit` and `args.offset` to their handler. No default-resolution in `ServerInner`; defaults live in the handler (matches `search_symbols` and `get_orphans` conventions)."
  - id: "3.5"
    title: "Update existing snapshots: file_symbols (×3), callers, callees"
    status: complete
    depends_on: ["3.4"]
    verification: "Five existing response snapshots regenerate to envelope shape and are reviewed via `cargo insta review`: `response_get_file_symbols_engine_cpp.snap`, `response_get_file_symbols_go_reader.snap`, `response_get_file_symbols_python_models.snap`, `response_get_callers_engine_update.snap`, `response_get_callees_engine_update.snap`. Confirm the `results` field of each contains the same entries as the prior flat array (file_symbols snapshots) or the prior BFS-order list re-sorted by `(depth, symbol_id)` (callers/callees — order WILL change). Approve."
  - id: "3.6"
    title: "Add new snapshots: paginated-offset variants"
    status: complete
    depends_on: ["3.5"]
    verification: "Three new snapshot tests added: `response_get_file_symbols_paginated_offset` (fixture with >100 symbols, request `offset=100 limit=50`); `response_get_callers_paginated_offset` (symbol with high fan-in, request `offset=50 limit=50`); `response_get_callees_paginated_offset` (similar, callees side). All approved via `cargo insta review`."
  - id: "3.7"
    title: "Unit-test pagination invariants for all three tools"
    status: complete
    depends_on: ["3.6"]
    verification: "For each of `get_file_symbols`, `get_callers`, `get_callees`: tests for default limit, page-1 + page-2 union, `total` invariance, clamping at 1000, zero-limit handling, **offset-beyond-total returns empty `results` with the correct full `total`** (each tool gets its own variant of this test — do not assume one tool's test covers the others). Plus tool-specific tests: `get_file_symbols` empty-raw-set returns the existing tool error (NOT an envelope) — diagnostic wording `\"no symbols found in file: <file>\"` is unchanged; `get_file_symbols` empty-post-filter (`top_level_only=true` on a file containing only methods) returns envelope with `results: []` and `total: 0`; `get_callers`/`get_callees` page ordering is `(depth, symbol_id)` ascending — build a fixture with at least two distinct depths plus same-depth ties and verify the resulting page is ordered first by depth then by symbol_id."
  - id: "3.8"
    title: "Update tools-list snapshots for the three tools"
    status: complete
    depends_on: ["3.7"]
    verification: "`snapshot_tools_list/*get_file_symbols*.snap`, `*get_callers*.snap`, `*get_callees*.snap` regenerate with the new args in `inputSchema` and are approved. Descriptions read sensibly (verify wording from 3.1 surfaces correctly)."
  - id: "3.9"
    title: "Structural verification"
    status: complete
    depends_on: ["3.8"]
    verification: "`cargo fmt --all --check` clean. `cargo clippy --workspace --all-targets -- -D warnings` clean. `cargo test --workspace` passes. `cargo insta pending-snapshots` reports zero."
tags: [pagination, mcp, llm-optimization, scale, ue, unreal-engine]
---

# Phase 3: List-shaped tools

## Overview

Three more tools, one shared shape. After Phase 2 the pagination pattern is established; this phase replicates it three times with one nuance per tool: `get_file_symbols` preserves the existing empty-file error path (raw-set-empty → error, not envelope), and `get_callers`/`get_callees` introduce an explicit `(depth, symbol_id)` sort because the underlying BFS visit order is non-deterministic across runs (HashMap adjacency iteration).

## 3.1: Extend args structs

### Subtasks
- [x] Add `limit: Option<u32>` and `offset: Option<u32>` to `GetFileSymbolsArgs`
- [x] Add same pair to `GetCallersArgs`
- [x] Add same pair to `GetCalleesArgs`
- [x] Update each tool's `#[tool(description=...)]` macro text
- [x] Confirm `JsonSchema` derive still works for all three structs

### Notes
Per the design's explicit decision, `GetCallersArgs` and `GetCalleesArgs` stay separate — their `#[schemars(description=...)]` strings differ ("callers of X" vs. "callees of X") and that wording surfaces in the MCP tool catalog.

## 3.2: Rewrite get_file_symbols

### Subtasks
- [x] Add `limit` and `offset` parameters to `handlers::symbols::get_file_symbols`
- [x] Preserve the empty-`file` arg check at the top (existing tool error)
- [x] Read raw `file_symbols`; if empty, return existing `"no symbols found in file: <file>"` tool error — DO NOT wrap this in an envelope
- [x] Apply the `top_level_only` filter, building `Vec<SymbolResult>` exactly as today
- [x] After filtering, record `total = results.len() as u32`
- [x] Stable sort by `symbol_id` ascending
- [x] Slice with `iter().skip(offset).take(limit)`
- [x] Wrap in `Page<SymbolResult>` and `tool_success_json` it

### Notes
The error-vs-envelope distinction is load-bearing for diagnostic UX. A misspelled file path keeps producing the existing error; an over-paginated request gets the empty envelope. Both behaviors are tested in 3.7.

## 3.3: Rewrite get_callers and get_callees

### Subtasks
- [x] Add `limit` and `offset` parameters to `handlers::query::get_callers` and `get_callees`
- [x] Run the existing BFS via `Graph::callers` / `Graph::callees`
- [x] Sort the resulting `Vec<CallChain>` by `(depth, symbol_id)` ascending — explicit, do not rely on BFS visit order
- [x] Record `total = vec.len() as u32`
- [x] Slice with `iter().skip(offset).take(limit)`
- [x] Wrap in `Page<CallChain>` and `tool_success_json` it

### Notes
The symbol-not-found error path stays as today — these tools currently error if the symbol isn't in the graph; that error is unchanged.

## 3.4: Wire new args

### Subtasks
- [x] Update `ServerInner::get_file_symbols`, `get_callers`, `get_callees` to forward the new args
- [x] Confirm no default-resolution leaks into the server layer

## 3.5: Update existing snapshots

### Subtasks
- [x] `cargo test -p codegraph-tools` to regenerate
- [x] `cargo insta review` — examine each of the five snapshots:
  - file_symbols × 3: only the wrapper changes; `results` content matches prior flat array
  - callers/callees × 2: wrapper changes AND row order may change due to the new sort key
- [x] For callers/callees, manually verify the new order is `(depth, symbol_id)` ascending — not BFS order
- [x] Approve all five

### Notes
The callers/callees snapshot review is the highest-risk moment in the phase. If row order changes are unexpectedly large, double-check the sort key — `depth` is `u32`, `symbol_id` is `String`, both have sensible `Ord` impls. Tuple sort `(depth, symbol_id)` is the intent.

## 3.6: Add new snapshots

### Subtasks
- [x] Build a fixture with >100 symbols in a single file (or extend `engine.cpp`); write `response_get_file_symbols_paginated_offset` test with `offset=100 limit=50`
- [x] Build (or find) a fixture with a high fan-in/fan-out symbol; write `response_get_callers_paginated_offset` and `response_get_callees_paginated_offset` tests
- [x] Approve all three

## 3.7: Unit-test pagination invariants

### Subtasks
- [x] For each tool: defaults, page union, total invariance, clamping, zero-limit, out-of-range offset
- [x] `get_file_symbols`-specific: `empty_raw_set_returns_error` (asserts `tool_error` content, NOT envelope); `empty_post_filter_returns_empty_envelope` (top_level_only=true on a file with only methods → envelope with `total: 0`)
- [x] `get_callers` / `get_callees`-specific: `pagination_orders_by_depth_then_symbol_id` (build fixture with multiple depths and verify order)

## 3.8: Update tools-list snapshots

### Subtasks
- [x] Regenerate, review, approve for all three tools
- [x] Verify the `inputSchema` descriptions match 3.1's text

## 3.9: Structural verification

### Subtasks
- [x] `cargo fmt --all --check` clean
- [x] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [x] `cargo test --workspace` passes
- [x] `cargo insta pending-snapshots` reports zero

## Acceptance Criteria

- [x] All three args structs gain `limit` + `offset` fields
- [x] `get_file_symbols` preserves the empty-raw-set tool error; introduces empty-post-filter envelope path
- [x] `get_callers` / `get_callees` sort by `(depth, symbol_id)` ascending — verified by a unit test
- [x] Five existing response snapshots regenerated and approved
- [x] Three new response snapshots added and approved
- [x] Tools-list snapshots regenerated for all three tools
- [x] Pagination unit tests pass for each tool, including tool-specific empty-set behavior
- [x] `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace` all clean
