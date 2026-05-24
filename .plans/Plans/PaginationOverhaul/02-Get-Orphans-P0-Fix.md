---
title: "get_orphans P0 fix — pagination + brief flag"
type: phase
plan: PaginationOverhaul
phase: 2
status: complete
created: 2026-05-07
updated: 2026-05-07
deliverable: "`get_orphans` returns `Page<SymbolResult>` instead of a flat array, with `limit` (default 20) / `offset` (default 0) / `brief` (default true) args. The reported UE-scale token-limit failure is resolved: a 50k-orphan result returns a single 20-row page plus `total: 50000` instead of a 5 MB array."
tasks:
  - id: "2.1"
    title: "Extend GetOrphansArgs with limit, offset, brief"
    status: complete
    verification: "`crates/codegraph-tools/src/server.rs::GetOrphansArgs` gains three new fields: `limit: Option<u32>`, `offset: Option<u32>`, `brief: Option<bool>`, all with `#[serde(default)]`. The `#[tool]` macro description text is updated to document the new args. The args struct still derives `JsonSchema`; `cargo build -p codegraph-tools` succeeds."
  - id: "2.2"
    title: "Rewrite get_orphans handler to return Page<SymbolResult>"
    status: complete
    depends_on: ["2.1"]
    verification: "`crates/codegraph-tools/src/handlers/structure.rs::get_orphans` accepts `limit`, `offset`, `brief` parameters. Logic: collect full match set from `Graph::orphans(kind)` → record `total = matches.len() as u32` → stable sort by `symbol_id` ascending → slice `[offset..offset+limit]` → map to `SymbolResult` with the resolved `brief` flag → wrap in `Page<SymbolResult>`. Limit normalization: `None` or `Some(0)` → 20; `Some(n)` clamps to 1000. Offset normalization: `None` → 0; otherwise echoed. The hardcoded `brief=true` call is replaced by the resolved flag."
  - id: "2.3"
    title: "Wire new args through ServerInner::get_orphans"
    status: complete
    depends_on: ["2.2"]
    verification: "`server.rs::ServerInner::get_orphans` extracts `args.limit`, `args.offset`, `args.brief` and forwards them to `handlers::structure::get_orphans`. The default-resolution and clamping happens in the handler, not in `ServerInner` (matches the existing convention used by `search_symbols`)."
  - id: "2.4"
    title: "Update existing get_orphans snapshot"
    status: complete
    depends_on: ["2.3"]
    verification: "`snapshot_responses__response_get_orphans_default_callables.snap` regenerates to the envelope shape (fields per `Page<T>`'s declaration order, mirroring `search_symbols`). Reviewed via `cargo insta review`. The new `results` array must contain the same set of entries as the prior flat array — no entries added or removed. **Sort order may differ:** the prior handler returned orphans in `Graph::orphans` iteration order; the new handler sorts by `symbol_id` ascending. An order-only change in `results` is expected and correct. An entry-set change (missing or new symbols) is a bug — do not approve such a snapshot."
  - id: "2.5"
    title: "Add new snapshots: page-2, brief=false, offset-beyond-total"
    status: complete
    depends_on: ["2.4"]
    verification: "Three new tests in `crates/codegraph-tools/tests/snapshot_responses.rs` produce three new snapshots: (a) `response_get_orphans_paginated_offset` — fixture with >20 orphans, request `offset=20 limit=20`, asserts page-2 contents are the next sorted slice; (b) `response_get_orphans_brief_false` — same fixture, `brief=false`, asserts `signature`/`column`/`end_line` fields appear in results; (c) `response_get_orphans_offset_beyond_total` — small fixture, `offset=999`, asserts `results: []` with `total: <full count>`. All three approved via `cargo insta review`."
  - id: "2.6"
    title: "Unit-test pagination invariants"
    status: complete
    depends_on: ["2.5"]
    verification: "Unit tests in `handlers/structure.rs` cover: (a) defaults — no args returns 20 rows starting at offset 0; (b) page 1 + page 2 union equals the full sorted set with no overlap on a 30-orphan fixture; (c) `total` is the pre-pagination count regardless of page; (d) `limit=999999` clamps to 1000 and the response echoes `limit: 1000`; (e) `limit=0` returns the default 20; (f) `offset >= total` returns empty `results` and the correct `total`; (g) `kind` filter still works combined with pagination."
  - id: "2.7"
    title: "Update get_orphans tools-list snapshot"
    status: complete
    depends_on: ["2.6"]
    verification: "`snapshot_tools_list/snapshot_tools_list__tools_list_get_orphans.snap` (or equivalent path under `crates/codegraph-tools/tests/snapshots/`) regenerates to include `limit`, `offset`, `brief` in `inputSchema`. Reviewed via `cargo insta review`. The descriptions in the schema must come from the `#[tool]` macro updates in 2.1 — confirm the wording."
  - id: "2.8"
    title: "Structural verification"
    status: complete
    depends_on: ["2.7"]
    verification: "`cargo fmt --all --check` clean. `cargo clippy --workspace --all-targets -- -D warnings` clean. `cargo test --workspace` passes. `cargo insta pending-snapshots` reports zero pending."
tags: [pagination, mcp, llm-optimization, scale, ue, unreal-engine]
---

# Phase 2: get_orphans P0 fix

## Overview

The reported failure. The Unreal-Engine dogfooding run hit MCP token limits on `get_orphans` because the handler returns every orphan as a flat array with no cap. This phase ships the pagination envelope on `get_orphans` plus the `brief` flag (the only symbol-list tool currently missing it). After this phase, the user's primary blocker is resolved.

Phase 1's `Page<T>` is consumed for the first time here. Subsequent phases follow the same pattern, so this phase also serves as the reference implementation for Phases 3 and 4's pagination work.

## 2.1: Extend GetOrphansArgs with limit, offset, brief

### Subtasks
- [x] Add `limit: Option<u32>` with `#[serde(default)]` and a `#[schemars(description = "...")]` annotation
- [x] Add `offset: Option<u32>` with same treatment
- [x] Add `brief: Option<bool>` with same treatment, defaulting to true in the handler
- [x] Update the `#[tool(description = "...")]` text on `ServerInner::get_orphans` to document `limit` (default 20, max 1000), `offset` (default 0), `brief` (default true) and the new envelope shape

### Notes
The `JsonSchema` derive must still work. Schemars metadata propagates into the MCP tool catalog so agents see the new args.

## 2.2: Rewrite get_orphans handler to return Page<SymbolResult>

### Subtasks
- [x] Add a `limit: Option<u32>`, `offset: Option<u32>`, `brief: Option<bool>` parameter list to `handlers::structure::get_orphans`
- [x] Resolve defaults: `limit = limit.filter(|&n| n != 0).unwrap_or(20).min(1000)`, `offset = offset.unwrap_or(0)`, `brief = brief.unwrap_or(true)`
- [x] Read `Graph::orphans(parsed_kind)` into a `Vec<&Symbol>` (full match set)
- [x] `let total = matches.len() as u32`
- [x] Stable sort by `symbol_id` ascending (use the existing helper if there is one, else `sort_by_key(|s| s.id().clone())` or equivalent)
- [x] Slice `[offset..offset+limit]` with bounds-safe slicing (use `iter().skip(offset).take(limit)` to avoid panics on out-of-range offsets)
- [x] Map to `SymbolResult` with `symbol_to_result(s, brief)`
- [x] Wrap in `Page<SymbolResult> { results, total, offset, limit }` and `tool_success_json` it

### Notes
The existing handler at `structure.rs:56-68` is short — the rewrite is a near-total replacement of the body. The `kind` filter at the top is preserved unchanged.

## 2.3: Wire new args through ServerInner::get_orphans

### Subtasks
- [x] Update the `ServerInner::get_orphans` body to forward `args.limit`, `args.offset`, `args.brief` to the handler
- [x] Confirm no default-resolution happens in `ServerInner` — the handler owns it (matches `search_symbols`'s pattern at the existing call site)

### Notes
Trivial. Two-line change.

## 2.4: Update existing get_orphans snapshot

### Subtasks
- [x] Run `cargo test -p codegraph-tools --test snapshot_responses response_get_orphans_default_callables`
- [x] Run `cargo insta review` to inspect the regenerated `.snap.new` file
- [x] Confirm the same set of symbol entries appears (no additions, no removals); order may differ (new sort by `symbol_id` ascending vs. prior insertion order)
- [x] Approve the snapshot

### Notes
The fixture for this snapshot lives in `tests/snapshot_responses.rs`. The existing fixture has fewer than 20 orphans, so default-limit paging will return all of them on page 1. `total` will equal `results.len()`.

## 2.5: Add new snapshots: page-2, brief=false, offset-beyond-total

### Subtasks
- [x] Add `response_get_orphans_paginated_offset` test — fixture with at least 25 orphans (extend an existing fixture or build a new one), request `{offset: 20, limit: 20}`, snapshot the response
- [x] Add `response_get_orphans_brief_false` test — same or smaller fixture, request `{brief: false}`, snapshot
- [x] Add `response_get_orphans_offset_beyond_total` test — small fixture, request `{offset: 999}`, snapshot (assert `results: []`, `total > 0`)
- [x] Approve all three via `cargo insta review`

### Notes
The fixture-construction work is the bulk of this task — building a graph with >20 orphans deterministically may require a custom fixture rather than reusing an existing one. Look at how `search_symbols` snapshot tests build fixtures with known cardinality.

## 2.6: Unit-test pagination invariants

### Subtasks
- [x] Add `orphans_default_limit_is_20` test
- [x] Add `orphans_page_1_and_page_2_cover_full_set` test on a 30-orphan fixture
- [x] Add `orphans_total_is_full_match_count` test asserting `total` is consistent across pages
- [x] Add `orphans_limit_clamps_at_1000` test
- [x] Add `orphans_zero_limit_uses_default` test
- [x] Add `orphans_offset_beyond_total_returns_empty` test
- [x] Add `orphans_kind_filter_combined_with_pagination` test (e.g., kind=class with mixed orphan kinds)

### Notes
These are direct handler unit tests — call `get_orphans(...)` and assert on the returned `CallToolResult`. Parsing the JSON out of `CallToolResult::Content::Text` should follow the existing test helper pattern in the file.

## 2.7: Update get_orphans tools-list snapshot

### Subtasks
- [x] Run `cargo test -p codegraph-tools --test snapshot_tools_list`
- [x] Run `cargo insta review` for the `get_orphans` entry
- [x] Confirm the `inputSchema` JSON shows the three new properties with the descriptions from 2.1
- [x] Approve

### Notes
The tools-list snapshot is the agent-visible contract for what args the tool accepts. Verify the descriptions read well — agents see this text when deciding how to call the tool.

## 2.8: Structural verification

### Subtasks
- [x] `cargo fmt --all --check` clean
- [x] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [x] `cargo test --workspace` passes
- [x] `cargo insta pending-snapshots` reports zero
- [x] Quick manual check: `cargo run -p code-graph-mcp` against a small test repo, send a `get_orphans` request via MCP — confirm response shape is the envelope, not a flat array

### Notes
The manual MCP smoke test is optional but strongly recommended — wire-format bugs that pass the snapshot suite (e.g. content-type mismatches) will fail at the rmcp serialization layer in real usage.

## Acceptance Criteria

- [x] `GetOrphansArgs` has `limit`, `offset`, `brief` fields wired through to the handler
- [x] Handler returns `Page<SymbolResult>` with correct defaults (20/0/true), clamping at 1000, and stable sort by `symbol_id`
- [x] Existing `response_get_orphans_default_callables.snap` regenerated and approved (envelope shape; results unchanged byte-for-byte aside from wrapper)
- [x] Three new response snapshots added and approved (page-2, brief=false, offset-beyond-total)
- [x] Tools-list snapshot regenerated showing new args in `inputSchema`
- [x] Seven new unit tests in `handlers/structure.rs` cover defaults, page union, total invariance, clamping, zero-limit, out-of-range offset, kind+pagination interaction
- [x] `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace` all clean
