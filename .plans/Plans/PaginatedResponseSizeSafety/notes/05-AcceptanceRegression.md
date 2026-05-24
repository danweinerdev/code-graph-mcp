---
title: "Phase 5 Debrief: Acceptance regression test"
type: debrief
plan: "PaginatedResponseSizeSafety"
phase: 5
phase_title: "Acceptance regression test"
status: complete
created: 2026-05-12
---

# Phase 5 Debrief: Acceptance regression test

## Decisions Made

- **Synthetic fixture (1500 orphan C++ functions) over ripgrep dogfood.** The plan's task 5.1 spec offered both paths. Implementer picked synthetic for determinism + no submodule dependency + size predictability. N=1500 sized to guarantee byte-budget truncation at `limit=1000` against the default 102400-byte budget with headroom for path-length variation.
- **Deterministic generator, not a static `.cpp` file.** Plan offered both. 185-line generator (`mod.rs`) writes the fixture into a caller-supplied `TempDir`. Compact, byte-deterministic, easy to grow if N=1500 ever proves insufficient.
- **Empirical probe before sizing the assertion.** Implementer wrote a throwaway probe test (`tests/byte_budget_probe.rs`, deleted before commit) that ran the fixture through `analyze_codebase` + `get_orphans(limit=1000)` and printed the actual `truncated`/`next_offset`/`body.len()` values. Without this, the fixture might have been under-sized and the acceptance test would have silently passed without exercising the truncation path. The probe revealed: body=101940 bytes (just under 102400 budget), truncated=true, next_offset=Some(971). N=1500 confirmed correct.
- **Re-export `DEFAULT_RESPONSE_MAX_BYTES` from `code-graph-core`.** Pre-Phase-5 it was `pub` inside a private `mod config;` so external crates couldn't reach it. Added to the re-export list so the acceptance test uses the production constant rather than a magic-number duplicate. (`ResponseConfig` was also added in 5.2 but later dropped in polish — no consumer needed it.)
- **`const { assert!(ORPHAN_COUNT >= 1500) }` over runtime assert.** Quality scanner under `-D warnings` flagged `assertions_on_constants` lint. Conversion to `const { assert! }` is the clippy-recommended fix AND strictly stronger — a future engineer setting `ORPHAN_COUNT < 1500` now gets a build error, not a test error.
- **Page-budget assertion ceiling is `DEFAULT_RESPONSE_MAX_BYTES + ENVELOPE_HEADROOM_BYTES`** (= 102400 + 512). Conservative — the handler already reserves 512 from the per-records budget, but envelopes serialize to ~100 bytes typical. The 512-byte headroom on the test assertion catches "budget bypassed entirely" regressions without flaking on envelope-size jitter.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| Fixture guarantees truncation at limit=1000 | Met | Empirically verified: body=101940 bytes, truncated=true, next_offset=Some(971) |
| Acceptance test reproduces 74K-token failure mode | Met | Top-of-file doc names the rust-main repro: 71 files / 1759 symbols / 1031 records / 297266 chars / ~74K tokens |
| Test loops on next_offset until truncated=false | Met | Both get_orphans and search_symbols: 2-page chain `[971, 529]`, both under budget |
| Sum of per-page lens == total | Met | `971 + 529 == 1500 == total`. Explicit `assert_eq!` |
| 3 count_only smoke tests asserting < 1KB | Met | All 3 tools; body = 85 bytes (12× under 1024 ceiling) |
| `total > 0` && `results == []` && `truncated == false` && `next_offset.is_none()` | Met | All 3 count_only tests assert all 4 conditions |
| Workspace clippy + fmt clean | Met | After 5.2 polish dropping unused `ResponseConfig` re-export |

## Deviations

- **`ResponseConfig` re-export was speculative, removed in 5.2 polish.** Phase 5.2 added it alongside `DEFAULT_RESPONSE_MAX_BYTES` to the `code-graph-core` re-export list, but nothing in the workspace imported it. Speculative public API surface; dropped in the polish commit.
- **Comment in `get_orphans` paging loop initially said "Each intermediate response stays under budget"** when in fact the loop also asserted on the final page (truncated=false). 5.2 polish reworded to "Every continuation page, including the final one." Comment-only fix.
- **Plan README's Architecture section claimed ~80–100 bytes per record post-Phase-3.** Empirical probe showed ~102 bytes on a short TempDir path (paths inflate the `id` field). The plan's math was slightly low. N=1500 still works because the budget margin is loose, but the more precise estimate would have been ~95–105 bytes/record. Not a correctness issue, but worth noting for future plans.

## Risks & Issues Encountered

- **Empirical probe was a critical de-risking step.** Without it, the acceptance test could have shipped with a fixture sized just-below-truncation. The test would have passed because the first call returned `truncated=false` (no truncation needed at the size), but it wouldn't have exercised the truncation path at all — a false-green. The plan reviewer pre-flagged this risk in the plan-review pass; the implementer correctly converted the flag into a runtime probe.
- **`assertions_on_constants` lint surfaced lazily.** The fixture's `mod tests { assert!(ORPHAN_COUNT >= 1500) }` compiled fine in 5.1 because no integration test pulled it in. When 5.2 added `#[path = "..."] mod large_orphan_set;`, the test module came along and clippy fired under workspace `-D warnings`. Caught at 5.2 commit time, not 5.1. Resolved via `const { assert! }`. A reminder: test code inside fixture modules is only exercised by clippy when something pulls the module in.
- **Re-export visibility was a one-line plumbing gap.** `DEFAULT_RESPONSE_MAX_BYTES` was `pub` but the module was `mod config;` (private). The 5.2 implementer correctly diagnosed and added the re-export — minor but easy to miss.

## Lessons Learned

- **Empirical probes beat estimates for acceptance test sizing.** The pre-Phase-5 plan estimated ~90 bytes/record and N=1200 as the floor. Probe revealed 102 bytes/record and 971 records fitting under 102400 bytes — slightly different from the estimate. For acceptance tests where false-greens are catastrophic (no test, no signal, no canary), spending 5 minutes on a probe is cheap insurance.
- **The 1500-orphan fixture is reusable.** Phase 5's 5 tests (2 acceptance + 3 count_only smoke) all share the same `build_indexed_fixture` helper. Future regression tests targeting paginated tools can extend this — e.g., if a Phase 6 introduces a new paginated tool, the fixture is ready to exercise it.
- **`const { assert! }` is strictly better than `assert!` for floor guards in test infrastructure.** Compile-time enforcement of a "fixture must satisfy this invariant" rule beats a runtime test failure. Mention worth carrying forward.
- **Acceptance test top-of-file doc is the regression contract.** The 74K-token incident description in the doc comment is what future engineers will read when the test fails. Spelling out "this is the bug, this is why the test exists, the fix is to restore byte-budget enforcement, not loosen the assertion" prevents the test from being silently disabled or weakened.

## Impact on Subsequent Phases

- **This was the final phase.** Plan complete.
- The 1500-orphan fixture under `tests/fixtures/large_orphan_set/` is now project infrastructure. Future paginated-tool work should extend its tests, not start from scratch.
- The acceptance test file `byte_budget_acceptance.rs` is a pattern other plans can copy: top-of-file regression contract, shared fixture builder, paging-loop assertion, count_only smoke tests.

## Skill Opportunities

### Empirical-probe template for acceptance fixtures
- **What you did repeatedly:** Before writing the acceptance test's first assertion, ran a probe to confirm the fixture would actually trigger the failure mode the test was supposed to catch.
- **Where it belongs:** A documented recipe in CLAUDE.md's "Test conventions" section, OR a `/sdd-planner:probe-fixture` skill that generates a throwaway test, runs it, prints the observed behavior, and deletes itself.
- **Why a skill:** Acceptance tests where the fixture is mis-sized produce false-greens — the most dangerous test failure mode (silent, looks like success). A formalized probe step would catch this before commit. The cost (one extra test compile + run) is trivial compared to the cost of shipping a non-catching test.
- **Rough shape:** Input — `(fixture_builder, target_tool_call, expected_failure_mode)`. Output — printed observed values for the failure-mode signals (truncated, next_offset, body size, etc.) so the implementer can confirm or adjust before locking in assertions. Invocation — at the start of any acceptance-regression test task.

### Compile-time floor guards
- **What you did repeatedly:** Used `const { assert!(X >= Y) }` to make a fixture-invariant a compile-time check rather than a runtime test. Future plans that introduce calibration constants (a budget, a fixture size, a threshold) could use the same idiom.
- **Where it belongs:** A pattern in CLAUDE.md's test conventions section, OR a project-wide macro `floor_assert!(const_name >= floor_value)`.
- **Why a skill:** A runtime test that asserts a constant's value is duplicating the constant. Compile-time form removes the duplication AND makes the failure mode louder (build error, not test error). Pattern recurs in any acceptance test that has size/scale guard rails.
- **Rough shape:** Input — `(constant_path, floor_value)`. Output — a `const { assert!(...) }` block at the right module level. Invocation — when adding a floor guard to a fixture or scale-dependent test.
