---
title: "Phase 5 Debrief: detect_cycles envelope honesty + Cycle type + per-cycle cap"
type: debrief
plan: "ResponseShapePolish"
phase: 5
phase_title: "detect_cycles envelope honesty + Cycle type + per-cycle cap"
status: complete
created: 2026-05-15
updated: 2026-05-15
tags: [mcp, pagination, ue, unreal-engine, ergonomics, hierarchy, diagrams, coupling, dependencies, fuzzy-match]
---

# Phase 5 Debrief: detect_cycles envelope honesty + Cycle type + per-cycle cap

## Decisions Made

- **`Cycle.truncated` is always emitted (no `skip_serializing_if`), per the phase Notes over the verification text (orchestrator decision after implementer escalation).** The 5.1 verification field showed a `skip_serializing_if` on `truncated` as one option AND asserted a leaf serializes to `{"files":[...]}` ("no extra fields"); the 5.1 Notes overrode that to "always emit `truncated`, matching the `Page<T>.truncated` convention for client-deserializer uniformity." These two parts of the same task doc conflicted. The implementer correctly STOPPED and flagged it rather than silently picking one; the orchestrator decided per the Notes (the more recent, reasoned decision citing the concrete `Page<T>` precedent). A leaf cycle therefore serializes to `{"files":[...],"truncated":false}` (`original_len` still skipped when `None`). The 5.1 serialization test pins the actual chosen shape.

- **The headline lie is gone.** `detect_cycles`'s tool description previously claimed `truncated` is always `false` and `next_offset` always `null`. 5.2 made the envelope honest (`truncated = (resolved_offset + emitted) < total`; `next_offset = truncated.then(|| resolved_offset + emitted)`); 5.5 deleted the false NOTE while *preserving* the still-true "byte budget does not apply; pagination is by-count" point. This was the plan's highest-priority agent-facing edit.

- **Two independent `truncated` notions, explicitly disambiguated in BOTH the description and the `max_cycle_size` schemars text.** The envelope's `truncated` (more cycle pages) and each `Cycle.truncated` (that cycle's file list was capped by `max_cycle_size`) are orthogonal. 5.3 added `max_cycle_size` (default 50, max 500, 0→default) with per-cycle truncation applied AFTER the page slice so it can never perturb the 5.2 envelope arithmetic. 5.5 reconciled a residual internal schema contradiction with a minimal `max_cycle_size` schemars tweak ("that cycle's own `Cycle.truncated` … distinct from the envelope's").

- **5.4 integration fixtures were REBUILT to drive the real pipeline (the Phase-4-debrief standing requirement, first real exercise).** The 5.2/5.3 unit helpers (`graph_with_n_cycles`, `graph_with_one_cycle_of_n_files`) construct a `Graph` via `merge_file_graph`, which bypasses the Phase-4 include-resolution filter entirely — structurally blind exactly like Phase 4's `.ini` unit test was. The 5.4 implementer recognized this and rebuilt the generators as on-disk TempDir `.h` rings driving the real `analyze_codebase` discover→parse→resolve→merge path, with each test early-asserting the expected `total` so a dropped-edge regression fails loudly instead of vacuously. This validated the "real-entry-point test" rule adopted in the Phase 4 debrief on its first phase in force.

- **Two standing process changes adopted this debrief (user-confirmed):**
  1. **`make verify` is the canonical structural gate.** Built + committed this debrief (`ce71345`). One target runs clippy(-D warnings) → `cargo fmt --all --check` → `cargo test --workspace` → `make snapshot-clean` under make's fail-fast, aborting on first failure with a non-zero exit. Replaces hand-chained `<cmd>; echo ok` verification (which masked a real failure this phase — see Risks).
  2. **Deferred scan findings are tracked in the target task's dispatch.** When the orchestrator defers a quality-scan finding to a specific later task, it now records it in that task's `code-implementer` dispatch as a "known incoming finding — you must resolve X; flagged N times by prior scans," and the eventual scan verifies the deferred finding was actually closed. Turns passive re-discovery into an explicit charter.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| `Cycle` struct with the three fields | Met | `Cycle { files: Vec<String>, truncated: bool, original_len: Option<u32> }` in `handlers/mod.rs`, `pub(super)`, `#[derive(Debug, Serialize)]`. `original_len` skip-when-None; `truncated`/`files` always emitted. |
| `detect_cycles` returns `Page<Cycle>` with honest `truncated`/`next_offset` | Met | Envelope arithmetic verified at every boundary by the W2 scan (mid-page, exact-fit, over-offset). `Graph::detect_cycles` graph-layer primitive unchanged (`Vec<Vec<PathBuf>>`); conversion is handler-only. |
| `max_cycle_size` parameter with default 50 | Met | `.filter(\|&n\| n != 0).unwrap_or(50).min(500)` — verbatim mirror of the `limit` idiom; per-cycle truncation runs after the page slice, independent of the envelope. Arg-deser tests mirror the `count_only` pattern. |
| 3 integration tests pass | Met | All three (envelope mid-page, per-cycle cap 200-file SCC, default-50) drive the real `analyze_codebase` pipeline, not a synthetic graph; each early-asserts `total` as a vacuous-pass guard. |
| Tool description rewritten — false claim removed | Met | False "truncated always false / next_offset always null" deleted with no softened residue; by-count point preserved & verified true; two-`truncated` disambiguation accurate and early; `get_orphans` resume phrasing mirrored (byte-cap caveat correctly NOT copied). |
| `total_nodes_seen` semantics unchanged | N/A | (That contract belongs to `get_class_hierarchy` / Phase 2; `detect_cycles` has no such field — phase-doc carryover wording, no work implied.) |
| clippy / fmt / `make snapshot-clean` clean | Met | Final closeout used exit-gated checks (post the masked-check fix); `cargo test --workspace` 1223 passed / 0 failed / 2 ignored. |

## Deviations

- **The 5.1 verification text vs. Notes conflict** (the `truncated` skip idiom). Resolved by orchestrator decision toward the Notes (Page<T> consistency). The verification field's `{"files":[...]}` expectation is now out of sync with the shipped `{"files":[...],"truncated":false}` — documented here so a future reader comparing the plan text to the wire shape doesn't read it as drift. Same *class* of "plan-doc internal inconsistency surfaced at implementation" as prior phases; the implementer's stop-and-flag was exactly right.

- **5.4 fixtures diverged from the unit-helper construction.** The plan implied reusing the synthetic-cycle generator pattern; the implementer correctly rebuilt it as a real-pipeline TempDir fixture because the unit helpers bypass the Phase-4 filter. This is a deviation *toward correctness* mandated by the Phase-4-debrief standing rule — not drift. Flagged so the two near-identical-looking generators (unit `merge_file_graph` vs integration TempDir) are understood as deliberately different layers.

- **`max_cycle_size` schemars text was tweaked in Task 5.5** (a description-rewrite task). Minimal and authorized (5.5's scope explicitly permitted touching the schemars text only if a residual contradiction remained). Not scope creep — it was the only way to make the tool's full JSON schema tell one consistent story about `truncated`.

- **5.5 deferred-finding recurrence.** The `detect_cycles` description contradiction was flagged by the W2, W3, AND W4 scans before its chartered fix (5.5) landed. Every deferral was correct (5.5 was the plan-authored home, harm window zero on `rust-main` mid-phase), but the finding was independently re-derived three times. Surfaced as the motivation for the new "track deferred findings in the target task's dispatch" rule.

## Risks & Issues Encountered

- **Orchestration self-defect: masked verification exit codes.** My per-wave structural checks used `cargo fmt --all --check 2>&1 | tail -1; echo fmt-ok` — the unconditional `echo fmt-ok` after `;` runs regardless of `cargo fmt`'s exit status, so a non-zero exit was reported as green. The W4 `ok_json` dedup left a stray double blank line (a real `cargo fmt --check` violation); it rode undetected across the W4 and W5 commits until the 5.5 implementer's clean `cargo fmt --all --check` surfaced it. Impact: a CI gate (`make fmt-check`) would have failed on `rust-main` had the phase shipped without the 5.5 implementer catching it. Resolution: fixed the violation (`3b6056c`), and adopted `make verify` (`ce71345`) — a single fail-fast exit-gated gate that structurally cannot be `; echo ok`-masked. This is the most important lesson of the phase: a verification step that cannot fail loudly is not a verification step.

- **Implementer rationalized a label-leak ("Phase-4 carry-forward" in source).** The 5.4 implementer wrote "Phase-4" into 10 spots (doc comments, code comments, AND three assertion failure-message strings that surface in test output) and rationalized it as "refers to the code mechanism, not the plan artifact." The standing "no phase numbers in source" rule has no such exception — "Phase-4" is a plan-phase number that rots. Caught and scrubbed pre-scan (`d29d780`), replacing every occurrence with a behavioral description ("the include-resolution filter", "through the real index pipeline"). Lesson: the standing constraint must be stated as absolute — "describes the mechanism" is NOT a carve-out; if you're tempted to write a phase number to explain *why*, describe the *what* instead.

- **Deferred-finding triple re-discovery (the description contradiction).** Not harmful (intent-blind scans re-finding an open issue is by design), but inefficient and a signal that deferral was implicit. Addressed by the new dispatch-tracking rule.

- **`detect_cycles` is by-count, not byte-budgeted — a real cross-tool inconsistency that is correct here.** Every other paginated tool in the plan threads `[response].max_bytes`; `detect_cycles` deliberately does not (design Decision 6: cycles are rare, by-count is the right unit). The risk was an implementer "fixing" this by adding `byte_budget_take` for consistency. The handler carries an explicit behavioral comment forbidding that, the 5.5 description states it, and the W2 scan verified no byte budget is threaded. Documented so Phase 6's CLAUDE.md sweep states the by-count exception explicitly.

## Lessons Learned

- **A verification step that cannot fail loudly is not a verification step.** The `; echo ok` anti-pattern is seductive because it produces a clean-looking line; it also discards the exit code that is the entire point of the check. The only safe forms are exit-gated (`<cmd> && echo CLEAN || { echo DIRTY; exit 1; }`) or a single fail-fast target (`make verify`). This generalizes beyond fmt: any chained `; echo ok` over clippy/test/snapshot is the same trap. Adopted `make verify` as the structural fix.

- **The "real-entry-point test" rule (Phase 4 debrief) paid off on its first phase in force.** A less careful implementer would have reused the `merge_file_graph` unit helpers for the 5.4 integration tests and shipped a suite structurally blind to the Phase-4 include filter — the exact failure mode the rule exists to prevent. The 5.4 implementer recognized the bypass and rebuilt against the real pipeline unprompted (the dispatch carried the rule). Standing-rule adoption is working; keep applying it to filter/resolver/pipeline tasks.

- **An implementer that stops on a *plan-internal* contradiction is as valuable as one that stops on a code bug.** The 5.1 verification-vs-Notes conflict on the `truncated` skip idiom was a documentation defect, not a code defect; the implementer halted and asked rather than guessing. Across this plan, stop-and-report has caught: a non-existent API (`kind_str(EdgeKind)`), a wrong default-string (`"include"` vs `"includes"`), a production bug invisible to its own unit test (the `.ini` filter), and now a self-contradictory task spec. The discipline's value is not bug-specific — it's "don't paper over any ambiguity."

- **A finding flagged by N independent scans is a prioritization signal even when each deferral is individually correct.** Three intent-blind scans re-deriving the same open issue is not noise — it's three independent confirmations that it's still open and load-bearing. Tracking it explicitly in the chartered task's dispatch turns that signal into a closed-loop (the eventual scan verifies the fix), instead of relying on the orchestrator's memory across waves.

- **Standing behavioral constraints must be stated as absolute, with the rationale, so they survive a clever rationalization.** "No phase numbers in source — describe the *what*, never the *which task*" needs the *why* attached (labels rot; a future reader has no plan context) precisely so an implementer can't talk itself into "but this one refers to the mechanism." The rule held for new behavioral code every phase; it only slipped where an implementer found a plausible-sounding exception.

## Impact on Subsequent Phases

- **Phase 6 is the final fan-in and inherits a substantial CLAUDE.md reconciliation backlog accumulated across all five debriefs:**
  - `get_symbol_summary` `<global>` display-vs-query asymmetry + the fact that `search_symbols(namespace="")` is "no filter," not "global-only" (Phase 1).
  - `get_class_hierarchy` has NO `direction` arg; `HierarchyNode.ref` diamond-stub contract (Phase 2).
  - `generate_diagram` shipped a *stronger* dedupe than `Designs/ResponseShapePolish` Decision 3 specified — design doc is behind the code by a user decision (Phase 3).
  - `Graph::includes` now contains ONLY edges between indexed source files (system/external/`.ini` dropped); `.ini` filter shipped *stronger* than Decision 4 (Phase 4); the cache-schema bump D10 note belongs in CLAUDE.md's Cache-invalidation section + the PR, NOT any tool description.
  - `detect_cycles` is by-COUNT (no byte budget) — an intentional cross-tool exception; the two independent `truncated` notions; `Page<Cycle>` shape (Phase 5).
  Phase 6's "Documentation read cold" lens must ensure CLAUDE.md describes the *shipped* contracts, not the design's originals.

- **Phase 6 must use `make verify`** for its structural-verification task instead of any hand-chained checks. The masked-check defect is fixed mechanically; Phase 6's closeout should be the first to call `make verify` as the gate from the start.

- **Phase 6 dispatches will carry explicit deferred-finding charters.** The standing tracking rule is in force: any finding the orchestrator deferred into Phase 6 (notably the still-outstanding one-time `Task N.N`/`Plans/` source-leak sweep, deferred since the Phase 2 debrief) is recorded in the relevant Phase-6 task's dispatch as a must-resolve item, and the closing scan verifies it.

- **Phase 6's acceptance-regression task** (the synthetic high-fanout fixture) should reuse the real-pipeline-fixture pattern 5.4 validated (TempDir + `analyze_codebase`), not a synthetic `merge_file_graph` graph — consistent with the standing real-entry-point rule and the actual UE-scale failure modes it must reproduce.

## Skill Opportunities

### 1. `make verify` — exit-gated structural gate — BUILT THIS DEBRIEF ✓

- **Status: built + committed (`ce71345`), in force from Phase 6.** Runs clippy(-D warnings) → `cargo fmt --all --check` → `cargo test --workspace` → `make snapshot-clean` under make's fail-fast; first failure aborts with a non-zero exit and that tool's output. Directly fixes the masked-check defect: one command, one exit code, no `; echo ok` surface. Verified end-to-end (exit 0 on clean tree, per-stage progress + final marker).

### 2. Deferred-finding tracking in dispatch — ADOPTED THIS DEBRIEF ✓ (process change, no artifact)

- **Status: in force from Phase 6.** When the orchestrator defers a quality-scan finding to a specific later task, the deferral is recorded in that task's `code-implementer` dispatch as an explicit "known incoming finding — flagged N times, you must resolve X" charter, and the task's closing scan verifies the finding was closed. Closes the loop on the W2/W3/W4 triple-re-discovery pattern. No standalone artifact — it is a dispatch-authoring discipline (analogous to the standing "no plan/task labels" and "real-entry-point test" rules).

### 3. Test-rewrite classification standard in quality-scan dispatch — REINFORCED (adopted Phase 4) ✓

- Ran on every Phase 5 scan; correctly classified all mechanical signature-adaptation `None` additions and the 5.1 element-shape adaptation as bucket (a), and verified each implementer's "no test asserted the lie / no existing test modified" claim independently. No false "all clean." Continue.

### 4. `make snapshot-accept FILE=<stem>` — VALIDATED (built Phase 3) ✓

- Used at 5.1, 5.3, 5.5; worked every time including the promote-one-then-gate-flags-the-sibling flow. Note for users: it requires the FULL snapshot stem (`snapshot_tools_list__tools_list_detect_cycles`), not a fragment — implementers occasionally tried a fragment first. A one-line usage hint in the target's error message ("pass the full stem, e.g. snapshot_tools_list__tools_list_<tool>") would remove that friction; minor, not blocking.

### 5. `/sdd-planner:close-phase` — STILL DEFERRED (5th phase)

- The end-of-phase status/checkbox/README dance recurred again in Phase 5 closeout (the `sed`-doesn't-persist-on-Edit-tracked-files trap continues; Edit `replace_all` remains the workaround). Flagged every debrief since Phase 1; still not prioritized. Unchanged rough shape. Recorded so it is not lost; not blocking. With only Phase 6 remaining, the marginal value of building it now is low — likely better captured as a cross-plan retro item than built mid-final-phase.

### 6. One-time `Task N.N` / `Plans/` source-leak sweep — STILL DEFERRED to Phase 6 (now with a tracked charter)

- Unchanged: a one-shot `grep -rnE '(Task [0-9]+\.[0-9]+|Phase [0-9]+|\.?plans/|Plans/Active)' crates/*/src/` remediation. The prevention rule held again in Phase 5 for *new* code (the only leak — the 5.4 "Phase-4" rationalization — was caught pre-scan), but pre-existing debt in untouched files persists. Per the new deferred-finding-tracking rule, this is now an explicit charter for the relevant Phase-6 docs/cleanup task, not just a debrief note.
