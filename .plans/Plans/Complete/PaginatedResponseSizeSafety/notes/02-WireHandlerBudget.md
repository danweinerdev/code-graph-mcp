---
title: "Phase 2 Debrief: Wire byte budget into the 5 paginated handlers"
type: debrief
plan: "PaginatedResponseSizeSafety"
phase: 2
phase_title: "Wire byte budget into the 5 paginated handlers"
status: complete
created: 2026-05-12
---

# Phase 2 Debrief: Wire byte budget into the 5 paginated handlers

## Decisions Made

- **Fully sequential wave execution (user-chosen).** Wave 2 had file-overlap risk on `query.rs` (2.3+2.4 both touch `callers_or_callees`) and `symbols.rs` (2.2+2.5 both touch the search/symbols handlers). User picked "all 5 fully sequential" over bundling or partial parallelism. This added wall-clock cost but gave each task its own commit boundary + quality scan + plan-task verification.
- **Single consolidated handler `callers_or_callees`** stayed intact across 2.3 and 2.4. The wiring change in 2.3 (`Direction::Callers`) covered both directions automatically since they share the function. Task 2.4 only added callee-specific tests + snapshot — no handler-body re-edit.
- **Plumbing-first commit (2.0) before behavior commits (2.1–2.5).** Threading `max_bytes: usize` through 5 handler signatures was its own atomic unit. The `let _ = max_bytes;` suppression in each handler body documented the gap with a "consumed in task 2.1+" comment. Made compile-incrementally true through 2.1–2.5.
- **`NO_BYTE_BUDGET` named sentinel** replaced 134 raw `usize::MAX` test sites after the quality scanner flagged the bare literal as unsourced. The polish commit defined `pub const NO_BYTE_BUDGET: usize = usize::MAX;` in `handlers/mod.rs` and substituted all 134 call sites in one pass. Future tests inherit the named constant.
- **`ENVELOPE_OVERHEAD_BYTES` promoted to `pub`** for integration test reuse (2.3 polish). Three test sites had been hardcoding `512 + N` with a comment naming the constant; visibility promotion makes the hardcoded literal vanish.
- **`search_symbols` is the architectural exception (2.5).** Unlike the 4 materializing handlers that own their full match set pre-pagination, `search_symbols` delegates pagination into `Graph::search` via `SearchParams { limit, offset }` and receives an already-sliced page. Used a handler-layer trim loop instead of `byte_budget_take`. This kept `Graph::search` byte-blind. Acknowledged limitation: future optimization could push byte-budget into `Graph::search` itself, but the design doc's Decision 12 punts this.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| `max_bytes` plumbed through all 5 handlers | Met | 2.0 + 131 test call sites updated; tests pass `usize::MAX` (now `NO_BYTE_BUDGET`) |
| `byte_budget_take` consumed by 4 materializing handlers | Met | get_orphans, get_file_symbols, callers_or_callees (covers both directions) |
| `search_symbols` handler-layer trim | Met | Pre-pagination `total` preserved; re-paging correctness pinned by test |
| Existing snapshots regenerate cleanly | Met | All 17 paginated-tool snapshots stable (they invoke with `NO_BYTE_BUDGET`) |
| New `*_byte_budget_truncated` snapshots added | Met | 5 new snapshots, one per tool |
| Empty-raw-set error path in `get_file_symbols` preserved | Met | Anti-regression guard test added in 2.2 |
| Sort determinism preserved across truncation | Met | Unit tests for callers + callees |
| `cargo clippy --workspace --all-targets -- -D warnings` clean | Met | After polish commits |

## Deviations

- **Plan said "no callers outside server.rs" — actually there were 13.** Task 2.0's verification field claimed handlers had no callers outside `server.rs`. The 2.0 implementer found 13 test files + `watch.rs` calling the handlers. The implementer correctly adapted: all 131 non-server.rs call sites were updated to pass `usize::MAX` (later `NO_BYTE_BUDGET`). The verification field was wrong; the implementer's adaptation was right. Worth flagging in retro.
- **Quality scanner caught two adjacent-scope items in 4.1** that retroactively applied to 2.x work: `detect_cycles` description still advertised the 4-field envelope after 5 sibling paginated tools were rewritten, AND `SearchSymbolsArgs.kind` schemars description listed 6 kinds when the top-level descriptions named 8. Neither was a Phase 2 deliverable but both surfaced as Phase 2 left them. Cleaned up in 4.1 polish.

## Risks & Issues Encountered

- **`#[allow(dead_code)]` interaction with workspace clippy.** Mid-Wave-2, one Phase 1 implementer reported "workspace clippy fails" because another task's helper was still unused. Phase 2.1 removed the `#[allow(dead_code)]` per its own doc-comment ("removed in Phase 2 once the first handler consumes the helper"). Confused other parallel agents temporarily. Sequential ordering resolved this.
- **`graph_with_layered_callees` line-number collisions** (2.4 polish) — same line within `/big.cpp` was used for `d1→d2` and `d2→d3` edges. No correctness impact (no test asserted on the lines), but diverged from the sibling builder. Polish commit interleaved line numbers (`i*2+1` and `i*2+2`).
- **`page_len` variable carried further than necessary** in `search_symbols` trim loop. Tiny over-engineering. Inlined in 2.5 polish.

## Lessons Learned

- **The user's "fully sequential" preference for same-file work** scales to ~10 tasks but is the dominant wall-clock cost on this plan. Worth re-asking on phases with heavy parallelism opportunity (Phase 4 had pure-sequential dep chain anyway).
- **Stale-finding refrain is real.** "Tool descriptions advertise 4-field envelope" surfaced in **6 separate quality scans** before Phase 4.1 fixed it. The polish-cadence pattern doesn't help here because the fix is genuinely deferred-by-design. A scan-deduplication skill could mute the noise (see Phase 1 debrief Skill Opportunities).
- **The "handler-layer trim" architectural exception for `search_symbols`** was the right call. Threading byte-budget into `SearchParams` would have been invasive for one tool's sake; trim-at-handler is local and reusable if a new search-like tool appears.
- **Per-task commit messages with `(N.M)` suffix** make `git log` a usable execution-trace of the plan. The polish suffix `(N.M polish)` distinguishes scan-driven cleanup from primary work. Pattern was self-enforcing once established in Phase 1.

## Impact on Subsequent Phases

- Phase 3 inherits the `max_bytes` parameter wiring — `count_only` becomes the next parameter to thread through (already added to Args structs in 3.1; resolved at handler entry in 3.2).
- Phase 3 inherits a sentinel-shape choice: `count_only` uses `Page { results: [], total, offset: 0, limit: 0, ... }`. The `limit: 0` is a deliberate exception to "envelope echoes resolved limit" — documented as Decision 9.
- Phase 4 inherits: 5 stale tool descriptions to rewrite (the deferred work the scanner kept flagging).
- Phase 5 inherits: 5 byte-budget snapshots to use as references for the acceptance test's expected shape.

## Skill Opportunities

### Sentinel-substitution refactor (`NO_BYTE_BUDGET` pattern)
- **What you did repeatedly:** Bulk-substituted 134 occurrences of a magic-value-as-sentinel (`usize::MAX`) with a documented named constant (`NO_BYTE_BUDGET`). Recurs whenever a refactor introduces a "no enforcement" or "default" value at many call sites.
- **Where it belongs:** A `/sdd-planner:promote-sentinel` skill OR an inline Edit recipe documented in CLAUDE.md.
- **Why a skill:** Bulk literal-substitution across 15+ files is error-prone by hand; an implementer agent took ~80 minutes on this single task. Encoding it (read literal+meaning → define const at canonical location → substitute call sites → confirm site count matches pre-substitution literal count) would compress to a fixed-cost operation.
- **Rough shape:** Input — `(literal_value, constant_name, module_path, intended_meaning)`. Output — a polish commit with the const definition + N substituted sites + verification that site count matches. Invocation — when a quality scan flags "raw literal repeated N times with same intent."

### Plumbing-first commit cadence
- **What you did repeatedly:** Phase 2.0 introduced a new parameter through 5 handler signatures as its own atomic commit, with `let _ = max_bytes;` suppressions, before 2.1+ wired the behavior. This pattern repeats whenever a multi-task refactor needs a signature change first.
- **Where it belongs:** A documented pattern in CLAUDE.md's quality lenses, OR a `/sdd-planner:plumbing` skill that generates the introduce-signature-suppress-uses-tag-future commit shape.
- **Why a skill:** Incremental compilation through the rest of the phase depends on the plumbing being clean. Doing it wrong (e.g., adding the parameter at one site and not another) breaks the build mid-phase. Formalizing the pattern enforces correctness.
- **Rough shape:** Input — `(parameter_name, type, target_functions, suppression_message)`. Output — a commit that adds the param to each function signature, updates all call sites with a default value, and inserts `let _ = <param>;` with a tagged comment. Invocation — at the start of any phase that needs to extend handler signatures.
