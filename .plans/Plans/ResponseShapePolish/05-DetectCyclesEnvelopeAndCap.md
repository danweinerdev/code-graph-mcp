---
title: "detect_cycles envelope honesty + Cycle type + per-cycle cap"
type: phase
plan: ResponseShapePolish
phase: 5
status: complete
created: 2026-05-13
updated: 2026-05-15
deliverable: "`detect_cycles` returns `Page<Cycle>` (was `Page<Vec<String>>`); `Cycle { files: Vec<String>, truncated: bool, original_len: Option<u32> }` carries structured per-cycle truncation info instead of injecting a human-readable sentinel string. Envelope `truncated` and `next_offset` correctly reflect by-count pagination state. New `max_cycle_size` argument (default 50, max 500) caps per-cycle file-list length."
tasks:
  - id: "5.1"
    title: "Cycle struct + detect_cycles return type change"
    status: complete
    verification: "`crates/code-graph-graph/src/algorithms.rs` (or wherever `detect_cycles` core lives) defines `pub struct Cycle { pub files: Vec<String>, #[serde(default, skip_serializing_if = \"std::ops::Not::not\")] pub truncated: bool, #[serde(default, skip_serializing_if = \"Option::is_none\")] pub original_len: Option<u32> }` with full Serde derives. The skip-when-false bool helper is the standard `std::ops::Not::not` pattern; alternative: a small custom fn `fn is_false(b: &bool) -> bool { !b }` referenced by `skip_serializing_if`. Pick the idiom that the rest of the workspace uses (inspect existing tools for precedent — the existing `Page<T>.truncated` field uses NO skip_serializing_if, so this is a NEW pattern; document the choice). The `detect_cycles` handler-facing path returns `Vec<Cycle>` instead of `Vec<Vec<String>>`; the inner graph function `Graph::detect_cycles` MAY keep its `Vec<Vec<PathBuf>>` return shape — the conversion to `Cycle` happens in the handler. Unit test: a leaf Cycle with truncated=false and original_len=None serializes to `{\"files\":[...]}` — no extra fields visible."
  - id: "5.2"
    title: "Envelope honesty: compute truncated and next_offset correctly"
    status: complete
    verification: "`detect_cycles` handler at `crates/code-graph-tools/src/handlers/structure.rs:41-89` is updated. After `let cycles: Vec<Cycle> = ... ; let total = cycles.len() as u32;` and `let results: Vec<Cycle> = cycles.into_iter().skip(offset).take(limit).collect();`: compute `let emitted = results.len() as u32; let truncated = (resolved_offset + emitted) < total; let next_offset = if truncated { Some(resolved_offset + emitted) } else { None };`. The hardcoded `truncated: false` and `next_offset: None` at lines 85-86 are REMOVED. The byte-budget `[response].max_bytes` is NOT applied to `detect_cycles` (per design Decision 6 — by-count units, not byte-size). The envelope honesty fix is the core deliverable. Unit test: synthesize a graph with 100 cycles; call `detect_cycles(limit=10, offset=0)`; assert `results.len() == 10`, `total == 100`, `truncated == true`, `next_offset == Some(10)`. Repeat with `offset=95`; assert `results.len() == 5`, `truncated == false`, `next_offset == None`."
    depends_on: ["5.1"]
  - id: "5.3"
    title: "max_cycle_size parameter + per-cycle truncation"
    status: complete
    verification: "`detect_cycles` handler accepts `max_cycle_size: Option<u32>` (default 50, max 500). Resolution mirrors `limit`: `let resolved_max = max_cycle_size.filter(|&n| n != 0).unwrap_or(50).min(500);`. After cycles are paginated (5.2), each cycle's `files` is checked: `if cycle.files.len() as u32 > resolved_max { let original = cycle.files.len() as u32; cycle.files.truncate(resolved_max as usize); cycle.truncated = true; cycle.original_len = Some(original); }`. No human-readable sentinel string injected. Unit test: synthesize a graph with one cycle containing 100 files; call `detect_cycles(max_cycle_size=50)`; assert the response cycle has `files.len() == 50`, `truncated == true`, `original_len == Some(100)`. The handler-side `Vec<PathBuf>` → `Vec<String>` conversion uses `.to_string_lossy().into_owned()` (today's pattern at `structure.rs:59-62` — preserve)."
    depends_on: ["5.2"]
  - id: "5.4"
    title: "Integration tests: envelope honesty mid-page, per-cycle cap, 200-file SCC"
    status: complete
    verification: "Three tests in `crates/code-graph-tools/tests/detect_cycles_envelope.rs` (or existing detect_cycles test location): (a) `envelope_honesty_mid_page` — synthetic graph with 100 cycles total; call `(limit=10, offset=30)`; assert `truncated == true`, `next_offset == Some(40)`; call again with `offset=40`; assert envelope continues correctly through to end; (b) `per_cycle_cap_truncates_large_scc` — synthetic graph with one 200-file SCC; call `(max_cycle_size=50)`; assert the one cycle in results has `files.len() == 50`, `truncated == true`, `original_len == Some(200)`; (c) `per_cycle_cap_default_50` — same fixture, no `max_cycle_size` argument; assert default 50 was applied. The 200-file SCC fixture is the synthetic version of the WebRTC cycle observed on a generic UE project."
    depends_on: ["5.3"]
  - id: "5.5"
    title: "Tool description rewrite + remove the false 'truncated always false' claim"
    status: in-progress
    verification: "`detect_cycles` tool description at `server.rs:662` is rewritten. New text: \"Returns `Page<Cycle>` where each `Cycle` has `files: Vec<String>` (file paths in canonical order within the cycle), `truncated: bool`, and `original_len: Option<u32>` (present only when truncated). The byte budget at `[response].max_bytes` does NOT apply here; cycle-level pagination is by-count via `limit`/`offset`. Per-cycle file-list truncation kicks in at `max_cycle_size` (default 50, max 500); a truncated cycle reports `truncated: true` and the original file count in `original_len`. To page through cycles, raise `offset`; to see full file lists for large cycles, raise `max_cycle_size`.\" The OLD claim 'truncated is always false and next_offset is always null here' is removed entirely — that was a lie about implementation behavior. Tool-list snapshot regenerates; `cargo insta review` accepts deliberately."
    depends_on: ["5.4"]
  - id: "5.6"
    title: "Structural verification"
    status: complete
    verification: "`cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all --check` clean; `cargo test --workspace` green; `make snapshot-clean` passes; the existing `detect_cycles` tests stay green (only the type rename and envelope-honesty fields change; sort order and total computation are preserved)."
    depends_on: ["5.5"]
tags: [mcp, pagination, ue, unreal-engine, ergonomics, hierarchy, diagrams, coupling, dependencies, fuzzy-match]
---

# Phase 5: detect_cycles envelope honesty + Cycle type + per-cycle cap

## Overview

Three surgical fixes to a single handler:

1. **Envelope honesty.** Today's `detect_cycles` applies `skip(offset).take(limit)` to the result list (correct count-based pagination), but hardcodes `truncated: false` and `next_offset: None` (lie). An agent following the documented contract believes all cycles were returned when in fact most were silently dropped.

2. **Structured truncation type.** The design originally proposed injecting a human-readable `[…+N more]` sentinel string into `Vec<String>`, but that corrupts the element type — clients iterating cycles and passing entries to filesystem ops would choke. Resolution: structured `Cycle { files, truncated, original_len }`.

3. **Per-cycle cap.** A 200-file WebRTC SCC ships as a single huge `Vec<String>` today. `max_cycle_size` parameter caps it; oversized cycles report `truncated: true` and the original count.

## 5.1: Cycle struct + detect_cycles return type change

### Subtasks
- [x] Define `Cycle` struct per the verification field
- [x] Decide the `skip_serializing_if` idiom for the `truncated: bool` field — inspect the workspace for an existing helper (likely none; `Page<T>.truncated` uses no skip). Pick: (a) `std::ops::Not::not`, (b) a small private fn `fn is_false(b: &bool) -> bool { !b }`, (c) accept `false` in the JSON output (don't skip). Recommend (c) for consistency with `Page<T>` — the field is always emitted, simplifying client deserialization
- [x] Update the handler's return type plumbing: `Vec<Vec<String>>` becomes `Vec<Cycle>` at the handler-Page boundary
- [x] The inner `Graph::detect_cycles` can keep returning `Vec<Vec<PathBuf>>` (it's a graph-layer primitive; the handler converts)

### Notes
The recommendation to NOT use `skip_serializing_if` for `truncated: bool` matches the `Page<T>` convention. The slight bandwidth cost (`"truncated":false` per cycle) is negligible against the byte savings from earlier phases; consistency wins.

## 5.2: Envelope honesty: compute truncated and next_offset correctly

### Subtasks
- [x] Open `handlers/structure.rs:41-89`
- [x] Find the existing `Page<Vec<String>>` construction at lines 80-87
- [x] Replace `truncated: false` with `truncated: (resolved_offset + emitted) < total`
- [x] Replace `next_offset: None` with `next_offset: if truncated { Some(resolved_offset + emitted) } else { None }`
- [x] `emitted` is `results.len() as u32` after the `skip(offset).take(limit)` step
- [x] Verify the `[response].max_bytes` value is intentionally NOT consulted here — `detect_cycles` is by-count, not byte-size (per design Decision 6)

### Notes
The existing handler is otherwise correct — it applies `skip/take` correctly and computes `total` correctly. Only the envelope fields lied. Two-line fix.

## 5.3: max_cycle_size parameter + per-cycle truncation

### Subtasks
- [x] Add `max_cycle_size: Option<u32>` to the handler's args struct; thread through from MCP tool input
- [x] Default 50, max 500, `Some(0)` resolves to default — mirror the `limit` defaulting pattern
- [x] After the page slice, iterate `for cycle in &mut results: ...` and apply per-cycle truncation
- [x] On truncation: `cycle.files.truncate(resolved_max as usize)`, `cycle.truncated = true`, `cycle.original_len = Some(original)`. No string injected
- [x] Default `max_cycle_size = 50` is the per-design value

### Notes
The mutability requirement on `results` is small but real — `Vec<Cycle>` rather than `&[Cycle]`. The handler already builds `results` as an owned `Vec` via `.collect()`, so this is free.

## 5.4: Integration tests: envelope honesty mid-page, per-cycle cap, 200-file SCC

### Subtasks
- [x] Construct a synthetic include-cycle generator helper in the test file — takes (cycle_count, files_per_cycle) and returns a populated `Graph` with that many disjoint SCCs of the given size
- [x] `envelope_honesty_mid_page` — 100 disjoint 2-file cycles; verify paging through correctly
- [x] `per_cycle_cap_truncates_large_scc` — 1 cycle of 200 files; verify per-cycle truncation fires
- [x] `per_cycle_cap_default_50` — same fixture, no explicit `max_cycle_size`; verify default applied
- [x] Each test names the offending invariant in its assertion message

### Notes
The synthetic-cycle generator is small (~20 lines) and useful for future cycle-related tests. Worth pulling into a `tests/test_helpers.rs` or similar if not already present.

## 5.5: Tool description rewrite + remove the false 'truncated always false' claim

### Subtasks
- [x] Open `server.rs:662` (the `detect_cycles` tool description)
- [x] Rewrite to the verification field's text
- [x] Apply the Agent-facing tool descriptions lens — "raise `offset` for next page; raise `max_cycle_size` for fuller files lists in large SCCs" operationally works
- [x] `cargo insta review` for the tool-list snapshot

### Notes
This is the highest-priority Agent-facing tool descriptions edit in the whole plan — the old description actively lied about behavior. The rewrite must read coldly as honest.

## 5.6: Structural verification

### Subtasks
- [x] Run `cargo clippy --workspace --all-targets -- -D warnings`
- [x] Run `cargo fmt --all --check`
- [x] Run `cargo test --workspace`
- [x] Run `make snapshot-clean`

## Acceptance Criteria
- [x] `Cycle` struct defined with the three fields
- [x] `detect_cycles` returns `Page<Cycle>` with honest `truncated`/`next_offset`
- [x] `max_cycle_size` parameter with default 50
- [x] 3 integration tests pass
- [x] Tool description rewritten — false claim about truncated removed
- [x] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [x] `cargo fmt --all --check` clean
- [x] `make snapshot-clean` passes
