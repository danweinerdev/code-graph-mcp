---
title: "Cache migration across all GraphCache path-bearing fields"
type: phase
plan: PathNormalization
phase: 2
status: complete
created: 2026-05-13
updated: 2026-05-13
deliverable: "`Graph::load` walks every path-bearing field of the deserialized `GraphCache` and applies `paths::simplify` in place. Old `\\\\?\\`-prefixed JSON caches load to a fully-consistent in-memory graph with no `\\\\?\\` strings anywhere. Anti-regression test pins all 10 distinct path locations; end-to-end consistency test proves cross-field key alignment after migration."
tasks:
  - id: "2.1"
    title: "Implement simplify_cache helper walking all 10 path-bearing locations"
    status: complete
    verification: "`simplify_cache(cache: &mut GraphCache)` lives as a private helper in `crates/code-graph-graph/src/persist.rs`. The helper rewrites every path-bearing field of `GraphCache` per the Decision 5 table in `Designs/PathNormalization/README.md`. The 10 locations: (1) `files: HashMap<PathBuf, FileEntry>` keys; (2) each `FileEntry.symbol_ids: Vec<SymbolId>` entry (SymbolId encodes `<file>:<name>`; strip the `<file>` portion); (3) `nodes: HashMap<SymbolId, Symbol>` keys (same `<file>:<name>` rewrite); (4) each `Symbol.file: String` value inside `nodes`; (5) `adj: HashMap<SymbolId, Vec<EdgeEntry>>` keys; (6) each `EdgeEntry.target: SymbolId` inside `adj`; (7) each `EdgeEntry.file: PathBuf` inside `adj`; (8/9/10) the same three fields inside `radj`; plus (11) `includes: HashMap<PathBuf, Vec<PathBuf>>` keys AND inner Vec values; (12) `mtimes: HashMap<PathBuf, u64>` keys. Helper is idempotent — running it twice produces byte-identical output. Helper uses `paths::simplify` for `PathBuf` fields and a `SymbolId` split-and-rejoin for the symbol-id strings (split on the rightmost `:` not part of `::`, simplify the file portion, rejoin). Helper has its own unit test asserting idempotency on already-clean input."
  - id: "2.2"
    title: "Wire simplify_cache into Graph::load (only the graph-materialization path)"
    status: complete
    verification: "`Graph::load` in `crates/code-graph-graph/src/persist.rs` (around line 178) calls `simplify_cache(&mut cache)` immediately after `serde_json::from_slice` and before the cache is consumed into the in-memory `Graph`. The migration is NOT applied at the `stale_paths` deserialization at line 217 — that function only reads `cache.mtimes` to compare on-disk timestamps and returns a `Vec<PathBuf>` whose only use is an emptiness check in `analyze_codebase`'s cache fast-path; running `simplify_cache` there would rewrite 9 fields the function discards immediately AND would risk reformatting the keys used for the subsequent `mtime_nanos` filesystem call (where the extended-path form may actually be required for paths near 260 chars). The migration is only applied on the cache→graph materialization path. On already-clean caches `simplify_cache` is a no-op (per `paths::simplify`'s identity behavior on non-extended paths). No schema-version bump is needed; the JSON shape is byte-identical, only string contents change."
    depends_on: ["2.1"]
  - id: "2.3"
    title: "Anti-regression test: every path-bearing field is rewritten"
    status: complete
    verification: "New test in `crates/code-graph-graph/tests/` (or inline in `persist.rs` under `cfg(test)`) synthesizes a JSON cache document with `\\\\?\\D:\\proj\\…` planted in EVERY one of the locations enumerated in 2.1's verification (11+ assertions covering all 10 distinct fields). Test calls `Graph::load` and then walks the resulting in-memory `Graph` asserting each field is stripped. Failure of ANY one assertion fails the test — partial migration is the explicit regression target. Test produces a clear failure message naming the offending field if the assertion fails (e.g. `\"adj[...].target still contains \\\\?\\: <value>\"`)."
    depends_on: ["2.2"]
  - id: "2.4"
    title: "End-to-end consistency test: short-form path lookup works after migration"
    status: complete
    verification: "Test loads a synthesized cache with `\\\\?\\D:\\proj\\file.h:Foo` style identifiers throughout, then calls `graph.file_symbols(Path::new(r\"D:\\proj\\file.h\"))` and asserts the returned `Vec<Symbol>` is non-empty AND each symbol's `file` field equals `D:\\proj\\file.h` (no `\\\\?\\`). This proves cross-field key alignment: `files` map key matches `nodes` SymbolId prefix matches `FileEntry.symbol_ids` strings matches `Symbol.file` values. If any one location were missed in 2.1, this test fails with an empty result set even though 2.3 might pass field-by-field."
    depends_on: ["2.3"]
  - id: "2.5"
    title: "Structural verification"
    status: complete
    verification: "`cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all --check` clean; `cargo test -p code-graph-graph` green; all pre-existing `code-graph-graph` cache tests continue to pass (the migration is a no-op on already-clean caches and existing tests use already-clean fixtures). `make snapshot-clean` passes — Phase 2 touches no snapshot-generating handler."
    depends_on: ["2.4"]
---

# Phase 2: Cache migration across all GraphCache path-bearing fields

## Overview

`GraphCache` in `crates/code-graph-graph/src/persist.rs` embeds path strings in **10 distinct field locations**. The naive cache migration (strip `files` and `includes` keys only) leaves the in-memory graph in a silent inconsistent state — `files` keys are simplified but `nodes` SymbolId keys, `Symbol.file` values, `adj`/`radj` keys+`EdgeEntry.file` values, `FileEntry.symbol_ids` strings, `mtimes` keys all still carry `\\?\`. The graph returns zero symbols for every file lookup with no error.

This phase delivers a complete `simplify_cache` migration that walks every path-bearing field, plus two pinned tests: a field-by-field anti-regression (each location is asserted independently) and an end-to-end consistency check (short-form lookup against a migrated cache returns the expected symbols).

Phase 2 only touches `code-graph-graph`. No handlers change here. The migration is idempotent — running it on an already-clean cache is a no-op per `paths::simplify`'s identity behavior on non-extended paths.

## 2.1: Implement simplify_cache helper walking all 10 path-bearing locations

### Subtasks
- [ ] Add `fn simplify_cache(cache: &mut GraphCache)` as private fn in `crates/code-graph-graph/src/persist.rs`
- [ ] Rebuild `cache.files` by draining and re-inserting with `paths::simplify(&old_key)`; for each `FileEntry` value, rebuild `symbol_ids` by splitting each SymbolId on the rightmost `:`-not-part-of-`::`, simplifying the file portion, and rejoining
- [ ] Rebuild `cache.nodes` the same way: drain, simplify the SymbolId key (file-portion-only), AND mutate the `Symbol.file` field on each value
- [ ] Rebuild `cache.adj` and `cache.radj` the same way: drain, simplify the SymbolId key, AND for each `EdgeEntry` in the Vec, simplify both `entry.target` (SymbolId; file-portion-only) and `entry.file` (PathBuf)
- [ ] Rebuild `cache.includes`: drain, simplify keys, AND simplify every entry in the inner `Vec<PathBuf>`
- [ ] Rebuild `cache.mtimes`: drain, simplify keys
- [ ] Helper for SymbolId rewriting: extract a private fn `simplify_symbol_id(id: &str) -> String` that splits on the rightmost `:` not part of `::` (mirror the existing `id_to_file` contract from `code-graph-core`), simplifies the file portion via `paths::simplify`, and rejoins with the name portion. Unit test the helper directly against examples like `r"\\?\D:\a\b.rs:Foo::bar"` → `r"D:\a\b.rs:Foo::bar"`
- [ ] Helper-level unit test: build a small in-memory `GraphCache` with extended-prefix strings in every location, call `simplify_cache`, assert each field is stripped (this is the field-by-field unit test; the integration version lives in 2.3)
- [ ] Idempotency unit test: call `simplify_cache(&mut clean_cache)` and assert no field changes (by-value equality before and after)

### Notes
The 10-vs-12 count discrepancy: the design enumeration lists 10 conceptual locations, but `includes`'s inner `Vec` values and the `symbol_ids` inside `FileEntry` are nested under the top-level field, so the actual write-side work is more granular. The unit test in 2.3 will plant `\\?\` in each of the granular sub-locations to ensure coverage.

`simplify_symbol_id` deliberately mirrors `id_to_file`'s rightmost-`:`-not-part-of-`::` rule rather than inventing new splitting logic. If a future change to symbol ID format ships, both helpers need to update together. Document the dependency in the doc comment.

## 2.2: Wire simplify_cache into Graph::load (only the graph-materialization path)

### Subtasks
- [ ] Edit `crates/code-graph-graph/src/persist.rs`: in `Graph::load` (around line 178 — the primary graph-materialization load path) insert `simplify_cache(&mut cache);` immediately after `let cache: GraphCache = serde_json::from_slice(&data)?;` and before any code consuming `cache`
- [ ] Do NOT apply at `stale_paths` line 217. The `stale_paths` function only reads `cache.mtimes` to compare on-disk timestamps and returns a `Vec<PathBuf>` consumed solely for emptiness check (cache fast-path decision) in `analyze_codebase`. Running `simplify_cache` there would: (a) waste work rewriting 9 fields the function discards, and (b) risk reformatting the mtime-stat keys, where the extended-path form may actually be the form the OS needs for paths near the 260-char limit. The correct invariant: cached `\\?\` keys flow through `stale_paths` unchanged → analyze_codebase decides cache-hit-or-reindex → on hit, `Graph::load` runs and applies the migration; on miss, full re-index uses `paths::canonicalize` (no migration needed).
- [ ] Verify no other call site in the workspace deserializes a `GraphCache` directly without going through `Graph::load` — `rg 'GraphCache' crates/` should return only `persist.rs`-internal uses

### Notes
The migration runs on every `Graph::load`, not just when `\\?\` is detected. The conditional-only optimization saves nothing (`paths::simplify` is already a cheap identity on clean paths) and complicates the code path; just always run it on the materialization path.

The distinction between the two deserialization call sites (line 178 vs line 217) is semantic, not syntactic: line 178 is "load the cache to USE it as the graph"; line 217 is "load the cache to PROBE its mtimes." Only the first needs migration because only the first's output keys flow into the in-memory graph's key consistency invariants.

## 2.3: Anti-regression test: every path-bearing field is rewritten

### Subtasks
- [ ] Add `cache_migration_strips_all_path_locations` test in `crates/code-graph-graph/tests/` (new file `cache_migration.rs`) or under `persist.rs::tests`
- [ ] Construct a `GraphCache` literal (or build via the existing `Graph::save` path against a fixture) with `\\?\D:\proj\…` planted in:
  - At least one `files` map key
  - At least one entry in that `FileEntry.symbol_ids` (SymbolId string with `\\?\` prefix)
  - At least one `nodes` map SymbolId key
  - The `Symbol.file` field on that node value
  - At least one `adj` map SymbolId key
  - At least one `EdgeEntry.target` SymbolId in that vec
  - At least one `EdgeEntry.file` PathBuf in that vec
  - The same three locations inside `radj`
  - At least one `includes` map key
  - At least one inner `Vec<PathBuf>` entry in `includes`
  - At least one `mtimes` map key
- [ ] Serialize the cache to JSON, write to a tempdir, call `Graph::load`
- [ ] Walk the loaded `Graph` and assert each location is stripped — one assertion per location (~12 assertions total)
- [ ] On any failure, the assertion message names the offending location for fast diagnosis

### Notes
The test value of separate per-location assertions over a single "no `\\?\` anywhere" sweep: when a regression strikes, the test failure message tells you which field was missed. A blanket "search the whole graph for `\\?\`" test would fail just as loudly but leave the developer searching.

## 2.4: End-to-end consistency test: short-form path lookup works after migration

### Subtasks
- [ ] Add `cache_migration_preserves_cross_field_consistency` test
- [ ] Construct a cache where `files["\\\\?\\D:\\proj\\file.h"] = FileEntry { symbol_ids: vec!["\\\\?\\D:\\proj\\file.h:Foo"], ... }` and `nodes["\\\\?\\D:\\proj\\file.h:Foo"] = Symbol { file: "\\\\?\\D:\\proj\\file.h", name: "Foo", ... }` (and the equivalent for `adj`/`radj` if any edges are needed for the assertion path)
- [ ] Save to tempdir, load via `Graph::load`
- [ ] Call `graph.file_symbols(Path::new(r"D:\proj\file.h"))` — assert returns a non-empty `Vec<Symbol>` with the expected `Foo` entry
- [ ] Assert `Foo.file == "D:\\proj\\file.h"` (no `\\?\`)
- [ ] This test fails if any single location (say, the `nodes` SymbolId key) is missed even though 2.3 passes the individual field check — proves cross-field alignment

### Notes
This is the "would the user notice?" test. 2.3 is the surgical regression target; 2.4 is the user-visible outcome. Both are needed: 2.3 makes failures specific, 2.4 makes the user-experience guarantee explicit.

## 2.5: Structural verification

### Subtasks
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Run `cargo fmt --all --check`
- [ ] Run `cargo test -p code-graph-graph`
- [ ] Run the full workspace test suite: `cargo test --workspace`
- [ ] Run `make snapshot-clean`
- [ ] Manually inspect: open `crates/code-graph-graph/src/persist.rs` and verify the `simplify_cache` call is present on every `Graph::load` path

### Notes
No snapshot tests in `code-graph-tools` regenerate from Phase 2 — the cache migration only affects loaded state, not response shapes. Snapshot regenerations are entirely Phase 3 + 4 work.

## Acceptance Criteria
- [ ] `simplify_cache(&mut GraphCache)` is implemented and rewrites all 10 path-bearing locations
- [ ] `Graph::load` runs `simplify_cache` on every load path
- [ ] Anti-regression test asserts each location is stripped independently
- [ ] End-to-end consistency test proves short-form lookup works after migration
- [ ] Idempotency unit test: running `simplify_cache` twice produces byte-identical output
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
- [ ] `make snapshot-clean` passes
