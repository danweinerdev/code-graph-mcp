---
title: "Phase 3 Debrief: List-shaped tools — get_file_symbols, get_callers, get_callees"
type: debrief
plan: PaginationOverhaul
phase: 3
phase_title: "List-shaped tools: get_file_symbols, get_callers, get_callees"
status: complete
created: 2026-05-07
---

# Phase 3 Debrief: List-shaped tools

## Decisions Made

- **Three tools paginated using the same `Page<T>` envelope** with default limit 100 (vs. orphans' 20). Rationale: file-scoped queries and BFS results are usually under 100; the higher default avoids paginating the common case.
- **`(depth, symbol_id)` ascending sort** for `get_callers`/`get_callees`. The Graph BFS returns rows in HashMap iteration order (non-deterministic across runs), so the sort is correctness-critical, not just aesthetic. Depth-first ordering also gives an agent's first page the most useful results (closest callers).
- **`get_file_symbols` empty-raw-set diagnostic preserved verbatim:** when the file has zero symbols, the existing `"no symbols found in file: <file>"` tool error fires before pagination is consulted. The post-filter empty path returns `{results: [], total: 0}` envelope. Distinguishes "wrong file path" from "filter excluded everything" — both useful agent signals.
- **`GetCallersArgs` and `GetCalleesArgs` stayed as separate structs** even after gaining identical `limit`/`offset` fields. The `JsonSchema` description text differs (direction-specific wording), and that text surfaces in the MCP tool catalog. Decision deliberately documented in the design.
- **Test-helper consolidation:** quality scanner caught that `body_text` and `page_parts` now existed in 3 separate test modules with byte-identical bodies. Extracted to `handlers/mod.rs::test_helpers` (a `pub(super)` test-only module). Three submodules now `use super::test_helpers::{body_text, page_parts}`.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| All three args structs gain `limit` + `offset` fields | Met | Plus updated `#[tool(description=…)]` for each |
| `get_file_symbols` preserves empty-raw-set tool error; introduces empty-post-filter envelope path | Met | Both paths covered by dedicated unit tests |
| `get_callers` / `get_callees` sort by `(depth, symbol_id)` ascending — verified by a unit test | Met | `pagination_orders_by_depth_then_symbol_id` for both directions |
| Five existing response snapshots regenerated and approved | Met | 3 file_symbols + 1 callers + 1 callees |
| Three new response snapshots added and approved | Met | All `*_paginated_offset` variants |
| Tools-list snapshots regenerated for all three tools | Met | New `inputSchema` properties visible in agent catalog |
| Pagination unit tests pass for each tool | Met | 25 new tests across the three tools |
| `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace` all clean | Met | 162 lib tests passing (was 137) |

## Deviations

- **Test-helper consolidation wasn't planned but happened.** The plan implicitly assumed each tool would have its own `page_parts` (Phase 2 set the precedent). Quality scanner flagged the duplication; user opted to fix in this phase rather than defer. Net: -10 lines of duplication, one shared abstraction.
- **Call-site fanout exceeded plan estimates.** The plan mentioned snapshot updates but didn't enumerate the 8 *non-snapshot* test files that needed envelope-shape updates: `integration.rs`, `mixed_language.rs`, `watch_dangling_edges.rs`, `watch_go_reindex.rs`, `watch_python_reindex.rs`, `watch_race.rs`, `watch_rust_reindex.rs`, `handlers/watch.rs`. All needed `["results"]` indexing or helper-function updates. Implementer handled it transparently; not a problem, but a sign that "wire-format change" should explicitly include "every consumer in the test tree" as a discrete sub-step.

## Risks & Issues Encountered

- **`page_parts` triplication** (described above) — Minor finding from quality scanner. Three byte-identical 7-line helpers across three modules. Easy to consolidate; the bigger risk was *future* divergence (someone adds a field to the envelope but only updates one helper).
- **No cross-tool snapshot bleed** — confirmed by `git diff --stat tests/snapshots/`. Only the 11 expected files appeared (5 modified existing + 3 new + 3 tools-list); no `get_orphans`, `search_symbols`, or `get_class_hierarchy` snapshots regenerated.

## Lessons Learned

- **Wire-format changes have a multiplier on test-file fanout.** Three production handlers + 1 args struct file changed; 8 test files needed call-site updates. Rule of thumb: when changing a tool's response shape, grep for every `parsed.as_array()` (or equivalent) on that tool's response in the entire test tree before estimating phase size. The plan's tasks are about production code; the implementation cost is dominated by test-file fanout.
- **"Sort key" deserves a named test, not just an implementation detail.** `pagination_orders_by_depth_then_symbol_id` was added per plan. Without it, a future regression that swaps the sort order to `(symbol_id, depth)` or drops the depth tiebreaker entirely would produce different pages 1+2 across runs and only show up as flaky snapshot diffs. The test makes the contract explicit.
- **Diagnostic-error wording should be treated as production contract.** The `"no symbols found in file:"` error wording was preserved byte-for-byte through this phase. Agents that pattern-match on tool-error strings (and there are some in real prompts) would have broken silently if the wording shifted to "file has no indexed symbols" or similar.

## Impact on Subsequent Phases

- **Phase 4 inherited the consolidated `test_helpers`.** Class hierarchy tests didn't need `page_parts` (different envelope shape) but did benefit from `body_text` being shared. No new helper duplication created.
- **Phase 4 inherited the per-tool `description` style** for `#[tool(description=...)]` text — Phase 3 established a pattern of "what it does + envelope shape + defaults + ceiling + when to deviate." Phase 4's `get_class_hierarchy` description and the readability-pass refinements followed the same template.

## Skill Opportunities

- **What you did repeatedly:** Updated 8 test files to thread the new envelope shape through call sites. Each update was mechanical (`.as_array()` → `["results"].as_array()`, or update a helper function once and let it propagate).
  **Where it belongs:** A `scripts/find-tool-callsites.sh <tool_name>` or a `cargo-grep` recipe that lists every test file invoking a given handler — would let an implementer scope the call-site fanout before starting the phase.
  **Why a skill:** Phase 4's class-hierarchy change had similar fanout (3 test files). For any future tool whose wire shape changes, knowing the call-site count up front prevents under-estimating the change's scope.
  **Rough shape:** `scripts/find-tool-callsites.sh get_file_symbols` greps `crates/codegraph-tools/tests/` for the tool name + handler invocation patterns; outputs a unique file list and a count.

- **What you did repeatedly:** Built test-helper functions (`page_parts`, `body_text`) in each new submodule before realizing they belonged in a shared test_helpers module.
  **Where it belongs:** Already done — `handlers/mod.rs::test_helpers`. Future paginated handlers should `use super::test_helpers::*` rather than defining their own.
  **Why a skill:** Documentation in `CLAUDE.md` ("when adding a paginated handler, use `super::test_helpers::page_parts` not a local copy") would prevent the next contributor from re-creating the duplication.
  **Rough shape:** Convention note in `CLAUDE.md` Code Conventions section.
