---
title: "Phase 4 Debrief: get_coupling + get_dependencies + .ini filter + Graph::includes widening"
type: debrief
plan: "ResponseShapePolish"
phase: 4
phase_title: "get_coupling + get_dependencies + .ini filter + Graph::includes widening"
status: complete
created: 2026-05-15
updated: 2026-05-15
tags: [mcp, pagination, ue, unreal-engine, ergonomics, hierarchy, diagrams, coupling, dependencies, fuzzy-match]
---

# Phase 4 Debrief: get_coupling + get_dependencies + .ini filter + Graph::includes widening

## Decisions Made

- **"Drop ALL unresolved includes" (user decision, mid-phase escalation).** Task 4.2's `.ini` filter, as planned, only fired on the `resolve_include() == Some` branch. The dominant real-world case — system/external/`.ini` headers are never discovered → never in the `FileIndex` → `resolve_include` returns `None` — was left unfiltered, leaking raw strings into `Graph::includes`. The user chose to drop an include edge unless it resolves to an indexed source file (resolve-miss OR resolved-to-non-source → drop). Rationale: consistent with Phase 3's "emit nothing > emit a pseudo-node" philosophy and the original generic-UE-project run (`Platform.cpp` leaking as a dependency is the same defect class). Trade accepted: `#include <vector>`, external headers, system headers no longer appear in `get_dependencies`/`get_coupling`/diagrams at all.

- **`kind = "includes"` (plural) is canonical (orchestrator decision, internal-consistency call).** Task 4.3's define-ahead-of-use placeholder for `DependencyEntry.kind` picked `"include"` (singular). `EdgeKind::Includes` serializes as `"includes"` via `#[serde(rename_all="lowercase")]` everywhere else (core locking test, parse-test, diagram tests, and 4.4's own verification text). The placeholder was corrected to the canonical plural; this preserved the test's `{file,kind,line}` shape pin rather than weakening it. No user escalation needed — one correct answer (match the established serde wire string).

- **`edge_kind_str(EdgeKind) -> &'static str` added.** The plan's 4.4 text said to call `kind_str(EdgeKind::Includes)`, but `kind_str` takes `SymbolKind`; no `EdgeKind → &'static str` helper existed in `code-graph-tools`. Added one mirroring `kind_str` (explicit per-variant arms, `#[non_exhaustive]`-safe `_ => "unknown"`), pinned by a serde-parity test.

- **`path_normalization.rs` fixture migrated Rust → C++ (orchestrator-authorized Option A, after escalation).** The PathNormalization plan's regression fixture deliberately relied on the now-fixed leak: its own doc comments admitted the `use util::helper;` Rust include was "unresolved-by-design but still populates `Graph.includes` so `get_dependencies`/`generate_diagram` return non-empty." The drop-all-unresolved fix correctly emptied those, breaking 2 tests. Rust cannot produce a *resolvable* Includes edge to an indexed sibling via the basename resolver (module paths ≠ file paths; `RustParser::resolve_include` is intentionally a no-op), so Option (i) was impossible. Switched the path-taking-handler coverage to a C++ mini-fixture (`#include "util.h"` basename-resolves to an indexed sibling). The regression assertions were *strengthened* (assert the specific resolved row/edge vs. the prior substring/non-empty checks); the `normalize_user_path`-wrap proof remains live.

- **`testdata_cpp` baseline 21 → 17 (validated, not masked).** The cpp baseline-lock edge count dropped by exactly 4 — every drop individually accounted for (`<iostream>` in main.cpp; `<string>` in engine.h/orphan.cpp/utils.h). Zero source-to-source includes lost; symbols/files unchanged. This is the baseline-protocol counterpart of deliberate snapshot acceptance, with the metric/fixture/root-cause validated in detail.

- **Per-task auto-commits + fold-quality-fixes-into-a-W-commit pattern continued** (consistent with Phases 1–3). Wave 2 ran 4.2 ∥ 4.3 truly in parallel (disjoint subsystems — indexer/watch vs handler/types); the orchestrator committed the W1 cleanup *before* dispatching the parallel wave specifically to give both agents a clean tree (avoiding a two-agents-racing-to-stage hazard).

- **Two standing process changes adopted this debrief (user-confirmed):**
  1. **Real-entry-point test requirement.** Any task adding a filter, resolver, or edge-population rule must have ≥1 test exercising the real entry point (`analyze_codebase` / `try_reindex_file`), not only a unit harness. Added to `code-implementer` dispatch prompts for such tasks; the plan readiness audit flags filter/resolver tasks whose `verification` names only unit-level checks. (Direct response to the 4.2-vs-4.5 lesson below.)
  2. **Test-rewrite classification is now standard in the quality-scan dispatch.** Whenever a commit's diff modifies existing `#[test]` bodies/assertions, the scan dispatch automatically includes the OLD-vs-NEW classification table (bucket (a) legitimate / (b) weakened-masking). Promoted from orchestrator-improvised (Phases 1–4) to always-on.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| `IncludeEntry` defined; `Graph::includes` widened; `CACHE_VERSION` bumped | Met | `IncludeEntry{path,line}`; `HashMap<PathBuf,Vec<IncludeEntry>>`; `CACHE_VERSION` 2→3 with reason-comment; a 7th call site (`detect_cycles`) the plan's enumerated-6 missed was caught by the compiler + implementer grep and handled minimally. |
| `.ini` filter at indexer (and watch reindex) drops non-source edges | Met (corrected) | Final behavior is stronger than the plan specified: drops on resolve-MISS too (the plan only specified resolve-success). Both sites textually in sync; Calls/Inherits arms byte-untouched. |
| `CouplingEntry`/`DependencyEntry`/`CouplingBoth` defined | Met | In `handlers/mod.rs`, `pub(super)`, `#[derive(Debug,Serialize)]` matching `SummaryRow`. |
| `get_coupling` emits new shapes with sequential byte-budget allocation | Met | `Page<CouplingEntry>` directional / `CouplingBoth` for `both`; incoming sized first, outgoing gets remainder − 48-byte wrapper reserve; starved-outgoing → empty page `truncated:true,next_offset:Some(0)`, true `total` preserved. |
| `get_dependencies` emits `Page<DependencyEntry>` with line numbers | Met | `{file,kind:"includes",line}`, sorted `(file,line)` asc, byte-budgeted; `normalize_user_path` wrap preserved. |
| 6 integration tests pass | Met +1 | 7 tests (the planned 6 + the folded-in `watch_reindex_applies_ini_filter` from a 4.2-scan finding). All green; (e)/(f) are the regression target the production fix had to satisfy. |
| Tool descriptions updated | Met | `get_coupling` fully rewritten (shape, sequential budget, resume contract, defaults); `get_dependencies` description (written by 4.4) verified + a non-source-filter sentence added. |
| Dogfood baselines within ±10% (or bumped with reason) | Met | fmt/curl/abseil structurally immune (baseline sums symbols, not include edges); cpp baseline bumped 21→17 with every drop accounted for. |
| clippy/fmt/`make snapshot-clean` clean | Met | Verified each wave + at close. |

Workspace at phase close: `cargo test --workspace` **1210 passed / 0 failed / 2 ignored**; cache-bump re-index path test-covered (`load_version_mismatch_returns_false`).

## Deviations

- **The `.ini` filter shipped stronger than the plan specified.** Plan 4.2 verification only described filtering on the `resolve_include()==Some` path. The shipped behavior also drops resolve-misses (the dominant real-world path). This is a deviation *toward correctness* forced by a production bug the plan's mental model didn't anticipate — and a user-authorized contract decision, not silent scope creep.

- **`path_normalization.rs` (CLAUDE.md-flagged "strongest cross-platform regression target") was modified.** Fixture migrated Rust→C++; `get_dependencies`/`generate_diagram` assertions rewritten. This is implicit-but-load-bearing scope: the fix correctly invalidated a fixture that was built on the bug. Flagged here so a future reader diffing that file does not mistake the strengthened assertions for drift — the scan confirmed all 8 arms bucket (a), four strengthened, the `normalize_user_path` guard still live.

- **`get_dependencies` tool description was written in Task 4.4, not 4.6.** 4.4 had to (the handler shape changed; the old "flat array" text would have been actively wrong). 4.6's scope shrank to `get_coupling` + verifying/reconciling 4.4's `get_dependencies` description against the later 4.2-fix (it was incomplete — no non-source-filter note — and got one surgical sentence). Same "implementer did the adjacent necessary work" pattern as Phase 1's task 1.1 sort.

- **A 7th `Graph::includes` call site beyond the plan's enumerated 6** (`detect_cycles`) — caught by compiler + grep, projected to a path-only map (cycle topology is line-agnostic). Plan under-enumeration, not drift.

- **`file_dependencies` signature was `Vec<PathBuf>` (clone-return), not the plan's stated `Option<&[PathBuf]>`.** Element-type-only change preserved the existing clone-return shape — plan-text drift, resolved without escalation.

## Risks & Issues Encountered

- **A real production bug shipped in "completed" Task 4.2 and was caught only at Task 4.5.** This is the phase's defining event. 4.2's unit test (`resolve_all_edges_drops_include_to_non_source_target`) force-constructs a synthetic `FileGraph` that puts the `.ini` in the `FileIndex`, so it exercises only the resolve-*success* branch — it is structurally incapable of seeing the resolve-*miss* leak. Task 4.5's integration test, going through the real `analyze_codebase` pipeline, hit the actual production path on the first run and failed by design. Without the 4.5 integration suite, Phase 4 would have shipped a chartered-deliverable that didn't work on the path that fires in every real codebase, with a green test suite. Resolution: implementer stopped and reported; orchestrator escalated the behavior choice to the user; user decided; corrective commit landed with the regression target migrated and the 4.5 suite as the gating proof.

- **Two implementer stop-and-report escalations, both correct.** (1) The `kind`-string contract mismatch in 4.4 (`"include"` placeholder vs `"includes"` canonical) — implementer halted rather than silently change a sibling task's pinned test. (2) The `.ini` bug + its `path_normalization.rs` consequence in 4.5 — implementer halted twice (the bug; then, after the authorized fix, the flagged-regression-target breakage) rather than weaken CLAUDE.md's load-bearing test. The stop-and-report discipline (standing constraint + escalation rules) is doing exactly what it exists for.

- **Plan verification text encoding an incomplete mental model — now 4 instances across 4 phases.** Phase 1: `search_symbols(namespace="")` as a global filter (isn't). Phase 2: `class_hierarchy("D", up)` direction arg (doesn't exist). Phase 3: dedupe on `(label,label,direction)` (produced a regression). Phase 4: `.ini` filter only on resolve-success (misses the dominant path) + `kind_str(EdgeKind)` (wrong type) + `file_dependencies` signature drift + a 7th call site. The defenses (readiness audit, implementer flagging, intent-blind scan, escalation) caught every one — but the pattern is now undeniable and motivated the "real-entry-point test" standing change.

- **Cache-schema break (D10) — the plan's only intentional one — shipped cleanly.** `CACHE_VERSION` 2→3; old caches fail the version check before deserialize and both `Ok(false)` and deserialize-`Err` fall through to full re-index via `unwrap_or(false)`. No transparent-migration shim (design forbids it). Test-covered.

- **`testdata_cpp` baseline change is the canonical "could mask a regression" move** — handled correctly: every one of the 4 dropped edges individually accounted for, cross-checked against the *unchanged* `response_get_dependencies_engine_cpp` snapshot, zero source-to-source loss. Classified bucket (a).

## Lessons Learned

- **Unit tests on a synthetic harness can be structurally blind to the production path; integration tests against the real entry point are not optional for filter/resolver/pipeline work.** This is the sharpest lesson of the entire plan so far. 4.2's unit test wasn't weak — it was *correct for what it tested*, and still could not see the bug, because the synthetic `FileGraph` it builds bypasses the discovery→FileIndex→resolve-miss path that fires in reality. Generalization adopted as a standing dispatch/audit change: any filter/resolver/edge-population task must have ≥1 test through `analyze_codebase`/`try_reindex_file`, and the readiness audit flags such tasks whose verification names only unit checks.

- **Test-rewrite classification is now a proven, load-bearing review primitive — promoted to always-on.** Across 4 phases it caught zero false "all clean" and correctly validated the highest-stakes change of the plan (a Rust→C++ migration of CLAUDE.md's flagged cross-platform regression target). Improvising it per-dispatch worked only because the orchestrator never forgot — that is luck-adjacent. It is now standard in the quality-scan dispatch whenever existing `#[test]` assertions change.

- **Stop-and-report compounds: an implementer that halts on a contract mismatch will also halt on the *consequence* of the fix.** The 4.5 implementer stopped on the bug, the orchestrator/user resolved the behavior, and the *same* implementer then stopped again on the flagged-regression-target breakage the fix caused — rather than quietly editing CLAUDE.md's load-bearing test. The discipline is not one-shot; it holds through a multi-step escalation chain. Worth preserving exactly.

- **A fixture built on a bug is a latent escalation.** PathNormalization's fixture deliberately exploited the unresolved-include leak to get non-empty `get_dependencies`/`generate_diagram` results. When Phase 4 fixed the leak, that fixture's design assumption became false and 2 load-bearing tests broke *correctly*. Lesson for future plan/fixture authors: a test fixture that relies on a known-wrong behavior to produce its signal is a debt that detonates when the wrong behavior is fixed — prefer fixtures whose signal comes from correct behavior.

- **"Pre-existing" is about origin, not impact.** The unresolved-include leak technically predated Task 4.2 (the pre-`284dbfb` loop also retained unresolved includes). It would have been easy to wave it away as out of scope. It was not — 4.2 was *chartered* to clean non-source pollution and its fix was structurally incomplete on the path that matters. Surfacing it by impact (every real codebase gets phantom deps) rather than origin was correct.

- **The orchestrator pre-committing the W1 cleanup before a parallel wave is the right hygiene.** Two parallel implementers + an uncommitted tree = a staging race. Committing the W1 quality cleanup first gave both 4.2/4.3 agents a clean base and made "stage only your explicit paths" enforceable.

## Impact on Subsequent Phases

- **Phase 5 (`detect_cycles` envelope honesty + `Cycle` type + per-cycle cap)** is the last independent phase. The two new standing process changes apply immediately: `detect_cycles` reshaping touches existing tests (test-rewrite classification now auto-included in its scans), and if Phase 5 adds any cycle-detection filtering/capping it falls under the real-entry-point-test requirement. `detect_cycles` consumes `Graph::includes` (now `Vec<IncludeEntry>`, and now containing *only* edges between indexed source files post-4.2-fix) — Phase 5 fixtures/expected cycle sets must account for the fact that system/external/`.ini` include edges no longer exist in the graph (a cycle that previously formed through a now-dropped edge will no longer be detected — likely fine since such cycles were artifacts, but Phase 5 must verify its fixtures form cycles via real source-to-source includes).

- **Phase 6 (CLAUDE.md sweep) inherits a substantial documentation reconciliation:**
  - The `get_coupling`/`get_dependencies` response-shape entries + the new `Page<CouplingEntry>`/`CouplingBoth`/`Page<DependencyEntry>` shapes must be documented in CLAUDE.md "Response shapes".
  - **The cache-schema-bump note (D10) belongs in CLAUDE.md's Cache-invalidation section + the PR description — NOT in any tool description** (the plan explicitly forbade a migration notice in agent-facing strings; Phase 6 is where the steady-state-vs-migration split gets the CLAUDE.md note).
  - The include-graph contract changed materially: `Graph::includes` now contains only edges to indexed source files (no system/external/`.ini`). CLAUDE.md's include-graph / `get_dependencies` / `get_coupling` invariants must state this.
  - The recurring stale `Task N.N` / `Plans/` source-leak sweep (deferred since Phase 2) is still outstanding — Phase 6 remains its natural home.

- **Phase 6 must reconcile the design doc with shipped reality (again).** As in Phase 3 (dedupe key) — Phase 4's `.ini` filter shipped *stronger* than `Designs/ResponseShapePolish` Decision 4 specified (drop-all-unresolved vs resolve-success-only). The design doc is now behind the code by a user-authorized decision; the Phase 6 "Documentation read cold" lens must ensure CLAUDE.md describes the *shipped* drop-all-unresolved contract, not the design's narrower original.

## Skill Opportunities

### 1. Real-entry-point test requirement for filter/resolver/pipeline tasks — ADOPTED THIS DEBRIEF ✓

- **Status: in force from Phase 5 onward** (user-confirmed). What it is: any `code-implementer` dispatch for a task that adds a filter, resolver, or edge-population rule carries an explicit requirement of ≥1 test through the real entry point (`analyze_codebase`/`try_reindex_file`), not only a synthetic-`FileGraph` unit harness; the `/implement` readiness audit flags filter/resolver tasks whose `verification` names only unit-level checks. Why: 4.2's unit test was structurally blind to the production path; the cost of that blindness was a chartered deliverable that didn't work, with a green suite. Rough shape: a standing clause in the dispatch template + a readiness-audit heuristic ("does this task's verification only name unit-level assertions for a filter/resolution change? → flag").

### 2. Test-rewrite classification standard in the quality-scan dispatch — ADOPTED THIS DEBRIEF ✓

- **Status: now always-on** (user-confirmed; was deferred in the Phase 3 debrief). When a commit's diff modifies existing `#[test]` bodies/assertions, the scan dispatch automatically includes the OLD-vs-NEW classification table (bucket (a) legitimate / (b) weakened-masking; (b) is ≥ Major regardless of suite-green). Proven across 4 phases; Phase 4 was its strongest case (validated a Rust→C++ migration of CLAUDE.md's flagged regression target).

### 3. `make snapshot-accept FILE=<stem>` — BUILT (Phase 3 debrief), HEAVILY USED ✓

- Used repeatedly across Phase 4 (4.1, 4.3, 4.4, 4.6 snapshot regens) and worked flawlessly every time, including the "promote one, the gate then flags the still-pending sibling" flow exactly as designed. No further action — recorded as a validated, paying-off skill.

### 4. `/sdd-planner:close-phase` — STILL DEFERRED (recurred a 4th time)

- The end-of-phase status/checkbox dance (phase-doc frontmatter + plan README `phases[]` + subtask checkbox bulk-toggle, with the documented `sed`-doesn't-persist-on-Edit-tracked-files trap) recurred again in Phase 4 closeout. Flagged every debrief since Phase 1; still not prioritized. Rough shape unchanged: `/sdd-planner:close-phase <plan> <phase-id>` does all three atomically via the Edit path, and moves the plan to `Complete/` if it was the final phase. Recording again so it is not lost; not blocking.

### 5. One-time `Task N.N` / `Plans/` source-leak sweep — STILL DEFERRED to Phase 6

- Unchanged from the Phase 3 debrief: a one-shot `grep -rnE '(Task [0-9]+\.[0-9]+|Phase [0-9]+|\.?plans/|Plans/Active)' crates/*/src/` remediation pass. The "no plan/task labels in source" prevention rule held again in Phase 4 (zero new-leak findings across all tasks) — but pre-existing debt in untouched files persists. Phase 6 (docs/cleanup) remains its natural home; folded into the Phase 6 impact list above.
