---
title: "get_symbol_summary pagination + <global> rename"
type: phase
plan: ResponseShapePolish
phase: 1
status: complete
created: 2026-05-13
updated: 2026-05-14
deliverable: "`get_symbol_summary` returns `Page<SummaryRow>` (flat row form, not nested map). Rows sort by `(namespace, kind)` asc. Empty-namespace symbols render as `<global>` in this tool's output only — `search_symbols` filter still uses the empty string. Default `limit=100`, max 1000; `count_only=true` returns the standard sentinel page with `total` = row count. The 196 KB UE-scale rejection is eliminated."
tasks:
  - id: "1.1"
    title: "Add SummaryRow type and shape change"
    status: complete
    verification: "`crates/code-graph-tools/src/handlers/symbols.rs` (or a shared types module under `handlers/`) defines `pub(super) struct SummaryRow { pub namespace: String, pub kind: &'static str, pub count: u32 }` with `Serialize` derive. The handler return type changes from `HashMap<String, HashMap<&'static str, u32>>` to `Page<SummaryRow>`. The shape change is breaking (intentional, pre-1.0); tool description updated in the same task to describe the new shape."
  - id: "1.2"
    title: "Refactor handler to flatten -> sort -> byte-budget-take"
    status: complete
    verification: "`get_symbol_summary` body in `symbols.rs:376-395`: (a) call `graph.read().symbol_summary(file)` to get the existing nested `HashMap<String, HashMap<SymbolKind, u32>>`; (b) flatten to `Vec<SummaryRow>` — for each `(ns, kinds_map)` entry, emit one row per `(kind, count)`; (c) sort the Vec by `(namespace, kind_str)` ascending using `Ord` on the tuple — stable across calls so paging is deterministic; (d) call `byte_budget_take(rows.into_iter(), resolved_offset, resolved_limit, max_bytes)` (the existing helper from PaginatedResponseSizeSafety); (e) emit `Page<SummaryRow>`. Limit defaults: `limit=100`, max 1000; `limit=0` resolves to default (mirrors `get_orphans`); `offset>=total` returns empty `results` with correct `total`."
    depends_on: ["1.1"]
  - id: "1.3"
    title: "Empty-namespace renamed to <global> in row output"
    status: complete
    verification: "When the source `s.namespace == \"\"`, the `SummaryRow.namespace` field is the literal string `<global>`. Non-empty namespaces pass through verbatim. The rename happens ONLY when building `SummaryRow` — `Symbol.namespace` in the graph stays empty (no in-place mutation). A unit test indexes a fixture with both global and non-global symbols and asserts both `<global>` and the real namespace string appear as distinct rows with correct counts. The rename does NOT affect `search_symbols` — filtering for global-scope symbols there still uses `namespace=\"\"` (verified by a second test that runs `search_symbols(namespace=\"\")` after the summary and asserts the same global-scope symbols are returned)."
    depends_on: ["1.2"]
  - id: "1.4"
    title: "count_only path returns row-count total"
    status: complete
    verification: "`get_symbol_summary` accepts a new `count_only: bool` argument (default false). When true, the handler returns the standard sentinel `Page { results: vec![], total: <row count>, offset: 0, limit: 0, truncated: false, next_offset: None }`. `total` is `summary.values().map(|m| m.len()).sum::<usize>() as u32` — the number of `(namespace, kind)` pairs, NOT the sum of symbol counts. A unit test asserts `page1.total == count_only_response.total` for the same query (i.e., paginating returns the same total)."
    depends_on: ["1.2"]
  - id: "1.5"
    title: "Tool description update for shape change + <global> caveat"
    status: complete
    verification: "`#[tool(description=...)]` for `get_symbol_summary` in `crates/code-graph-tools/src/server.rs:551` (or current line) is rewritten. New description names: (a) the new return shape — `Page<SummaryRow>` envelope; (b) sort order — `(namespace, kind)` asc; (c) defaults — `limit=100`, max 1000; (d) `count_only` semantics — total is row count not symbol sum; (e) the `<global>` caveat — \"`<global>` in this response is a display label for the empty namespace; to filter by global-scope symbols in `search_symbols`, use `namespace=\\\"\\\"`\". Tool-list snapshot regenerates; `cargo insta review` accepts deliberately. The Agent-facing tool descriptions lens applies: each suggested action verb operationally produces the claimed result."
    depends_on: ["1.3", "1.4"]
  - id: "1.6"
    title: "Structural verification"
    status: complete
    verification: "`cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all --check` clean; `cargo test -p code-graph-tools` green (new tests + existing tests stay green except the deliberately-regenerated tool-list snapshot for `get_symbol_summary`); `make snapshot-clean` passes."
    depends_on: ["1.5"]
tags: [mcp, pagination, ue, unreal-engine, ergonomics, hierarchy, diagrams, coupling, dependencies, fuzzy-match]
---

# Phase 1: get_symbol_summary pagination + <global> rename

## Overview

Smallest phase in the plan. Pure handler-layer refactor. Eliminates the 196 KB UE-scale rejection by flattening the nested map to rows and threading the existing `Page<T>` byte-budget machinery.

## 1.1: Add SummaryRow type and shape change

### Subtasks
- [x] Decide placement. Look for an existing types module (`crates/code-graph-tools/src/handlers/mod.rs` houses `Page<T>` and similar shared types per PaginatedResponseSizeSafety). Place `SummaryRow` there or in a focused submodule
- [x] `#[derive(Debug, Serialize)]` — `Debug` for test assertions, `Serialize` for the response
- [x] Field types: `namespace: String` (owned because `<global>` is constructed inline), `kind: &'static str` (the existing `kind_str` helper returns `&'static str`), `count: u32`
- [x] Update the handler's return type signature

### Notes
The existing handler at `symbols.rs:380-395` builds a `HashMap<String, HashMap<&'static str, u32>>` using `kind_str(k)` for the inner key. The row-form refactor consumes that same `kind_str` output so symbol-kind naming stays consistent across tools.

## 1.2: Refactor handler to flatten -> sort -> byte-budget-take

### Subtasks
- [ ] Rewrite `get_symbol_summary(graph, file, limit, offset, max_bytes, count_only)` to:
  - Get `summary: HashMap<String, HashMap<SymbolKind, u32>>` from `graph.read().symbol_summary(file)` (unchanged)
  - Build `let mut rows: Vec<SummaryRow> = ...` via nested iteration
  - Sort `rows.sort_by(|a, b| (a.namespace.as_str(), a.kind).cmp(&(b.namespace.as_str(), b.kind)))` — Ord on `(&str, &str)` tuple
  - Call `byte_budget_take(rows.into_iter(), resolved_offset, resolved_limit, max_bytes)` (existing helper)
  - Build `Page<SummaryRow>` from the returned tuple
  - Serialize via `tool_success_json(&page)`
- [ ] Defaults: `limit = 100` if `None` or `Some(0)`; clamp to 1000; `offset` defaults to 0
- [ ] Add unit tests for: (a) basic round-trip with multiple namespaces, (b) sort stability across two pages, (c) `limit=0` resolves to default, (d) `offset >= total` returns empty results with correct total

### Notes
The sort key includes both `namespace` and `kind` so two rows with the same namespace are sub-sorted by kind — keeps the page-boundary partition deterministic across paging calls. Without the secondary key, two rows with the same namespace could appear in different page-orderings on different calls.

## 1.3: Empty-namespace renamed to <global> in row output

### Subtasks
- [ ] In the flatten loop, when constructing `SummaryRow`: `let display_ns = if ns.is_empty() { "<global>".to_string() } else { ns.clone() };`
- [ ] Confirm the rename happens ONLY here. The graph's `Symbol.namespace` field stays empty — no graph mutation
- [ ] Unit test: index a fixture with one global-scope symbol (e.g., a top-level Rust function `fn foo() {}`) and one namespaced symbol (e.g., a function inside `mod bar`); call `get_symbol_summary`; assert `results` contains a row with `namespace: "<global>"` AND a row with `namespace: "bar"`
- [ ] Unit test: after the summary call, run `search_symbols(namespace="")` on the same graph; assert it returns `foo` (the global-scope symbol). Pins the asymmetry — display label is `<global>` but query filter is empty string

### Notes
The asymmetry (display vs query) is the load-bearing user-visible behavior; it's the only namespace renaming in the entire MCP surface. Phase 6's tool description for `search_symbols` will mention it from the other side ("use `namespace=\"\"` for global; the `<global>` label in `get_symbol_summary` is display-only").

## 1.4: count_only path returns row-count total

### Subtasks
- [x] Add `count_only: Option<bool>` (or `bool` with default-false) to the handler signature; thread through from the tool args
- [x] When true, return early: build the row count via `summary.values().map(|m| m.len()).sum::<usize>() as u32`; emit the standard sentinel page (`results: vec![], offset: 0, limit: 0, truncated: false, next_offset: None`)
- [x] Unit test: count_only=true returns total = row count; the same row count appears as `total` on the paginated response

### Notes
The sentinel shape (`limit: 0`, `offset: 0`, `truncated: false`, `next_offset: None`) mirrors `get_orphans`'s `count_only` per the PaginatedResponseSizeSafety contract. Reuse the same shape for client deserializer compatibility.

## 1.5: Tool description update for shape change + <global> caveat

### Subtasks
- [ ] Locate `get_symbol_summary` in `crates/code-graph-tools/src/server.rs` (around :551)
- [ ] Rewrite the description string to name: return shape, sort order, defaults, `count_only` semantics, `<global>` caveat
- [ ] Run `cargo test` — the tools-list snapshot for `get_symbol_summary` regenerates
- [ ] `cargo insta review` — accept the new description deliberately, not blanket-accept
- [ ] Apply the Agent-facing tool descriptions lens: "raise `limit` for more rows" operationally works; "use `namespace=\"\"` for `search_symbols` global filter" is true

### Notes
The tool description is production behavior for agents. Get the wording right; reviewers will read it as the user-facing contract.

## 1.6: Structural verification

### Subtasks
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Run `cargo fmt --all --check`
- [ ] Run `cargo test -p code-graph-tools`
- [ ] Run `make snapshot-clean`

## Acceptance Criteria
- [ ] `SummaryRow` type defined and exported as needed
- [ ] Handler returns `Page<SummaryRow>` with `<global>` rename
- [ ] `count_only` returns row-count total in sentinel shape
- [ ] Tool description rewritten and snapshot accepted
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
- [ ] `make snapshot-clean` passes
