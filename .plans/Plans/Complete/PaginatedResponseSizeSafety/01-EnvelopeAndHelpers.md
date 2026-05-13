---
title: "Envelope additions, byte-budget helper, config plumbing"
type: phase
plan: PaginatedResponseSizeSafety
phase: 1
status: complete
created: 2026-05-11
updated: 2026-05-11
deliverable: "Page<T> envelope gains truncated + next_offset; byte_budget_take helper available to handlers; RootConfig.response.max_bytes loadable from .code-graph.toml (default 102400); id_to_file recovery helper available to consumers"
tasks:
  - id: "1.1"
    title: "Extend Page<T> with truncated + next_offset"
    status: complete
    verification: "Page<T> now has truncated: bool and next_offset: Option<u32> in declaration order that produces a stable JSON shape; existing serialization derives still work; doc comment about field order updated; clippy -D warnings clean"
  - id: "1.2"
    title: "Add [response] section to RootConfig"
    status: complete
    verification: "RootConfig::load parses [response].max_bytes from .code-graph.toml when present; defaults to 102400 when section is absent or key omitted; deserialization rejects negative/zero values (returns Err with context); unit tests cover (a) absent section uses default, (b) explicit override is honored, (c) malformed value (e.g. negative integer, non-integer) errors with a clear message"
    depends_on: ["1.1"]
  - id: "1.3"
    title: "Implement byte_budget_take helper"
    status: complete
    verification: "byte_budget_take<T, I> in handlers/mod.rs takes (iter, offset, limit, max_bytes) and returns (Vec<T>, total_kept, truncated, next_offset); pre-serializes each candidate with serde_json::to_string to count bytes; unit tests cover (a) page fits exactly under budget -> truncated=false, next_offset=None, (b) page overflows on second record -> truncated=true, next_offset=Some(offset+1), (c) max_bytes=0 -> empty results, truncated=true, next_offset=Some(offset), (d) iter shorter than limit -> truncated=false, next_offset=None even when budget never tested"
    depends_on: ["1.1"]
  - id: "1.4"
    title: "Implement id_to_file recovery helper"
    status: complete
    verification: "id_to_file(&str) -> &str lives in code-graph-core (with the symbol_id construction helpers); rsplit-once on the last ':' that is not part of '::'; unit tests cover (a) Unix abs path: '/a/b.rs:foo' -> '/a/b.rs', (b) Unix abs + Parent::name: '/a/b.rs:Foo::bar' -> '/a/b.rs', (c) Windows-style: 'C:\\\\a\\\\b.cs:Baz::qux' -> 'C:\\\\a\\\\b.cs' (drive-letter colon not confused with separator), (d) malformed id with no separator -> empty &str (defensive), (e) round-trip: id = format!(\"{file}:{name}\") then id_to_file(&id) == file, (f) Unix path with ':' in filename: '/project/foo:bar.rs:func' -> '/project/foo:bar.rs' (documents that the rightmost-single-colon rule is the contract, not an accident)"
    depends_on: ["1.1"]
  - id: "1.5"
    title: "Update page_parts test helper for new fields"
    status: complete
    verification: "page_parts (test_helpers in crates/code-graph-tools/src/handlers/mod.rs under cfg(test)) returns the existing (results, total, offset, limit) tuple AND a sibling accessor (page_extras or extension) exposing (truncated, next_offset); all existing call sites compile; one new snapshot consumer test asserts both accessors return matching values"
    depends_on: ["1.3"]
---

# Phase 1: Envelope additions, byte-budget helper, config plumbing

## Overview

Foundation phase. Everything downstream consumes the pieces built here. No handler is touched; no snapshot regenerates (the envelope change is additive but no handler emits the new fields yet, so existing snapshots stay byte-identical until Phase 2 wires them through).

This phase is also the only one that touches `code-graph-core` (`RootConfig` + `id_to_file`). All other phases stay inside `code-graph-tools` / `code-graph-graph`.

## 1.1: Extend Page<T> with truncated + next_offset

### Subtasks
- [x] Edit `crates/code-graph-tools/src/handlers/mod.rs` to add `truncated: bool` and `next_offset: Option<u32>` to `Page<T>`
- [x] Place new fields after the existing ones to preserve declaration order for the existing snapshot keys (insta alphabetizes, so order won't matter for snapshot output, but the doc comment about declaration order should be amended to clarify this is a stable additive change, not a reorder)
- [x] Verify `#[derive(Debug, Serialize)]` is sufficient — `Option<u32>` serializes as `null` when None; no custom Serialize needed
- [x] Update the file-level doc comment if it mentions only `{results, total, offset, limit}` so future readers see the full envelope

### Notes
The existing doc at handlers/mod.rs ~line 69 explicitly calls out that "reordering these fields is a breaking JSON change." Adding fields at the end is NOT a reorder. Phase 2 will exercise the new fields; until then they always emit as `false` / `null` on the wire and snapshots stay byte-equivalent.

## 1.2: Add [response] section to RootConfig

### Subtasks
- [x] Locate `RootConfig` in `crates/code-graph-core/` (likely `config.rs` or similar)
- [ ] Add a `response: ResponseConfig` field with `#[serde(default)]`
- [ ] Define `ResponseConfig { max_bytes: usize }` with a constructor / default that yields `102400`
- [ ] Custom `Deserialize` or `#[serde(deserialize_with = ...)]` for `max_bytes` to reject zero and negative values with a clear error
- [ ] Update `RootConfig::load` if necessary so the new section is read alongside `[discovery]`, `[parsing]`, `[cpp]`, `[extensions]`
- [ ] Wire access from `ServerInner` — confirm `RootConfig` is already cached on the server state (per CLAUDE.md: "Read once per `analyze_codebase` and cached for watch events"); add a `config().response.max_bytes` accessor or similar shortcut
- [ ] Unit tests: absent section uses default, override works, malformed errors clearly

### Notes
The `.code-graph.toml.example` update lands in Phase 4 alongside the docs sweep, not here — keeps the example coherent with the description-string rewrites. The field itself ships in Phase 1 with the default applied silently.

Cache behavior to document later (Phase 4): `[response].max_bytes` is consulted from the cached `RootConfig` on each tool call (the TOML file is NOT re-read per query). The cache is refreshed by `analyze_codebase`; the value affects response shaping only, so no `force=true` is required to apply a changed value at the next reload.

## 1.3: Implement byte_budget_take helper

### Subtasks
- [ ] Add `pub(super) fn byte_budget_take<T: Serialize, I: IntoIterator<Item = T>>(iter: I, offset: u32, limit: u32, max_bytes: usize) -> (Vec<T>, u32, bool, Option<u32>)` in `crates/code-graph-tools/src/handlers/mod.rs`
- [ ] Iterate over `iter`, pre-serializing each candidate with `serde_json::to_string`; track running byte count
- [ ] Stop on the first candidate whose serialized size + running total would exceed `max_bytes` (the candidate is NOT included); set `truncated=true`, `next_offset = Some(offset + kept_count)`
- [ ] If `limit` is reached before the budget, set `truncated=false`, `next_offset=None`
- [ ] If `iter` exhausts before either trigger, same: `truncated=false`, `next_offset=None`
- [ ] Account for the per-record overhead inside the array (comma + JSON formatting) — over-estimate slightly to leave headroom for the envelope wrapper itself; document the chosen overhead constant
- [ ] Unit tests cover all four boundary cases from `verification`

### Notes
The 4 materializing handlers (orphans, file_symbols, callers, callees) call this helper after their sort step and before constructing `Page<T>`. `search_symbols` cannot use this helper as-is because the iterator coming back from `Graph::search` is already sliced; Phase 2.5 documents the variant.

The chosen "per-record overhead" is small (~16–32 bytes) — exists to ensure envelope serialization (`{"results": [...], "total": N, "offset": M, "limit": L, "truncated": false, "next_offset": null}`) plus inter-record commas never push the total response over budget. The unit test for max_bytes=record_size_exactly should fail-safe to truncated=true rather than gambling on overhead.

## 1.4: Implement id_to_file recovery helper

### Subtasks
- [ ] Locate where `symbol_id(s)` is constructed in `code-graph-core` (CLAUDE.md anchors: "Symbol ID format" = `file:name` or `file:Parent::name`)
- [ ] Add `pub fn id_to_file(id: &str) -> &str` alongside the construction helpers; idiom: walk the string from right to left, find the rightmost `:` whose immediate next character is not also `:` (i.e., not part of `::`), and not whose immediate prev character is `:` (also not part of `::`); the file is everything to the left of that position
- [ ] Edge case: if no separator found (malformed id), return an empty `&str` defensively rather than panicking
- [ ] Edge case: Windows path `C:\Users\foo\bar.cs` — drive-letter colon at index 1 is NOT followed by another colon, so naive rightmost-`:` scan finds the correct separator since the file:name boundary colon comes after the drive-letter one
- [ ] Edge case (f): Unix filename containing `:` — rightmost-single-colon still wins; document explicitly so a future reader doesn't think this case is unhandled
- [ ] Unit tests cover all 6 cases from `verification`, including round-trip
- [ ] Document the helper in a top-of-file or rustdoc comment that names this as the public id-recovery contract used by MCP consumers

### Notes
This helper is the documented public contract that lets `SymbolResult` drop `file` in Phase 3. Agents calling MCP tools can split records' `id` themselves; the helper exists in the codebase for the same parse logic and exists in CLAUDE.md as documentation for clients.

## 1.5: Update page_parts test helper for new fields

### Subtasks
- [ ] Locate `page_parts` in `crates/code-graph-tools/src/handlers/mod.rs` (under `cfg(test)` per Researcher report)
- [ ] Either widen the tuple to `(results, total, offset, limit, truncated, next_offset)` and update all callers, OR add a sibling `page_extras` returning `(truncated, next_offset)` to avoid touching every existing test
- [ ] Picking the sibling-accessor approach keeps existing test diffs minimal
- [ ] One smoke test in `cfg(test)` that constructs a `Page<T>` with both truncated and next_offset set, calls both accessors, asserts matching values

### Notes
Test helper signature is internal — chooses ergonomics over fewer churned lines, but minimizing churn lets the snapshot regenerations in Phase 2 land in a focused diff.

## Acceptance Criteria
- [ ] `Page<T>` exposes `truncated` and `next_offset` and serializes them
- [ ] `RootConfig` loads `[response].max_bytes` with default 102400; absent section, explicit override, and malformed value all behave per spec
- [ ] `byte_budget_take` unit tests pass for all four boundary cases
- [ ] `id_to_file` unit tests pass for all five cases including Windows and round-trip
- [ ] `page_parts` (or its sibling) exposes the new fields to test code
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
- [ ] `make snapshot-clean` passes (no `*.snap.new` produced — no handler touched yet)
