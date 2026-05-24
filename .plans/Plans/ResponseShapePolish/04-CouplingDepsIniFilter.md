---
title: "get_coupling + get_dependencies + .ini filter + Graph::includes widening"
type: phase
plan: ResponseShapePolish
phase: 4
status: complete
created: 2026-05-13
updated: 2026-05-15
deliverable: "`get_coupling(direction=both)` returns `CouplingBoth { incoming: Page<CouplingEntry>, outgoing: Page<CouplingEntry> }`; `get_coupling(direction=incoming|outgoing)` returns `Page<CouplingEntry>`; sequential byte-budget allocation (incoming first, outgoing gets remainder). `get_dependencies` returns `Page<DependencyEntry { file, kind, line }>`. `Graph::includes` widened to preserve line numbers; `CACHE_VERSION` bumped so old caches force re-index. `.ini` and other non-source-extension paths filtered at indexer `resolve_edges` before edges reach the graph."
tasks:
  - id: "4.1"
    title: "Widen Graph::includes to preserve line numbers + bump cache schema"
    status: complete
    verification: "`crates/code-graph-graph/src/graph.rs` defines `pub struct IncludeEntry { pub path: PathBuf, pub line: u32 }` with Serde derives. `Graph::includes` type changes from `HashMap<PathBuf, Vec<PathBuf>>` to `HashMap<PathBuf, Vec<IncludeEntry>>`. The widening cascades to SIX call sites that must all be updated together or compilation fails: (1) `merge_file_graph` at `graph.rs:148-153` pushes `IncludeEntry { path, line: edge.line }` instead of bare `PathBuf`; (2) `Graph::coupling` at `diagrams.rs:~111-113` (`for inc in incs { counts.entry(inc.clone()) }` → `inc.path.clone()`); (3) `Graph::incoming_coupling` at `diagrams.rs:~156-162` (the `if inc == path` comparison → `inc.path == path`); (4) `Graph::diagram_file_graph` BFS outgoing loop at `diagrams.rs:~316-322` (the `raw_edges.push((curr.clone(), inc.clone()))` and `visited.contains(inc)` calls switch to `inc.path`); (5) `Graph::diagram_file_graph` BFS incoming loop at `diagrams.rs:~328-335` (same treatment as the outgoing loop); (6) `Graph::file_dependencies` in `callgraph.rs:~151` signature changes from `-> Option<&[PathBuf]>` to `-> Option<&[IncludeEntry]>`. Test updates: assertions in `callgraph.rs:~457-479` (existing `file_dependencies` tests) update to compare against `IncludeEntry` rather than raw `PathBuf`. The `GraphCache.includes` field in `persist.rs:57-70` auto-updates via the type change (Serde derives unchanged). `CACHE_VERSION` in `persist.rs` is incremented (next integer above current); existing cache deserialization fails on version mismatch — `Graph::load` returns `Ok(false)` for old caches → `analyze_codebase` falls through to full re-index. No transparent migration. The version-bump is documented in a code comment at the `CACHE_VERSION` definition citing this phase."
  - id: "4.2"
    title: ".ini filter at indexer resolve_edges"
    status: complete
    verification: "The indexer's edge-resolution loop (in `crates/code-graph-tools/src/indexer.rs` — the `resolve_edges` function or similar) gains an extension check: after `plugin.resolve_include(raw, file_index)` returns `Some(resolved_path)`, the indexer checks `registry.language_for_path(&resolved_path).is_some()` — if not, the edge is dropped (continue). The same check runs in the watch handler's reindex path (`crates/code-graph-tools/src/handlers/watch.rs` — the `resolve_edges` equivalent). The filter applies universally (not just C++) — any language plugin that emits an Includes edge to a non-source file is filtered identically. Indexer-layer test: synthesize a `FileGraph` with one `Includes` edge pointing to `<../config/foo.ini>` and one to `<sibling.h>`; run `resolve_edges`; assert the resulting graph has only the `.h` entry in `self.includes`."
    depends_on: ["4.1"]
  - id: "4.3"
    title: "CouplingEntry, DependencyEntry, CouplingBoth types + handler shape changes"
    status: complete
    verification: "`crates/code-graph-tools/src/handlers/` defines: `pub struct CouplingEntry { pub file: String, pub count: u32 }`; `pub struct DependencyEntry { pub file: String, pub kind: &'static str, pub line: u32 }`; `pub struct CouplingBoth { pub incoming: Page<CouplingEntry>, pub outgoing: Page<CouplingEntry> }`. All with `Serialize` derives. `get_coupling` handler at `structure.rs:308-354` branches on `direction`. The graph layer exposes outgoing coupling via `Graph::coupling()` and incoming via `Graph::incoming_coupling()` (two separate methods at `diagrams.rs:~111` and `~156` respectively — there is no unified `coupling(direction=...)` API; the handler routes to the appropriate method). `incoming|outgoing` returns `Page<CouplingEntry>` — entries from the appropriate graph method mapped into rows, sorted desc by count then asc by file, byte-budgeted. `both` returns `CouplingBoth` with sequential allocation — call `byte_budget_take` for incoming first with the full `max_bytes` cap; subtract consumed bytes plus 32-byte fixed overhead for the outer `CouplingBoth` JSON wrapper from `max_bytes`; pass the remainder to a second `byte_budget_take` for outgoing. If incoming exhausts the budget, outgoing receives an empty `Page` with `truncated: true, next_offset: Some(0)`."
    depends_on: ["4.1"]
  - id: "4.4"
    title: "get_dependencies handler emits Page<DependencyEntry>"
    status: complete
    verification: "`get_dependencies` handler at `query.rs:126-139` is rewritten. Calls `graph.read().file_dependencies(Path::new(file))` (now returns `&[IncludeEntry]`); maps each entry to `DependencyEntry { file: entry.path.to_string_lossy().into_owned(), kind: kind_str(EdgeKind::Includes), line: entry.line }`; sorts by `(file, line)` ascending; calls `byte_budget_take(rows, offset, limit, max_bytes)`; returns `Page<DependencyEntry>`. The `kind` field is currently always `\"includes\"` (Includes edges are the only edges stored in `Graph::includes`); when other edge kinds get added to includes, the kind threads through. The `line` field reflects the source line where the `#include` (or `import`) directive appears. Test fixture with three `#include` lines at known positions; assert each line number matches."
    depends_on: ["4.1", "4.3"]
  - id: "4.5"
    title: "Integration tests across all 4 sub-changes"
    status: complete
    verification: "(a) `get_coupling_both_split_shape` — fixture file with 3 incoming and 2 outgoing includes; call `get_coupling(direction=both)`; assert response is `CouplingBoth` with non-empty `incoming` and `outgoing` pages; verify sort order (desc by count then asc by file); (b) `get_coupling_byte_budget_sequential` — fixture with many entries; set `max_bytes` small enough that incoming consumes most of it; call `direction=both`; assert outgoing has `truncated: true` and `next_offset` set to 0 (start-fresh marker since outgoing didn't emit anything); (c) `get_coupling_directional_pagination_resume` — fixture with > limit entries on incoming side; call `direction=both` and observe `truncated=true` on incoming; then call `direction=incoming` with `offset = next_offset`; assert continuation returns the remaining entries; (d) `get_dependencies_line_numbers_preserved` — fixture with `#include` at line 5, 10, 15; assert response entries have line 5, 10, 15 (sorted); (e) `indexer_ini_filter_drops_non_source_edges` — indexer-layer test from 4.2; (f) `get_dependencies_ini_excluded_from_response` — handler-layer test confirming the filter shows through to the handler response."
    depends_on: ["4.4"]
  - id: "4.6"
    title: "Tool descriptions for both shape changes"
    status: complete
    verification: "Tool descriptions in `server.rs` updated for `get_coupling` and `get_dependencies`. `get_coupling` description names: the two response shapes (`Page<CouplingEntry>` for directional, `CouplingBoth` for both), the sort order, the byte-budget allocation strategy when `direction=both`, and the paging-continuation contract (\"when `direction='both'` returns `truncated: true` on a side, re-call with `direction='incoming'` (or `'outgoing'`) and `offset = next_offset` from the truncated page to continue\"). `get_dependencies` description names the new `DependencyEntry` shape (`file`, `kind`, `line`) and the page envelope. The `.ini` filter is a graph-population behavior — not directly user-visible — but mentioned briefly in the `get_dependencies` description: \"Non-source-file targets (`.ini`, etc.) are filtered at index time and do not appear in dependencies.\" Tool descriptions describe STEADY-STATE post-upgrade behavior only; cache-bump migration notes live in CLAUDE.md (Phase 6.4) and the PR description, NOT in the permanent agent-facing tool descriptions (a notice saying 'this phase bumped the schema' becomes stale documentation six months later when an agent reads it cold). Tool-list snapshots regenerate; `cargo insta review` accepts deliberately."
    depends_on: ["4.5"]
  - id: "4.7"
    title: "Structural verification"
    status: complete
    verification: "`cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all --check` clean; `cargo test --workspace` green; `make snapshot-clean` passes; existing dogfood baselines stay within ±10% (the `.ini` filter MAY shave a small percentage of edges from baselines that picked up junk paths — if the drift exceeds ±10%, update the baseline per the CLAUDE.md SHA-bump protocol AND note in this task's debrief why the count moved); the cache-schema bump is verified by running the test suite with a pre-existing cache present — confirm the test logs the version mismatch + re-index path."
    depends_on: ["4.6"]
tags: [mcp, pagination, ue, unreal-engine, ergonomics, hierarchy, diagrams, coupling, dependencies, fuzzy-match]
---

# Phase 4: get_coupling + get_dependencies + .ini filter + Graph::includes widening

## Overview

Four interlocking changes that all touch the include-graph layer. Bundling is correct because they share infrastructure: the line-preserving widening of `Graph::includes` (4.1) is what makes the `get_dependencies` line field possible (4.4); the `.ini` filter (4.2) cleans up the same map; the response shape changes (4.3, 4.4) consume the widened type.

The cache schema bump is unconditional — every user re-indexes on first post-upgrade run. Pre-1.0 codebase; acceptable per Decision 10.

## 4.1: Widen Graph::includes to preserve line numbers + bump cache schema

### Subtasks
- [x] Define `IncludeEntry { path: PathBuf, line: u32 }` in `crates/code-graph-graph/src/graph.rs` with `#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]`
- [x] Change `Graph::includes` field type from `HashMap<PathBuf, Vec<PathBuf>>` to `HashMap<PathBuf, Vec<IncludeEntry>>`
- [x] Update `merge_file_graph` at `graph.rs:148-153` — push `IncludeEntry { path: target.clone(), line: edge.line }` instead of just `target`
- [x] Update `Graph::coupling` in `diagrams.rs:~111-113` — `for inc in incs { counts.entry(inc.clone()).or_insert(0).inc(); }` → `inc.path.clone()` for the counts key
- [x] Update `Graph::incoming_coupling` in `diagrams.rs:~156-162` — the `if inc == path` membership check changes to `if inc.path == path` (the comparison is against `PathBuf`, now needs to dereference the new struct)
- [x] Update `Graph::diagram_file_graph` outgoing BFS loop at `diagrams.rs:~316-322` — `raw_edges.push((curr.clone(), inc.clone()))` and `visited.contains(inc)` references switch to `inc.path` for both the push and the contains-check
- [x] Update `Graph::diagram_file_graph` incoming BFS loop at `diagrams.rs:~328-335` — same treatment as the outgoing loop above
- [x] Update `Graph::file_dependencies` in `callgraph.rs:~151` — signature changes from `-> Option<&[PathBuf]>` to `-> Option<&[IncludeEntry]>`
- [x] Update existing `file_dependencies` test assertions in `callgraph.rs:~457-479` — compare against `IncludeEntry` rather than raw `PathBuf` (likely via building expected `IncludeEntry` values inline)
- [x] Confirm `GraphCache` struct in `persist.rs:57-70` auto-updates via the type change — Serde derives on the existing struct re-derive from the new `Vec<IncludeEntry>` member type
- [x] Bump `CACHE_VERSION` constant in `persist.rs` — find the current value, increment, add a `// v<N>: get_dependencies line preservation (ResponseShapePolish Phase 4)` comment
- [x] Verify the existing version-mismatch path in `Graph::load` correctly handles the bump — old caches return `Ok(false)` from the load function, falling through to the full re-index path in `analyze_codebase`
- [x] Run `cargo build --workspace` after each call-site change to localize compile errors; the cascade is large enough that getting it green incrementally is faster than all-at-once

### Notes
The version bump is the breaking event. Every user post-upgrade re-indexes. The verification step (running with a pre-existing cache) is in 4.7 — log inspection confirms the re-index path was taken.

## 4.2: .ini filter at indexer resolve_edges

### Subtasks
- [x] Open `crates/code-graph-tools/src/indexer.rs`; locate the `resolve_edges` function (or the equivalent edge-resolution loop)
- [x] After `plugin.resolve_include(raw, file_index)` returns `Some(resolved_path)`, insert: `if registry.language_for_path(&resolved_path).is_none() { continue; }` — skip the edge
- [x] Do NOT log the skip. The filter fires frequently in C++ codebases (`#include <stdio.h>` resolves to system headers we don't index); logging would flood
- [x] Apply the same filter in `crates/code-graph-tools/src/handlers/watch.rs`'s reindex path — locate the edge-resolution call and add the identical check
- [x] Indexer-layer test in `tests/`: synthesize a `FileGraph` with two `Includes` edges (one to `.ini`, one to `.h`); run the resolve loop; assert only the `.h` entry survives in `self.includes`
- [x] Confirm the C++ dogfood baselines stay within ±10% post-filter — if abseil-cpp or fmt baselines drop by 11%+, investigate (likely some legitimate include paths are getting filtered as side effects; tune the test fixture)

### Notes
The filter is universal across languages. Today's C++ resolver is the only one observed to pick up junk paths (the `.ini` false-positive), but Rust/Go/Python plugins could exhibit the same in principle. One central filter at the indexer protects all of them.

## 4.3: CouplingEntry, DependencyEntry, CouplingBoth types + handler shape changes

### Subtasks
- [x] Define `CouplingEntry`, `DependencyEntry`, `CouplingBoth` per the verification field
- [x] Rewrite `get_coupling` handler at `structure.rs:308-354`:
  - For `direction == "incoming"` or `"outgoing"`: get the map from `graph.coupling(file, direction)`; flatten to `Vec<CouplingEntry>`; sort by `(count desc, file asc)`; call `byte_budget_take`; return `Page<CouplingEntry>`
  - For `direction == "both"`: get both maps; build incoming and outgoing rows; call `byte_budget_take` for incoming with full `max_bytes`; subtract consumed + 28-byte overhead; call `byte_budget_take` for outgoing with remainder; bundle into `CouplingBoth`
- [x] If the existing `Graph::coupling` signature doesn't accept a direction filter, add `Graph::incoming_coupling` / `Graph::outgoing_coupling` or refactor to a direction-parameter API. Inspect existing signatures and pick the path of least churn
- [x] Default `limit = 50` per side (per design); max 1000; standard `byte_budget_take` defaults

### Notes
The "28-byte overhead" constant accounts for the literal `{"incoming":<page>,"outgoing":<page>}` outer JSON. Over-estimate slightly (use 32 or 48) to leave headroom; under-estimating risks emitting an envelope that's a few bytes over budget. The exact number is empirical — set it conservatively and let tests confirm.

## 4.4: get_dependencies handler emits Page<DependencyEntry>

### Subtasks
- [x] Rewrite `get_dependencies` handler at `query.rs:126-139`:
  - Call `graph.read().file_dependencies(Path::new(file))` — now returns `Option<&[IncludeEntry]>`
  - `None` (file not in graph) → standard not-found response with empty results
  - `Some(entries)` → map to `Vec<DependencyEntry>`, sort by `(file, line)` asc, call `byte_budget_take`, return `Page<DependencyEntry>`
- [x] `kind` field: today every edge is `EdgeKind::Includes`; use `kind_str(EdgeKind::Includes)` which returns `"includes"`. If future edge kinds get added to the includes map (currently they're not), thread through the actual kind
- [x] Default `limit = 100`; max 1000

### Notes
The sort by `(file, line)` is the deterministic-paging key. Two `#include` lines for the same file (rare but possible — e.g., conditional includes in different macro branches that both happen to land in the index) get sorted by line so the second occurrence's line is visible.

## 4.5: Integration tests across all 4 sub-changes

### Subtasks
- [x] 6 tests per the verification field
- [x] Tests (a)-(d) live in `tests/coupling_dependencies.rs` (new file or extend existing)
- [x] Test (e) lives in `tests/indexer_resolve_edges.rs` (new file if needed) — pure indexer-layer
- [x] Test (f) lives wherever (a)-(d) live; it's a handler-layer round-trip confirmation
- [x] Each test has a focused failure message naming the offending behavior

### Notes
The byte-budget sequential-allocation test (b) is the trickiest. The test must construct a fixture and pick a `max_bytes` value that's just barely large enough for incoming to fit completely or just barely too small. Pick a value that makes the test deterministic; document the math in a code comment so future readers understand the magic number.

## 4.6: Tool descriptions for both shape changes

### Subtasks
- [x] Update `get_coupling` description per verification field
- [x] Update `get_dependencies` description per verification field
- [x] Do NOT include a cache-bump notice in the tool descriptions — they're permanent agent-facing contracts that describe steady-state behavior. The cache-bump goes in CLAUDE.md (Phase 6.4) and the PR description, where future readers see it in the right context
- [x] Apply the Agent-facing tool descriptions lens; verify paging-continuation phrasing matches the actual mechanism
- [x] `cargo insta review` for both tool-list snapshots

### Notes
The earlier draft of this task added a cache-bump notice to tool descriptions. Plan-reviewer correctly flagged that this is migration-time information that becomes stale documentation later. Tool descriptions should describe what the tool does today, not what changed during a particular upgrade.

## 4.7: Structural verification

### Subtasks
- [x] Run `cargo clippy --workspace --all-targets -- -D warnings`
- [x] Run `cargo fmt --all --check`
- [x] Run `cargo test --workspace`
- [x] Run `make snapshot-clean`
- [x] Run dogfood baselines: `cargo test -p code-graph-lang-cpp fmt`, `... curl`, `... abseil-cpp` — confirm within ±10%
- [x] Manual cache-bump verification: with a pre-bump cache file present in a tempdir, run `analyze_codebase`; confirm the test log shows the version-mismatch + re-index path

### Notes
The dogfood baselines may shift slightly post-`.ini`-filter. Document any drift in the debrief; small shifts are acceptable, large shifts (>10%) need investigation before merge.

## Acceptance Criteria
- [x] `IncludeEntry` defined; `Graph::includes` widened; `CACHE_VERSION` bumped
- [x] `.ini` filter at indexer (and watch reindex) drops non-source-extension edges
- [x] `CouplingEntry`, `DependencyEntry`, `CouplingBoth` types defined
- [x] `get_coupling` handler emits new shapes with sequential byte-budget allocation
- [x] `get_dependencies` handler emits `Page<DependencyEntry>` with line numbers
- [x] 6 integration tests pass
- [x] Tool descriptions updated with shape, paging-continuation, and cache-bump notice
- [x] Dogfood baselines within ±10% (or baselines bumped with documented reason)
- [x] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [x] `cargo fmt --all --check` clean
- [x] `make snapshot-clean` passes
