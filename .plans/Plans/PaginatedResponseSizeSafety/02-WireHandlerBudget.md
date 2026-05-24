---
title: "Wire byte budget into the 5 paginated handlers"
type: phase
plan: PaginatedResponseSizeSafety
phase: 2
status: complete
created: 2026-05-11
updated: 2026-05-12
deliverable: "All 5 paginated MCP tools route their page construction through the byte budget. Oversized pages return truncated=true with a usable next_offset. search_symbols handler trims its Graph::search result mid-page."
tasks:
  - id: "2.0"
    title: "Thread max_bytes from server layer into the 5 handler signatures"
    status: complete
    verification: "Each of the 5 handler functions in crates/code-graph-tools/src/handlers/ gains a max_bytes: usize parameter (added as the last positional parameter to keep argument ordering stable); the server.rs tool method for each tool reads self.inner.config().response.max_bytes (or equivalent accessor wired in Phase 1.2) and passes it through; cargo check passes after the signature change; no caller of these handlers exists outside server.rs (verified via grep) so the change is fully contained"
  - id: "2.1"
    title: "get_orphans: wire byte_budget_take after sort"
    status: complete
    verification: "Existing snapshots (get_orphans_default_callables, brief_false, paginated_offset, offset_beyond_total) regenerate cleanly via cargo insta review with truncated=false and next_offset=null for in-budget pages; new snapshot get_orphans_byte_budget_truncated covers a synthetic oversized scenario and asserts truncated=true, next_offset=Some(n>offset), results.len()<limit, total>=results.len()+offset"
    depends_on: ["1.3", "2.0"]
  - id: "2.2"
    title: "get_file_symbols: wire byte_budget_take after sort"
    status: complete
    verification: "Existing snapshots (engine_cpp, go_reader, python_models, paginated_offset) regenerate cleanly; empty-raw-set returns the documented error envelope (not a Page<T>) per PaginationOverhaul Phase 3 decision — confirmed by an unchanged existing error-path test; new snapshot get_file_symbols_byte_budget_truncated demonstrates truncation on a synthetic large-file fixture"
    depends_on: ["1.3", "2.0"]
  - id: "2.3"
    title: "get_callers: wire byte_budget_take after (depth, symbol_id) sort"
    status: complete
    verification: "Existing snapshots (engine_update, paginated_offset) regenerate; (depth, symbol_id) sort order preserved across truncation (verified by a unit test that builds an oversized chain set and asserts every kept record's depth <= next-page's first depth); new snapshot get_callers_byte_budget_truncated demonstrates truncated=true with depth-ordered partial page"
    depends_on: ["1.3", "2.0"]
  - id: "2.4"
    title: "get_callees: wire byte_budget_take after (depth, symbol_id) sort"
    status: complete
    verification: "Existing snapshots (engine_update, paginated_offset) regenerate; sort-order-across-truncation unit test mirrors get_callers; new snapshot get_callees_byte_budget_truncated parallels 2.3"
    depends_on: ["1.3", "2.0"]
  - id: "2.5"
    title: "search_symbols: handler-layer trim of Graph::search result"
    status: complete
    verification: "Existing snapshots (helper_language_go, helper_language_rust, helper_language_python, query_engine) regenerate; the trim path is exercised by a new snapshot search_symbols_byte_budget_truncated that calls with limit=1000 against a synthetic broad-match fixture and demonstrates truncated=true with next_offset=Some(n) where n < limit; total stays as the Graph::search-reported pre-pagination match count; unit test asserts a recursive get_symbol_detail(records[last].id) still resolves (records aren't corrupted by trim); re-paging correctness test: a second call to search_symbols with offset=next_offset returns records whose first entry equals the (k+1)-th record from the first call (no overlap or gap at the trim boundary)"
    depends_on: ["1.3", "2.0"]
tags: [pagination, mcp, llm-optimization, byte-budget, regression-fix]
---

# Phase 2: Wire byte budget into the 5 paginated handlers

## Overview

Each of the 5 paginated handlers gains byte-budget awareness. The 4 materializing tools (orphans, file_symbols, callers, callees) follow a uniform pattern: keep the existing sort, replace `.skip(offset).take(limit).collect()` with a call to `byte_budget_take`. `search_symbols` is the exception — it receives an already-sliced page from `Graph::search`, so its trim path is handler-side post-processing on `sr.symbols`.

`max_bytes` is read from `ServerInner.config().response.max_bytes` (added in Phase 1.2) and threaded into each handler as a function parameter; there is no per-call override at the MCP tool surface.

## 2.0: Thread max_bytes from server layer into the 5 handler signatures

### Subtasks
- [ ] Edit each of the 5 handler functions in `crates/code-graph-tools/src/handlers/` (structure.rs, symbols.rs, query.rs) to add `max_bytes: usize` as the last positional parameter
- [ ] In `crates/code-graph-tools/src/server.rs`, locate each tool method that calls these handlers (5 of them — `get_orphans`, `get_file_symbols`, `search_symbols`, `get_callers`, `get_callees`)
- [ ] In each tool method: read `self.inner.config().response.max_bytes` (or whichever accessor was wired in Phase 1.2) and pass it as the new parameter to the handler call
- [ ] `cargo check` clean after the signature change; `cargo test --workspace` should NOT yet exercise the new parameter (handlers don't use it yet — that's tasks 2.1–2.5)
- [ ] Verify via grep that the 5 handler functions have no other callers outside `server.rs`

### Notes
This is mechanical plumbing but easy to miss when scanning the per-handler subtasks. Splitting it out makes the diff for tasks 2.1–2.5 stay focused on byte-budget logic, not signature changes. Performing this task first also enables incremental compilation through 2.1–2.5 — each per-handler change is type-correct against the new signature.

## 2.1: get_orphans: wire byte_budget_take after sort

### Subtasks
- [ ] Edit `crates/code-graph-tools/src/handlers/structure.rs` near the existing `matches.sort_by_key(symbol_id)` and the `let results = matches.iter().skip(offset).take(limit)...` block
- [ ] Replace skip+take+collect+symbol_to_result chain with `byte_budget_take`; preserve the existing brief/full toggle by collecting `SymbolResult` values from `symbol_to_result(s, resolved_brief)` before feeding into the helper
- [ ] Pull `max_bytes` from server config
- [ ] Wire `truncated` and `next_offset` into the constructed `Page<SymbolResult>`
- [ ] Add a synthetic-fixture test that constructs >budget worth of orphan records and asserts the documented truncation semantics

### Notes
The sort happens before the helper; the helper preserves iteration order. Truncation never reorders kept records. The `kind` filter (already supported) applies upstream of the sort — unchanged.

## 2.2: get_file_symbols: wire byte_budget_take after sort

### Subtasks
- [ ] Edit `crates/code-graph-tools/src/handlers/symbols.rs` near the existing `results.sort_by(|a, b| a.id.cmp(&b.id))` and the `let page = results.into_iter().skip(offset).take(limit).collect()` block
- [ ] Replace with `byte_budget_take`
- [ ] CRITICAL: the empty-raw-set case (no symbols match the file at all) returns an error envelope per Phase 3 of `PaginationOverhaul`. That branch executes BEFORE the byte-budget step and must stay intact — confirmed by an existing error-path test that this phase does NOT touch
- [ ] Add a synthetic-fixture test that asserts truncation on a deliberately bloated file (UE-generated-headers style: many symbols)

### Notes
`top_level_only` is the upstream filter; byte_budget_take operates after.

## 2.3: get_callers: wire byte_budget_take after (depth, symbol_id) sort

### Subtasks
- [ ] Edit `crates/code-graph-tools/src/handlers/query.rs` near the existing `chains.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.symbol_id.cmp(&b.symbol_id)))` and the skip+take block
- [ ] Replace skip+take with `byte_budget_take`
- [ ] Sort determinism: byte-budget truncation must not interfere with the (depth, symbol_id) ordering. The helper preserves order, so kept records are a prefix of the sorted chain. Add a focused unit test that builds an oversized chain set with mixed depths and asserts: max(kept depth) <= min(dropped depth) where strictly less, OR within-depth tie-broken by symbol_id
- [ ] Add `get_callers_byte_budget_truncated` snapshot

### Notes
`CallChain.file` is the CALL SITE file, not the definition file (Researcher caught this). It is NOT a candidate for the Phase 3 slimming. This phase makes no per-record shape change.

## 2.4: get_callees: wire byte_budget_take after (depth, symbol_id) sort

### Subtasks
- [ ] Mirror 2.3 for the callees handler (same file, parallel function)
- [ ] Same sort-determinism unit test mirrored for callees
- [ ] Add `get_callees_byte_budget_truncated` snapshot

### Notes
Parallels 2.3.

## 2.5: search_symbols: handler-layer trim of Graph::search result

### Subtasks
- [ ] Edit `crates/code-graph-tools/src/handlers/symbols.rs` near the existing `let response = Page::<SymbolResult> { results, total: sr.total, offset, limit }` block
- [ ] After `sr.symbols` is mapped to `Vec<SymbolResult>` (via `symbol_to_result`), iterate it serializing each record, summing bytes; truncate at the first record that would push past `max_bytes`
- [ ] If truncated at index k (zero-based), set `truncated=true`, `next_offset = Some(resolved_offset + k)`; if the full page fits, set `truncated=false`, `next_offset=None`
- [ ] `total` stays as `sr.total` (pre-pagination match count from `Graph::search`)
- [ ] Add `search_symbols_byte_budget_truncated` snapshot
- [ ] Add a unit test that takes the last kept record's `id` and asserts `get_symbol_detail` (or `id_to_file(&id)`) still resolves — verifying records aren't corrupted mid-serialization
- [ ] Add a re-paging correctness test: call with `offset=0` and a broad match; assume `truncated=true` with `next_offset=Some(k)`; re-call with `offset=k`, assert the first returned record equals what would have been the (k+1)-th record from the first call (Graph::search's deterministic symbol_id ordering guarantees this)

### Notes
This is the documented architectural exception. The 4 other handlers can use `byte_budget_take` directly because they iterate over their pre-paginated full match set. `search_symbols` receives only `sr.symbols.len() <= limit` records back from `Graph::search`, so the helper's `offset` and `limit` parameters don't map cleanly. A bespoke trim loop is simpler than retrofitting the helper.

Future work (out of scope): if `Graph::search`'s materialization ever shows up as a hot path, threading `max_bytes` into `SearchParams` becomes attractive. Phase 3 already threads `count_only` there for a different reason — same code path is the place to add `max_bytes` if needed.

## Acceptance Criteria
- [ ] Task 2.0 `max_bytes` plumbing complete; `cargo check` clean
- [ ] All 5 handlers route their final page through byte-budget logic
- [ ] All 16 existing response snapshots regenerate cleanly via `cargo insta review` (NOTE: these same 16 will regenerate AGAIN in Phase 3 when `SymbolResult.file` is dropped — Phase 3's regeneration supersedes this one)
- [ ] All 5 new `*_byte_budget_truncated` snapshots ship with documented expectations
- [ ] Sort-order-preservation unit tests pass for callers + callees
- [ ] `search_symbols` records-not-corrupted-mid-trim test passes
- [ ] `search_symbols` re-paging correctness test passes
- [ ] `get_file_symbols` empty-raw-set error path is unchanged (existing test passes without modification)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
- [ ] `make snapshot-clean` passes
