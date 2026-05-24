---
title: "Phase 2 Debrief: get_orphans P0 fix — pagination + brief flag"
type: debrief
plan: PaginationOverhaul
phase: 2
phase_title: "get_orphans P0 fix — pagination + brief flag"
status: complete
created: 2026-05-07
updated: 2026-05-07
tags: [pagination, mcp, llm-optimization, scale, ue, unreal-engine]
---

# Phase 2 Debrief: get_orphans P0 fix

## Decisions Made

- **Closed the user-reported MCP token-limit failure.** `get_orphans` now returns `Page<SymbolResult>` with `limit=20` default, capped at 1000. A 50k-orphan UE result returns a 20-row page plus `total: 50000` instead of a 5 MB array — the original blocker is gone.
- **Added `brief: Option<bool>` to `GetOrphansArgs`** (default true). Replaces the prior hardcoded `brief=true`; consistent with the other symbol-list tools.
- **Harmonized `search_symbols` clamp to 1000.** Quality scanner caught that Phase 1's "byte-identical" migration deliberately skipped adding `.min(1000)` to `search_symbols`, leaving an inconsistency: an agent passing `limit=5000` would get 5000 from `search_symbols` but 1000 from `get_orphans`. Fixed in this commit (one-line change to `symbols.rs:109`).
- **Doc-comment correction:** initial commit said "Stable sort by symbol_id" — but `symbol_id` is unique by construction, so stability is irrelevant. The actual invariant is "deterministic order over a non-deterministic HashMap iteration." Reworded.
- **Test name + assertion tightening:** `orphans_limit_clamps_at_1000` originally used a 5-item fixture and only checked the echoed `limit`. With a 5-item fixture, `take(1000)` and `take(5)` are indistinguishable — so the test passed even if the clamp were deleted. Added `assert_eq!(arr.len(), 5)` so the test name's promise (clamp enforcement) matches what the test actually proves.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| `GetOrphansArgs` has `limit`, `offset`, `brief` fields wired through | Met | All three with `#[serde(default)]` |
| Handler returns `Page<SymbolResult>` with correct defaults (20/0/true) and clamping | Met | `limit.filter(\|&n\| n != 0).unwrap_or(20).min(1000)` pattern |
| Existing snapshot regenerated, results unchanged byte-for-byte aside from wrapper | Met | Same 5 entries, now sorted by `symbol_id` |
| Three new response snapshots added (page-2, brief=false, offset-beyond-total) | Met | All approved via `cargo insta accept` |
| Tools-list snapshot regenerated showing new args in `inputSchema` | Met | `brief`/`limit`/`offset` visible to agent catalog |
| Seven new unit tests in `handlers/structure.rs` | Exceeded | 8 unit tests added (the 7 from the plan plus `orphans_brief_false_includes_signature`) |
| `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace` all clean | Met | Workspace green |

## Deviations

- **In-scope spec gap fixed.** `search_symbols` clamp at 1000 wasn't in Phase 2's task list — it should have been a Phase 1 task but was deferred by the "byte-identical" goal. Including it in Phase 2 (with explicit commit-message acknowledgement) closed the design-vs-implementation gap before Phase 3 could propagate it.

## Risks & Issues Encountered

- **The search_symbols clamp inconsistency** (described above) was a real spec-compliance miss. Cost: one quality-scan finding, three lines to fix, an extra paragraph in the commit message. Cheap. The miss came from Phase 1's "no behavior change" success criterion conflicting with the design's "limit ≤ 1000 applies to all paginated tools" — both statements are true; they just contradict each other for `search_symbols`.
- **Snapshot-fixture friction:** the page-2 test required a 25-orphan fixture, which the existing test data didn't have. Implementer built `build_indexed_fixture_with_many_orphans(n)` — synthesizes `void func_NNN()` entries via the full analyze pipeline. Reusable for Phase 3's similar need (`build_indexed_fixture_with_many_file_symbols`, `build_indexed_fixture_with_high_fan`).

## Lessons Learned

- **"Byte-identical" foundation phases can hide spec gaps.** Phase 1's contract was "no behavior change," but the design's contract was "all paginated tools clamp at 1000." When a foundation phase legitimately needs to skip a contract, the *next* phase that introduces the contract for new tools must be the place to harmonize the existing tool. Don't let the contract gap survive past the second phase that touches it.
- **Test names are testable too.** `orphans_limit_clamps_at_1000` claimed to verify clamp enforcement but actually only verified echo. The 5-item fixture made the take() invisible. Lesson: when a test name says "X enforces Y," the test must construct conditions where a missing Y would change the observable result. A test that passes regardless of whether the production code does the thing is a documentation lie.
- **Quality scanner finds real bugs.** Two of three Minor findings in this phase were genuine improvements (the spec-compliance gap and the test-name-vs-assertion mismatch). The third (the doc-comment) was a polish item. Net: scanner per phase is paying for itself.

## Impact on Subsequent Phases

- **Phase 3 inherited the clamp pattern as established convention.** All three Phase 3 tools used `limit.filter(|&n| n != 0).unwrap_or(<default>).min(1000)` — copied from `get_orphans`'s established shape.
- **Phase 4 inherited the default-resolution + zero-sentinel pattern.** `max_nodes` resolution mirrors limit resolution: `max_nodes.filter(|&n| n != 0).unwrap_or(250).min(1000)`.
- **`build_indexed_fixture_with_many_orphans` set the template** for Phase 3's `build_indexed_fixture_with_many_file_symbols` and `build_indexed_fixture_with_high_fan`. Pattern: synthesize source files via templates → run the real analyze pipeline → fixture has known cardinality and realistic edges.

## Skill Opportunities

- **What you did repeatedly:** Built three fixture-generator helpers (`build_indexed_fixture_with_many_orphans`, `…_many_file_symbols`, `…_high_fan`) following the same shape — temp-dir + write template source files + run analyze + return graph.
  **Where it belongs:** A test-only crate-level builder in `crates/codegraph-tools/tests/fixtures.rs` (or similar), parameterized by symbol kind + count + edge pattern.
  **Why a skill:** Phase 3 immediately needed two more variants. Phase 4 needed one. The fourth+ paginated tool will need a fifth. Each implementer reinvents the same wheel; consolidating gives them one helper to learn.
  **Rough shape:** `fn build_fixture(spec: FixtureSpec) -> (TempDir, Graph)` where `FixtureSpec` is `{ kind: SymbolKind, count: usize, pattern: FixturePattern }` and `FixturePattern` covers `OrphanFunctions`, `MethodsInOneClass`, `HighFanInTo(symbol)`, etc.

- **What you did repeatedly:** Manually checked `git diff --stat tests/snapshots/` after each phase to verify only expected snapshots changed.
  **Where it belongs:** A small script `scripts/snapshot-audit.sh <expected-paths...>` that fails CI if `git diff --stat tests/snapshots/` shows files outside the expected set.
  **Why a skill:** The "10 untouched tools-list snapshots show zero diff" check in Phase 4 was load-bearing for confidence that no cross-tool effects leaked. Doing it by hand each phase is fragile — easy to skim the list and miss one.
  **Rough shape:** `scripts/snapshot-audit.sh response_get_orphans_default_callables tools_list_get_orphans …` exits non-zero if `git diff --name-only tests/snapshots/` contains any file not in the expected list.
