---
title: "count_only flag + SymbolResult.file drop"
type: phase
plan: PaginatedResponseSizeSafety
phase: 3
status: complete
created: 2026-05-11
updated: 2026-05-12
deliverable: "count_only: bool added to get_orphans / search_symbols / get_file_symbols; SymbolResult.file dropped universally; Graph::search short-circuits when count_only is set; agents recover file via the documented id_to_file contract."
tasks:
  - id: "3.1"
    title: "Add count_only field to the three Args structs"
    status: complete
    verification: "GetOrphansArgs, SearchSymbolsArgs, GetFileSymbolsArgs each gain count_only: Option<bool> with #[serde(default)] (Option's Default is None, matching the existing brief/force/force-style Args fields); JSON deserialization tests cover (a) field absent -> None (handler resolves None to false via unwrap_or(false) at call site), (b) field present and true -> Some(true), (c) field present and false -> Some(false), (d) malformed (string instead of bool) -> deserialization error; tools-list snapshots for these three tools regenerate"
  - id: "3.2"
    title: "Handler early-return path for count_only=true"
    status: complete
    verification: "Each of the three handlers, when count_only resolves to true, emits Page { results: vec![], total: <real count>, offset: 0, limit: 0, truncated: false, next_offset: None } before the byte-budget step is invoked; snapshot count_only_response_{orphans,search_symbols,file_symbols} asserts serialized response size < 1KB regardless of input scale; total reflects the true pre-pagination match count, not zero"
    depends_on: ["3.1"]
  - id: "3.3"
    title: "Thread count_only into SearchParams for search_symbols"
    status: complete
    verification: "SearchParams gains count_only: bool with default false; Graph::search early-returns with total computed but BinaryHeap<TopEntry> never constructed when count_only is true (verified by a unit test using a search_count_only_skips_heap predicate via a test-only counter on heap pushes); search_symbols handler passes the resolved count_only through to SearchParams; behavioral test confirms a count-only call returns the same total as a regular call with limit=1"
    depends_on: ["3.2"]
  - id: "3.4"
    title: "Drop file field from SymbolResult; update all consumers (typed + JSON-key)"
    status: complete
    verification: "SymbolResult no longer has a file field; symbol_to_result no longer populates it; all existing response snapshots regenerate without 'file' keys on records; one new unit test demonstrates round-trip: a sampled response record id is fed through code_graph_core::id_to_file and produces the absolute path the snapshot would previously have had in the file field; CallChain (used by get_callers/get_callees) is unchanged (its file is the call-site, not redundant); two NON-snapshot test consumers are explicitly migrated: (i) crates/code-graph-tools/tests/mixed_language.rs:203 (.expect(\"each result has a file field\") panics if not migrated — switch language inference to use id_to_file(&record.id)), (ii) crates/code-graph-tools/src/handlers/symbols.rs:341 (asserts entry.get(\"file\").is_some() — flip to is_none() or remove); workspace-wide rg --type rust '\"file\"' under crates/code-graph-tools/ has zero remaining matches on SymbolResult-shaped JSON"
    depends_on: ["3.2"]
---

# Phase 3: count_only flag + SymbolResult.file drop

## Overview

Two independent payload optimizations land together: `count_only` for stats-only callers, and `file` drop from `SymbolResult` for everyone else.

`count_only` requires changes in three layers: Args struct (MCP surface), handler (early-return), and `Graph::search` (`SearchParams` for the search-symbols case). The first two are uniform; the third is the `search_symbols`-specific path established as the architectural exception in Phase 2.5.

`file` drop is wire-format breaking but pre-1.0 acceptable per the Phase 2.5 design decision. The id-recovery contract delivered in Phase 1.4 is the migration path.

## 3.1: Add count_only field to the three Args structs

### Subtasks
- [ ] Edit `crates/code-graph-tools/src/server.rs` to add `count_only: Option<bool>` to `GetOrphansArgs`, `SearchSymbolsArgs`, `GetFileSymbolsArgs`
- [ ] Use `Option<bool>` (resolved to `false` at handler entry) rather than bare `bool` to match the existing `Option<u32>` / `Option<String>` style on `limit`/`offset` (verifies via grep of the existing Args structs)
- [ ] Deserialization tests in `server.rs` `#[cfg(test)] mod tests` if such a module exists, or in the handlers test module
- [ ] Regenerate the 3 tools-list snapshots for these tools
- [ ] Do NOT yet update the `#[tool(description=…)]` strings — that happens in Phase 4.1 alongside the byte-budget description rewrites for coherence

### Notes
`get_callers` and `get_callees` deliberately do NOT get `count_only`. Decision recorded in plan README as D9. Their depth/limit interaction makes "how many?" cheap already.

## 3.2: Handler early-return path for count_only=true

### Subtasks
- [ ] In each of the three handlers, after `args` resolution and the indexed-state guard, check `count_only.unwrap_or(false)`
- [ ] When true:
  - For `get_orphans` / `get_file_symbols`: compute `total` exactly as today (the cheap path — filter by kind/file, count the result, never materialize `SymbolResult`s)
  - For `search_symbols`: delegate to `Graph::search` with `SearchParams { count_only: true, .. }` — implemented in 3.3
  - Return `Page { results: vec![], total, offset: 0, limit: 0, truncated: false, next_offset: None }`
  - **`limit: 0` is a deliberate exception to the "envelope echoes resolved limit" contract.** Rationale: count_only callers explicitly opted out of paging; echoing the would-have-been-resolved limit suggests there's a record page to fetch, which is misleading. The exception is documented in Phase 4.2 (CLAUDE.md Response shapes) alongside the count_only sub-block.
- [ ] New snapshot per tool: `count_only_response_{orphans,search_symbols,file_symbols}`
- [ ] Smoke test that the count_only response size is bounded — `serde_json::to_string(&response).len() < 1024`

### Notes
The "skip the take" path for orphans / file_symbols is cheap and bypasses both `symbol_to_result` and `byte_budget_take`. Order: count_only check FIRST, byte-budget LAST.

## 3.3: Thread count_only into SearchParams for search_symbols

### Subtasks
- [ ] Locate `SearchParams` in `crates/code-graph-graph/` (probably `queries.rs` per Researcher's hint at `BinaryHeap<TopEntry>` algorithm)
- [ ] Add `count_only: bool` field with `#[serde(default)]` if serialized (likely not — it's a Rust-internal struct)
- [ ] In `Graph::search`: at top of function, if `count_only` is set, walk the matchers exactly as today to compute `total` but skip the `BinaryHeap<TopEntry>` push/pop loop and any allocation of result Vec; return `SearchResult { symbols: vec![], total }`
- [ ] Behavioral test: same query with `count_only=false` (limit=1) and `count_only=true` returns equal `total`
- [ ] Heap-not-touched test: introduce a `#[cfg(test)] static HEAP_PUSHES: AtomicUsize` or similar test-only counter; assert it stays at 0 during a count_only call

### Notes
The heap-not-touched test is optional but recommended — it pins the cost win to behavior, making future refactors that accidentally re-introduce heap construction visible immediately.

## 3.4: Drop file field from SymbolResult; update all consumers (typed + JSON-key)

### Subtasks
- [ ] Edit `crates/code-graph-tools/src/handlers/mod.rs` to remove `file: String` from `SymbolResult`
- [ ] Edit `symbol_to_result` to stop populating it
- [ ] Grep for `SymbolResult` typed usages workspace-wide via `rg --type rust 'SymbolResult'` and update each (none should read `.file` directly, but verify)
- [ ] **JSON-key consumer sweep (clippy does NOT catch these):** `rg --type rust '"file"' crates/code-graph-tools/ --include="*.rs"` to find serde_json::Value indexing or `.get("file")` calls
- [ ] **Explicit migration of two known non-snapshot consumers:**
  - `crates/code-graph-tools/tests/mixed_language.rs:203` — the `.expect("each result has a file field")` call panics when `file` is dropped. Migrate the language-inference path to use `code_graph_core::id_to_file(&record.id)` instead of reading `record.file`.
  - `crates/code-graph-tools/src/handlers/symbols.rs:341` — the test asserts `entry.get("file").is_some()` in `file_symbols_returns_full_list_in_brief_mode`. Flip to `entry.get("file").is_none()` (post-drop expectation) OR remove the assertion entirely if it's redundant with the regenerated snapshot
- [ ] Regenerate all `SymbolResult`-emitting response snapshots — they'll lose the `file` key on each record
- [ ] New unit test: pick the first record from a regenerated snapshot, call `id_to_file(&record.id)`, assert it equals the absolute path that snapshot previously had as `record.file`
- [ ] CONFIRM `CallChain` is untouched (used in `get_callers`/`get_callees`); the `file` field on `CallChain` is the call-site file, not redundant with `symbol_id`
- [ ] Final sweep: `cargo test --workspace` AND clippy AND cargo check all clean — failure modes for missed consumers are panic (test fails), assertion failure (test fails), or compile error (clippy/check fails), respectively

### Notes
This is the breaking wire-format change. Decision recorded in plan README as D10. The id-recovery contract from Phase 1.4 is the documented migration path.

The "round-trip from snapshot record's id to expected file" test is the contract test — if `id_to_file` ever diverges from how `symbol_id(s)` is constructed, this test will fail before any client notices.

**Critical:** Clippy and `cargo check` do NOT catch JSON-key consumers (`serde_json::Value` indexing or `.get("file")` on JSON values). The two named consumers above were identified by the plan reviewer; the `rg "file"` sweep is the safety net for any others.

## Acceptance Criteria
- [ ] `count_only: Option<bool>` accepted on the three Args structs with proper deserialization (`None` when absent, NOT `Some(false)`)
- [ ] Handler early-return emits the documented sentinel response shape (`limit: 0` is the deliberate exception); total is accurate; serialized response size is < 1KB
- [ ] `SearchParams.count_only` short-circuits `Graph::search` before heap construction
- [ ] `SymbolResult` no longer carries `file`; all 16 SymbolResult-emitting snapshots regenerate without that key (these are the same 16 already regenerated in Phase 2; Phase 3's regeneration supersedes Phase 2's)
- [ ] Both named non-snapshot consumers (`mixed_language.rs:203`, `symbols.rs:341`) migrated; `cargo test --workspace` clean
- [ ] id-to-file round-trip test passes on regenerated snapshots
- [ ] `CallChain.file` retained (validated by an unchanged-file integration test)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
- [ ] `make snapshot-clean` passes after `cargo insta review`
