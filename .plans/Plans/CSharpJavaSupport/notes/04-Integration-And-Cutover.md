---
title: "Phase 4 Debrief: Integration and Cutover"
type: debrief
plan: "CSharpJavaSupport"
phase: 4
phase_title: "Integration and Cutover"
status: complete
created: 2026-05-11
---

# Phase 4 Debrief: Integration and Cutover

Four tasks (4.1–4.4), 5 commits, ~3 review-fix-cycle interventions. Phase 4 brought both plugins live in the binary, widened the cross-language collision regression to 5-way with 25 assertions, completed the workspace fmt sweep deferred since Phase 1, added the new `## C# Parser Limitations` + `## Java Parser Limitations` sections to CLAUDE.md, and closed out the plan with 995 workspace tests passing.

## Decisions Made

- **Registration order: append after Python (not alphabetical).** The existing four plugins register cpp → rust → go → python (not alphabetical — historical insertion order). Phase 4.1 appended csharp → java to preserve that history. Module-level doc comment lists languages in the same order: "C++, Rust, Go, Python, C#, and Java."
- **4.3 (snapshot acceptance) became a no-op verification step.** The plan README anticipated "10 total new `.snap` files" from registration. Reality: zero. The snapshot tests in `code-graph-tools/tests/snapshot_tools_list.rs` use `CodeGraphServer::new(LanguageRegistry::new())` — an empty registry that's language-agnostic. Adding plugins to the binary's registry doesn't affect snapshot output unless tool descriptions or schemas change. Phase 4.3 ran `cargo test --workspace` + `make snapshot-clean` and confirmed clean.
- **5-way collision regression: 25 assertions (5 positive + 20 negative).** Asymmetric pattern preserved per the Phase 6 debrief of RustRewrite. Two helpers factored out (`assert_isolation`, `init_id_for`) to keep the test readable. All five fixtures use bare lowercase `init` (load-bearing per the design's "Cross-Language Collision Regression Widening" section — PascalCase `Init` for C# would have broken the name-key isolation contract).
- **Workspace fmt sweep matched Phase 1 prediction.** Phase 1 debrief said "7-8 pre-existing dirty files." Fmt sweep touched exactly 7: `config.rs`, `code-graph-lang/src/lib.rs`, `code-graph-lang-cpp/tests/corpus.rs`, `discovery.rs`, `watch_{go,python,rust}_reindex.rs`. No production-code files from Phases 2 or 3 needed reformatting (those crates stayed fmt-clean throughout their own task gates).
- **Stale `analyze_codebase` tool description fixed.** Phase 4.4's implementer correctly flagged that `crates/code-graph-tools/src/server.rs:412` still listed only "C/C++, Rust, Go, Python" — agent-facing description text that pattern-matches drive tool-call decisions. Fixed in commit `0e01507`. Same lens as Wave 1's `search_symbols` fix in `8a2cde2`.
- **Architecture diagram in CLAUDE.md uses Mermaid + ASCII parallel forms.** Already had this shape from prior phases; just extended the existing ASCII diagram and Mermaid graph to add the C# and Java plugin/grammar nodes.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| Both plugins registered in `code-graph-mcp/src/main.rs` and `code-graph-parse-test/src/main.rs` | Met | Commit `ad96610`; module doc updated to "all six shipped language plugins" |
| 5-way collision regression with asymmetric (positive + negative) assertions for all 20 cross-language pairs | Met | Commit `0528c44`; 25 assertions = 5 positive + 20 negative; 2 helpers factored |
| All new `.snap` files accepted; `make snapshot-clean` clean | Met | 4.3 was a no-op verification — registration didn't trigger snapshot diffs; tool-description fix in `0e01507` did refresh one snapshot |
| README.md, CLAUDE.md, supported-languages table, `[extensions]` table, dogfood-submodules table all updated to 6 languages / 8 submodules | Met | Commit `59e06c7`; `[extensions]` was already correct from Phase 1.2; dogfood table was already correct from Phases 2.6 + 3.6 |
| `## C# Parser Limitations` and `## Java Parser Limitations` CLAUDE.md sections written | Met | Commit `59e06c7`; ~50 lines each, mirroring existing four sections' shape |
| Workspace structural gates all pass | Met | `cargo build --release` ✓, `cargo test --workspace` 995 tests ✓, clippy clean ✓, fmt clean ✓ (post-sweep), `cargo audit` ✓, `make snapshot-clean` ✓, `make submodules` ✓ |
| Phase 4 debrief at `notes/04-Integration-And-Cutover.md` | Met | This document |
| Plan status `active` → `complete`; folder moved to `Plans/Complete/CSharpJavaSupport` | (orchestrator handles after this debrief) | Pending |
| `make dashboard` regenerated if `dashboard: true` | N/A | `planning-config.json` has `dashboard: false`; nothing to regenerate |

## Deviations

- **4.3 was a no-op verification rather than the predicted 10-snapshot acceptance.** The plan README anticipated wire-format snapshot diffs from adding plugins. The snapshot tests' design (empty-registry construction in test setup) made them robust to registry changes. This is a positive deviation — less manual review work — but the plan should have caught it in design review (the snapshot tests' construction pattern was knowable without dispatching the work).
- **`analyze_codebase` tool description was stale.** 4.4 brief did NOT enumerate "update tool descriptions" — only README and CLAUDE.md. The implementer correctly flagged the gap; orchestrator addressed it in a follow-up commit (`0e01507`). The check should have been in the 4.4 brief explicitly. **Carry-forward to future plans:** any "add a language" plan needs an explicit "update agent-facing tool descriptions" subtask. The Wave 1 commit `8a2cde2` is the canonical model for this fix shape.
- **Two stale references in older content** that 4.4 did NOT fix:
  - `PLANNER_IMPROVEMENTS.md` line ~399 mentions "3-way" collision tests (now 5-way). The file is untracked in the working tree (predates this plan); flagged for future cleanup if it becomes tracked.
  - The Wave 4 docs scan (Java 3.4) noted the C# parse_to_filegraph doc-comment originally said "upcoming 3.3/3.4/3.5" — that was corrected in `fd1d30a`. No equivalent survives in 4.4.

## Risks & Issues Encountered

- **Snapshot test architectural surprise (positive).** The plan assumed wire-format snapshots would track every registered plugin. The actual architecture — explicit-registry construction in test fixtures — made the snapshots stable across registration changes. **Lesson:** when a plan predicts a documentation/snapshot churn cost, briefly verify by reading one of the affected tests rather than assuming. A 5-minute spike could have removed Phase 4.3 from the plan entirely.
- **Tool description gap caught at the last minute.** The 4.4 implementer flagged the stale `analyze_codebase` description; the orchestrator caught it because of the CLAUDE.md "Agent-facing tool descriptions" lens being applied. Without the lens, the gap would have shipped. **Lesson:** the lens-based review approach (project-specific lenses in CLAUDE.md) is working — re-validates the pattern from Phase 7 of RustRewrite.
- **No new workspace failures.** Phase 4 introduced 0 new clippy lints, 0 new fmt drift, 0 new test failures. The 5-way collision regression added 25 new assertions; all passed on first run.

## Lessons Learned

- **Per-task quality scan + carry-forward continues to work.** Phase 4 had 3 review-fix interventions (4.1 has 1 deferred Minor; 4.2 had 0; 4.3 had 0; 4.4 had 1 caught by the implementer + addressed by orchestrator). Per-task scan caught the relevant gap before plan close-out.
- **The "batched-doc-update directive" pattern was the highest-leverage planning decision.** Every C# and Java task that wanted to update CLAUDE.md's count language deferred correctly to Phase 4.4. The single-task documentation batch produced ~50 lines of new Limitations sections + ~10 lines of count-language updates in one commit (`59e06c7`), instead of 13 partial-update commits scattered across the 13 sub-tasks. Reviewers can read one commit to see the documentation cutover instead of grep-tracking 13. Ready to elevate to plan-template convention.
- **5-way collision test confirms the cross-language isolation contract.** The `(Language, name)` SymbolIndex key works as designed: 5 distinct `init` symbols (one per language) coexist with zero cross-language pollution. The asymmetric assertion pattern (positive + 4 negatives per language) is the right rigor — without negatives, a regression that allowed cross-language collision would still pass the positive halves.
- **Workspace fmt drift was a quiet liability for 6 waves.** Every wave's review surfaced it; every task correctly avoided fixing it. Phase 4.4's batched sweep eliminated it cleanly. **Mitigation for future plans:** the Phase 1 retrospective's `make fmt-clean` skill suggestion is valid — make fmt-cleanup a one-command ritual at plan boundaries, not an ad-hoc task.
- **The 4.4 brief should have included the tool-description update step.** Wave 1's `8a2cde2` set a precedent that the orchestrator forgot to thread into 4.4's enumeration. **Carry-forward:** the enum-extension checklist proposed in Phase 1's retrospective should explicitly include "agent-facing tool descriptions" as item #3 — it caught real gaps in both Wave 1 and Phase 4.

## Impact on Subsequent Phases

There ARE no subsequent phases in this plan. Phase 4 closes out the plan.

**For future plans of similar shape** (e.g., a hypothetical "KotlinScalaSupport" Phase 8 or "GoLangPort" Phase 2):
1. **Use the enum-extension checklist proactively.** Apply it to the brief BEFORE dispatching, not during review.
2. **Pre-verify the snapshot test architecture** (5 minutes of reading) before predicting snapshot churn in the plan.
3. **Batch documentation updates explicitly.** Designate one task as the doc batch owner; contributing tasks leave docs visibly stale.
4. **Make fmt-cleanup a plan-boundary task, not a task-level concern.** Or add `make fmt-clean` as a Makefile target the plan can invoke once.

## Skill Opportunities

### 1. Tool-description update step in the enum-extension checklist (validated 2x)

Wave 1's `8a2cde2` and Phase 4.4's `0e01507` both added languages to a stale `description` string. Both were caught by the CLAUDE.md "Agent-facing tool descriptions" lens during review, not by the implementer's brief. **Recommendation:** the enum-extension checklist (proposed in Phase 1 retrospective) should explicitly enumerate:
1. Match arms (covered by `#[non_exhaustive]` + `_ =>`)
2. String-to-enum mappers
3. **Agent-facing description text** (search all `#[tool(description=...)]` + `#[schemars(description=...)]` strings)
4. Plugin-internal constants and helpers (naming-convention consistency)

This checklist could live in CLAUDE.md as a "When adding a language" subsection, OR as a snippet in the plan-template.

### 2. `make fmt-clean` Makefile target (re-validated)

Phase 1 proposed this; Phase 4.4 demonstrated the value. The current `cargo fmt --all` is one command but requires reviewing the diff before committing. A `make fmt-clean` target could codify the workflow: run fmt, show the diff via `git diff --stat`, confirm only-known-drifty files were touched, then either commit-as-one or abort.

Lower priority since `cargo fmt --all` is already a single command. The value is mostly in establishing a plan-boundary convention.

### 3. Pre-flight architecture sanity check for plan steps that predict churn

Phase 4.3's plan README predicted 10 new snapshots; reality was 0. A 5-minute spike (read one snapshot test setup) would have caught this. **Recommendation:** when a plan step predicts files-to-change or assertions-to-update, the `/planner:design` or `/planner:plan` review pass should verify the prediction by reading one canonical instance. The `planner:plan-reviewer` agent already does some of this; making it explicit for "prediction" claims would catch this class.

### 4. The plan close-out steps deserve a `/planner:close-plan` skill

Phase 4.4's "close-out" subtasks (flip frontmatter, `git mv` to Complete/, regen dashboard, write final debrief) are mechanical and the same shape every plan ends with. A skill could orchestrate this. Mostly redundant if `/planner:debrief` already handles part of it; could fold into that skill instead of being its own.

### 5. Plan-template assumption-marker

The plan README at draft time made several predictions that turned out wrong (snapshot churn count, tool-description scope, Java `LANG_3_X_X` tag naming). These were correctly handled at execution time, but consistently. **Recommendation:** plan templates should have an explicit "Predictions" section with each item marked as `prediction: ...` so the close-out debrief can systematically report which held and which didn't. The current "Risks" section partially serves this purpose but isn't structured for prediction-tracking.

This is more of a plan-template improvement than a skill, but worth elevating.
