---
title: "search_symbols suggestions + UE watcher preset + CLAUDE.md sweep + acceptance regression"
type: phase
plan: ResponseShapePolish
phase: 6
status: complete
created: 2026-05-13
updated: 2026-05-16
deliverable: "`search_symbols` with anchored-zero query (`^…$` returning total==0) attaches an optional `suggestions: Vec<String>` field with top-5 substring-fallback hits. `.code-graph.toml.example` ships a recommended UE watcher preset under `[discovery].extra_ignores`. CLAUDE.md fully updated for all 5 shape changes from Phases 1–5 (tool table rows, descriptions, response-shape contracts, cache-bump notice). Synthetic acceptance-regression fixture exercises the generic-UE-project scenarios end-to-end."
tasks:
  - id: "6.1"
    title: "SearchSymbolsResponse wrapper with #[serde(flatten)]"
    status: complete
    verification: "`crates/code-graph-tools/src/handlers/symbols.rs` (or shared handlers module) defines `#[derive(Serialize)] pub struct SearchSymbolsResponse { #[serde(flatten)] pub page: Page<SymbolResult>, #[serde(default, skip_serializing_if = \"Vec::is_empty\")] pub suggestions: Vec<String> }`. Compile-time smoke test in this same file (under `#[cfg(test)]`): construct a `SearchSymbolsResponse { page: Page { results: vec![], total: 0, offset: 0, limit: 20, truncated: false, next_offset: None }, suggestions: vec!\\[\"Foo\".into(), \"Bar\".into()] }`; serialize; assert the JSON output contains both top-level `results` (via flatten) AND a `suggestions` array. Confirm `#[serde(flatten)]` compiles cleanly with the generic `Page<SymbolResult>` — if it fails, fall back to a non-flatten layout (`{page: {...}, suggestions: [...]}`) and document the deviation."
  - id: "6.2"
    title: "search_symbols handler: anchored-zero suggestion trigger"
    status: complete
    verification: "`search_symbols` handler body is updated. After computing `Page<SymbolResult>`: if `page.total == 0` AND the query string starts with `^` AND ends with `$`, call `graph.search_symbols(pattern_inner, None)` directly (where `pattern_inner` is the query with `^` and `$` stripped); take the first 5 results' `symbol_id` strings. The existing `suggest_symbols` helper at `crates/code-graph-tools/src/handlers/mod.rs:199` returns `String` (comma-joined for embedding in error messages), NOT `Vec<String>` — it cannot be reused here without changing its signature, which would break its existing callers (`get_symbol_detail`, `get_callees`, `get_class_hierarchy`). The direct `graph.search_symbols` call returns the underlying matches as a `Vec<SymbolResult>`; collect the top 5 `symbol_id` strings into `Vec<String>`. Build `SearchSymbolsResponse { page, suggestions }` and return via `tool_success_json`. For NON-anchored zero-result queries OR for non-zero results, the `suggestions` field is empty and (via `skip_serializing_if`) ABSENT from the JSON — not `[]`. Unit tests: (a) `^NotFoundClass$` returns 0 results, suggestions field present with up to 5 substring hits; (b) `NotFoundClass` (non-anchored) returns 0 results, NO `suggestions` field in the JSON; (c) `^ExistingClass$` returns >=1 result, NO `suggestions` field."
    depends_on: ["6.1"]
  - id: "6.3"
    title: "UE watcher preset in .code-graph.toml.example"
    status: complete
    verification: "`.code-graph.toml.example` at the repo root gains a commented `[discovery]` block (or extends an existing one) with a recommended UE preset: `extra_ignores = [\"Intermediate/\", \"Saved/\", \"Binaries/\", \"DerivedDataCache/\", \".vs/\", \".idea/\", \".vscode/\", \"*.uasset\", \"*.umap\"]`. Block is fully commented out by default. Inline comments label this as the recommended starting point for Unreal Engine codebases; users are expected to opt in. The file remains TOML-valid when fully commented (verify via `toml::from_str(include_str!(...))` smoke test OR rely on the existing such test if it already covers `.code-graph.toml.example`). The change is documentation-only — no code touches the discovery system."
  - id: "6.4"
    title: "CLAUDE.md sweep across all 5 shape changes"
    status: complete
    verification: "`CLAUDE.md` is updated to describe the final aggregate state. Specific edits: (a) MCP tools table: 5 rows updated (`get_symbol_summary`, `get_class_hierarchy`, `generate_diagram`, `get_coupling`, `get_dependencies`, `detect_cycles`, `search_symbols`) — the Notes column for each names the new response shape/contract. (b) Response shapes section: new entries for `SummaryRow`, `HierarchyNode.ref`, `DiagramDirection`/`EdgeDirection`, `CouplingEntry`/`CouplingBoth`, `DependencyEntry`, `Cycle`, `SearchSymbolsResponse`. The `Page<T>` envelope description stays unchanged (the envelope itself didn't change). (c) Cache invalidation section: new bullet for `Graph::includes` schema bump (`CACHE_VERSION` bumped; old caches auto-trigger re-index on load); the `.ini` filter applies at index time so re-indexed graphs are clean. (d) Known limitations: the `generate_diagram` lossy-dedupe note ('rendered-label collapse') is documented; `detect_cycles` byte-budget non-application is documented. `grep -c '<global>\\|CouplingBoth\\|HierarchyNode.ref\\|DependencyEntry\\|max_cycle_size' CLAUDE.md` returns >= 5."
    depends_on: ["6.1", "6.2", "6.3"]
  - id: "6.5"
    title: "Acceptance regression: synthetic high-fanout fixture"
    status: complete
    verification: "New test `crates/code-graph-tools/tests/response_shape_acceptance.rs` (or matching existing acceptance-test convention). Constructs a synthetic Rust fixture engineered to exercise THREE of the generic-UE-project pain points: (a) ~50 namespaces × ~10 kinds = ~500 `SummaryRow` entries — enough to trigger byte-budget pagination on `get_symbol_summary` at the default 100 KB cap; (b) a deep trait hierarchy with explicit diamond inheritance — e.g., a base trait `Root`, two intermediate traits `D1: Root, D2: Root`, and a leaf trait `Leaf: D1 + D2` — to exercise `HierarchyNode.ref` (Rust uses traits for the diamond shape; `Inherits` edges fire on trait implementations); (c) a high-fanout function with ~100 callers — to exercise the `generate_diagram` direction + dedupe paths. The 100-file include-cycle scenario is **explicitly skipped** in this acceptance test — Rust has no real cyclic imports, and constructing the scenario in C++ would require the C++ language plugin in the test harness, breaking language homogeneity. Phase 5's `per_cycle_cap_truncates_large_scc` unit test (with a 200-file synthetic SCC) is the regression pin for that scenario; this is documented in 6.5's test docstring with a comment citing the Phase 5 test name. Each scenario asserts the FAILURE mode originally observed on a generic UE project is gone: summary response stays under 100 KB; hierarchy response has at least one `ref: true` stub; diagram has both `Calls` and `CalledBy` edges with no file-basename leaks. Test runs on every CI invocation; serves as the long-term regression target for the three covered scenarios."
    depends_on: ["6.1", "6.2", "6.3", "6.4"]
  - id: "6.6"
    title: "Structural verification"
    status: complete
    verification: "`make verify` passes (the exit-gated gate: clippy -D warnings, `cargo fmt --all --check`, `cargo test --workspace`, `make snapshot-clean` — fail-fast, single non-zero exit on any failure); the acceptance regression test (6.5) passes; CLAUDE.md grep checks (per 6.4) confirm sweep coverage; the 6.7 source-leak grep returns zero plan-artifact leaks; existing dogfood baselines stay within ±10%."
    depends_on: ["6.5", "6.7"]
  - id: "6.7"
    title: "Source-leak remediation sweep (judged, not blind-replaced)"
    status: complete
    verification: "Run `grep -rnE '(Task [0-9]+\\.[0-9]+|Phase [0-9]+|\\.?plans/|Plans/Active|ResponseShapePolish)' crates/*/src/` and classify EVERY hit into exactly one of two buckets. (1) PLAN-ARTIFACT LEAK — a comment/doc-comment/string that points at *which task/phase added or will add* code, or references a `.plans/`/`Plans/Active` path, or names `ResponseShapePolish`. These rot (no plan context survives) and MUST be rewritten to describe the BEHAVIOR/mechanism instead (the established fix from every prior phase). (2) LEGITIMATE HISTORICAL CONTEXT — preamble text where a phase reference is the canonical documentation of THAT subsystem's origin under the *original RustRewrite plan* (e.g. `server.rs` module preamble 'Phase 3.1 ships the scaffold…', `mod.rs` 'Phase 3.4 filled in the P0 handlers'); these describe a shipped-and-documented architectural milestone, not a rotting pointer, and are PRESERVED verbatim. The deliverable: zero bucket-(1) leaks remain in `crates/*/src/` (re-run the grep, confirm every surviving hit is a justified bucket-(2) entry); a short report enumerates each surviving (2) hit with a one-line justification so the scan can audit the judgment. No behavior change — comment/doc/string edits only; `make verify` stays green. This is the deferred remediation tracked across the Phase 2–5 debriefs (the prevention rule stopped NEW leaks; this clears residual pre-existing debt with per-hit judgment, NOT a blind search-replace)."
    depends_on: ["6.1", "6.2"]
tags: [mcp, pagination, ue, unreal-engine, ergonomics, hierarchy, diagrams, coupling, dependencies, fuzzy-match]
---

# Phase 6: search_symbols suggestions + UE watcher preset + CLAUDE.md sweep + acceptance regression

## Overview

Wraps the plan. Three small features (search suggestions, UE watcher preset, the acceptance regression) plus the cross-cutting CLAUDE.md sweep that records the final state of all five Phase 1–5 changes.

This phase waits on every other phase to land because the CLAUDE.md sweep needs to describe the final aggregate — partial sweep would either leave stale references or pre-describe undelivered behavior.

## 6.1: SearchSymbolsResponse wrapper with #[serde(flatten)]

### Subtasks
- [ ] Define `SearchSymbolsResponse` struct with `#[serde(flatten)] pub page: Page<SymbolResult>` and `#[serde(default, skip_serializing_if = "Vec::is_empty")] pub suggestions: Vec<String>`
- [ ] Compile-time smoke test:
  ```rust
  #[test]
  fn search_symbols_response_flattens_correctly() {
      let r = SearchSymbolsResponse {
          page: Page { results: vec![], total: 0, offset: 0, limit: 20, truncated: false, next_offset: None },
          suggestions: vec!["Foo".into(), "Bar".into()],
      };
      let json = serde_json::to_string(&r).unwrap();
      assert!(json.contains("\"results\":[]"));        // flattened from page
      assert!(json.contains("\"total\":0"));           // flattened from page
      assert!(json.contains("\"suggestions\":[\"Foo\",\"Bar\"]"));
  }
  ```
- [ ] If `#[serde(flatten)]` fails to compile with the generic `Page<SymbolResult>` (rare, but it's a known serde limitation with certain generic + flatten combinations), fall back to a nested layout: `{page: <Page>, suggestions: [...]}` and update the tool description in 6.4 to match
- [ ] Document the chosen layout in a code comment on the struct

### Notes
The `#[serde(flatten)]` approach is preferred because it preserves the existing wire shape on the page-fields (agents already pattern-match on `results`/`total`/`offset`/`limit`). A nested layout would be a wire break — clients would have to dig into `response.page.results` instead of `response.results`. Try flatten first; only fall back if it fails the smoke test.

## 6.2: search_symbols handler: anchored-zero suggestion trigger

### Subtasks
- [ ] Open the `search_symbols` handler body in `crates/code-graph-tools/src/handlers/symbols.rs`
- [ ] After computing the `Page<SymbolResult>`, check the suggestion trigger. Do NOT use `suggest_symbols` (its return type is `String`, comma-joined for error messages — different shape from what we need). Call `graph.search_symbols` directly and collect the top 5 symbol IDs:
  ```rust
  let suggestions: Vec<String> = if page.total == 0
      && query.starts_with('^')
      && query.ends_with('$')
  {
      let inner = &query[1..query.len() - 1];  // strip ^ and $
      graph.search_symbols(inner, None)
          .into_iter()
          .take(5)
          .map(|sr| sr.symbol_id)
          .collect()
  } else {
      Vec::new()
  };
  ```
- [ ] Confirm `graph.search_symbols(name, None)` returns a `Vec<SymbolResult>` (per `graph.rs` / `queries.rs`); the `None` second arg disables the namespace filter. If the existing API differs, adjust to call whatever underlying broad-match helper the graph exposes
- [ ] Do NOT modify `suggest_symbols` itself — its existing callers (`get_symbol_detail`, `get_callees`, `get_class_hierarchy`) all consume the `String` form for error messages and changing the return type would break them
- [ ] Build `SearchSymbolsResponse { page, suggestions }` and return via `tool_success_json`
- [ ] Three unit tests per the verification field

### Notes
The anchored-exact-only trigger is intentional (per design Decision 8). Substring queries returning zero suggest a "concept not in codebase" rather than a "typo of an existing symbol"; suggestions there would be noise.

## 6.3: UE watcher preset in .code-graph.toml.example

### Subtasks
- [ ] Open `.code-graph.toml.example` at the repo root
- [ ] Locate the existing `[discovery]` block (or add one if not present) — check whether it's fully commented out or active
- [ ] Add a commented `# extra_ignores = [...]` block with the UE preset
- [ ] Add inline comments naming the preset's purpose: "Recommended starting point for Unreal Engine codebases — engine-side scratch dirs that re-generate on every build and aren't worth watching/indexing"
- [ ] Verify TOML validity post-edit (the existing smoke test, if any, covers this; otherwise add one)

### Notes
This is pure documentation; the `extra_ignores` mechanism already exists in `[discovery]`. The change is making the right preset discoverable.

## 6.4: CLAUDE.md sweep across all 5 shape changes

### Subtasks
- [ ] Open `CLAUDE.md` and read the current `MCP tools` table — identify the rows for the 7 affected tools (`get_symbol_summary`, `get_class_hierarchy`, `generate_diagram`, `get_coupling`, `get_dependencies`, `detect_cycles`, `search_symbols`)
- [ ] Update each row's Notes column to name the new response shape/contract
- [ ] Open the `Response shapes` section — currently describes `Page<T>` and `get_class_hierarchy`'s tree shape. Add entries for:
  - `SummaryRow` (Phase 1)
  - `HierarchyNode.ref: Option<bool>` (Phase 2)
  - `DiagramDirection` / `EdgeDirection` / `DiagramEdge.direction` (Phase 3)
  - `CouplingEntry` / `CouplingBoth` (Phase 4)
  - `DependencyEntry { file, kind, line }` (Phase 4) — **document `kind` as always `"includes"`** for all languages. `EdgeKind` has only three variants (`Calls`, `Includes`, `Inherits`); every language plugin emits `EdgeKind::Includes` for file-import edges (Rust `use`, Python `import`, Go `import`, Java `import`, C# `using`, C++ `#include`). The design's mention of `"imports"` as an alternative value was speculative — it is not produced today. If a future plan adds a separate `EdgeKind::Imports` variant, both the design and this CLAUDE.md entry update together
  - `Cycle { files, truncated, original_len }` (Phase 5)
  - `SearchSymbolsResponse` with optional `suggestions` (Phase 6)
- [ ] Open the Cache invalidation section — add a new bullet about Phase 4's `Graph::includes` schema bump (auto re-index on load) and the `.ini` filter (indexer-layer, doesn't require `force=true` going forward)
- [ ] Open the Known limitations section — add the `generate_diagram` lossy-dedupe note (Phase 3) and the `detect_cycles` byte-budget non-application note (Phase 5)
- [ ] Grep verification: `grep -c '<global>\|CouplingBoth\|HierarchyNode.ref\|DependencyEntry\|max_cycle_size' CLAUDE.md` returns >= 5

### Notes
The CLAUDE.md sweep is the highest-volume task in the phase. Allocate enough time to read each affected section coldly (per the "Documentation read cold" lens) and confirm framing consistency across sections (a feature documented in two places must convey consistent signals).

## 6.5: Acceptance regression: synthetic high-fanout fixture

### Subtasks
- [ ] Create `crates/code-graph-tools/tests/response_shape_acceptance.rs` (new file)
- [ ] Build the synthetic fixture programmatically via the test-server harness:
  - 50 namespaces × 10 kinds via small generated Rust files: e.g., `ns_0/file.rs` with `pub fn fn_0() {}`, `pub struct S_0;`, etc. — repeat for ns_0 through ns_49
  - Diamond hierarchy: `pub trait Root {}`, `pub trait D1: Root {}`, `pub trait D2: Root {}`, `pub trait Leaf: D1 + D2 {}` — `Inherits` edges fire on trait-implementation edges; Leaf reaches Root via both D1 and D2 (a diamond)
  - High-fanout function: 100 callers calling a single `target_fn()` — generate via simple loop in a generator helper
  - **Cycle scenario explicitly skipped** — Rust has no real cyclic imports; constructing a C++ fixture for this one sub-scenario would force language-mixing in the test. Phase 5's `per_cycle_cap_truncates_large_scc` unit test pins the cycle regression with a 200-file synthetic SCC; this acceptance test documents the skip in its doc comment and points to that test as the cycle-scenario pin
- [ ] Each scenario asserts the specific failure mode observed on a generic UE project is gone:
  - `get_symbol_summary` global response under 100 KB
  - `get_class_hierarchy("Root", down)` contains at least one `ref: true` stub in the JSON
  - `generate_diagram(symbol=target_fn)` has edges with both `direction: "calls"` and `direction: "called_by"`; no edges with file-basename `from` or `to`
- [ ] Each assertion names the failure mode in its message so future regressions are diagnosable
- [ ] Test doc comment names the explicitly-skipped scenario: "100-file include cycle is covered by Phase 5's `per_cycle_cap_truncates_large_scc` unit test (200-file synthetic SCC). This acceptance test stays Rust-only for harness simplicity."

### Notes
The diamond fixture in Rust uses traits — Rust doesn't have multiple class inheritance, but `Inherits` edges fire on trait implementations, so a trait pyramid (`Root` ← `D1, D2` ← `Leaf`) produces the same diamond shape in the graph. Confirm by inspecting the graph output before writing the assertion.

The 100-file include cycle is harder to construct in Rust because Rust doesn't have real cyclic imports. Pragmatic answer: use a small synthetic C++ fixture for that one scenario, OR skip that assertion (and document why) and lean on Phase 5's per-cycle-cap unit tests instead.

## 6.6: Structural verification

### Subtasks
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Run `cargo fmt --all --check`
- [ ] Run `cargo test --workspace`
- [ ] Run `make snapshot-clean`
- [ ] Run the acceptance regression test (6.5) and confirm passing
- [ ] Run the dogfood baselines: `cargo test -p code-graph-lang-cpp fmt`, `... curl`, `... abseil-cpp` — confirm within ±10%
- [ ] Spot-check CLAUDE.md: grep for the strings named in 6.4's verification field

## 6.7: Source-leak remediation sweep (judged, not blind-replaced)

### Subtasks
- [ ] `grep -rnE '(Task [0-9]+\.[0-9]+|Phase [0-9]+|\.?plans/|Plans/Active|ResponseShapePolish)' crates/*/src/` — capture every hit
- [ ] For each hit, classify into bucket (1) plan-artifact leak (rots — rewrite to describe behavior/mechanism) or bucket (2) legitimate RustRewrite-plan historical preamble (the canonical origin doc for that subsystem — preserve verbatim)
- [ ] Rewrite every bucket-(1) hit to a behavioral description (the established per-phase fix); leave every bucket-(2) hit untouched
- [ ] Re-run the grep; confirm zero bucket-(1) leaks remain
- [ ] Produce a short report enumerating each surviving bucket-(2) hit + a one-line justification (so the scan can audit the judgment, not just the absence)
- [ ] Confirm `make verify` stays green (comment/doc/string edits only — zero behavior change)

### Notes
This is the one-time remediation deferred across the Phase 2–5 debriefs. The standing "no plan/task labels in source" prevention rule stopped NEW leaks every phase; this clears the residual PRE-existing debt. The judgment matters: a blind grep-replace would destroy legitimate RustRewrite-plan milestone documentation (e.g. `server.rs`'s "Phase 3.1 ships the scaffold…" preamble is the canonical origin doc for the rmcp-server subsystem, not a rotting pointer). Bucket (1) = "this comment tells you *which task added this code*" → rot, rewrite. Bucket (2) = "this preamble documents *when/how this subsystem came to exist* under the original rewrite plan" → keep.

## Acceptance Criteria
- [ ] `SearchSymbolsResponse` wrapper using `#[serde(flatten)]` ships (or documented fallback layout)
- [ ] Anchored-zero suggestion trigger fires; substring-zero does not
- [ ] UE watcher preset documented in `.code-graph.toml.example`
- [ ] CLAUDE.md sweep covers all 5 shape changes (tool table, response shapes, cache invalidation, known limitations)
- [ ] Acceptance regression fixture passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
- [ ] `make snapshot-clean` passes
- [ ] Dogfood baselines within ±10%
