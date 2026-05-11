---
title: "Retrospective: CSharpJavaSupport Plan Complete"
type: retro
status: draft
created: 2026-05-11
updated: 2026-05-11
tags: [language-plugin, c-sharp, java, multi-language, planner-improvements]
related:
  - Plans/Complete/CSharpJavaSupport
  - Plans/Complete/CSharpJavaSupport/notes/01-Pre-Work.md
  - Plans/Complete/CSharpJavaSupport/notes/02-CSharp-Plugin.md
  - Plans/Complete/CSharpJavaSupport/notes/03-Java-Plugin.md
  - Plans/Complete/CSharpJavaSupport/notes/04-Integration-And-Cutover.md
  - Plans/Complete/RustRewrite
  - Retro/2026-05-07-rust-rewrite-complete.md
---

# Retrospective: CSharpJavaSupport Plan Complete

Four phases, 21 tasks, 31 commits, ~4 calendar days, ~395 net new tests. The MCP binary grew from 4 → 6 supported languages with zero regressions, zero workspace gate failures at close, and two new dogfood baselines (efcore @ 9184 symbols, commons-lang @ 4598).

## What Went Well

- **Carry-forward pattern was the single highest-leverage practice.** Phase 3 ran ~50% faster per-task than Phase 2 because every Wave 2 lesson threaded forward into the Java briefs. The records-leak bug (orphan methods inside C# records, caught by 2.2's quality scan and fixed in `0cf200b`) was *prevented at implementation time* in Java 3.2 — the implementer applied the lesson preemptively. Validated 8x across this plan + Phase 7 of RustRewrite.
- **Per-task quality scan caught real bugs, not just doc nits.** Eight findings worth fixing landed during the plan: records-leak (Wave 2), `nameof` filter (Wave 3 C#), `alias_qualified_name` silent-skip (Wave 4 C#), self-contradicting test comment (Wave 5 Java), name-pinning gap on Broken.java (Wave 6 Java), stale tool description (Phase 4.4 close), generic-class hierarchy lookup asymmetry doc (Wave 5 C#), and a handful of stale phase-status comments. Most were caught before they could surface in dogfood or integration.
- **Batched-doc-update directive eliminated commit churn.** Every C# and Java task that wanted to update CLAUDE.md's count language ("init all six", "all four languages") deferred correctly to Phase 4.4. The single-task documentation batch produced ~150 lines of new CLAUDE.md content in two commits (`447dcc5` fmt sweep + `59e06c7` docs) instead of 13 scattered partial-updates. Reviewers can read one diff.
- **5-way collision regression as a load-bearing test.** The `(Language, name)` SymbolIndex contract was validated by 25 assertions (5 positive + 20 negative). The asymmetric pattern (positive AND negative per pair) is what makes it load-bearing — without negatives, a regression that allowed cross-language collision would still pass the positive halves.
- **Run-and-record dogfood baselines worked cleanly.** Neither efcore nor commons-lang baselines were pre-guessed; both were measured on the first dogfood run and committed with `symbols: N, tag: ..., commit: ...` headers. ~8 seconds wall-time for efcore shallow-clone + parse. Auto-skip-on-missing-submodule (`eprintln!` + `return`, no `#[ignore]`) means base CI doesn't pay the cost.
- **Convergent design across languages.** C# 2.2 and Java 3.2 independently arrived at the same Decision 11 rule (body-presence discriminator subsumes `default`/`static`/`private` modifier check). The Java implementer corrected the brief's modifier-check wording on the fly. Two independent paths landing on the same rule is a strong signal that the rule is right.
- **Zero `unsafe`, zero `#[allow(dead_code)]` at plan close.** Both C# and Java crates ship with no suppressions remaining. Each task's removal of one suppression per field was incrementally satisfying and proved the wiring matched intent.
- **The "all-eight-submodules" dogfood ritual works end-to-end.** `make submodules` initializes all 8 in one command; per-language baselines are auto-discoverable; CI without the submodules still passes via the auto-skip pattern.
- **CLAUDE.md project-specific lenses ("Agent-facing tool descriptions", "Documentation read cold") caught real issues.** Both lenses fired during quality scans across the plan; both identified shipped bugs that the standard 5 lenses wouldn't have. The pattern from the Phase 7 RustRewrite retro extends to follower plans.

## What Could Be Improved

- **Brief-vs-shipped-state drift was the single largest source of review-fix cycles.** Every wave (1–6) caught at least one drift item: `pub const DEFINITION_QUERY` vs `pub(crate) const DEFINITION_QUERIES`, the records-leak omission, stale phase-status comments, line-number references that shifted, the `analyze_codebase` tool description listing 4 languages. Total ~13 review-fix interventions across 21 tasks (~62%). All were caught and fixed pre-merge, but they added orchestration overhead. **A `/planner:refresh-brief` slash command run before each `/implement` dispatch would prevent ~80% of these.**
- **Worktree isolation broke from Wave 2 onward.** Wave 1's parallel dispatch (2.1 + 3.1) worked. Wave 2's (2.2 + 3.2) hit a sandbox-mode issue where worktrees were created off the default branch (`main`, the Go branch) and `git reset --hard rust-main` was blocked. Both Wave 2 agents completed without committing; both worktrees auto-cleaned. The pattern was abandoned for Phases 2-3 onward. Sequential dispatch was the right fallback, but the plan's "validates parallel dispatch" experimental goal was only partially confirmed (Wave 1 only). The Agent tool's `isolation: "worktree"` parameter doesn't accept a base-branch argument; documented limitation worth surfacing.
- **Plan-level snapshot-churn prediction was wrong.** Phase 4.3's brief anticipated "10 new `.snap` files" from registration. Reality: zero — the snapshot tests construct fixtures with explicit-language registries, not registry-walking. A 5-minute spike (reading one snapshot test setup) would have caught this in plan review. The plan's Risks section listed grammar-compat as the load-bearing prediction; the snapshot prediction was unverified and turned out wrong.
- **Phase 4.4 brief didn't include "update agent-facing tool descriptions."** The implementer correctly flagged the stale `analyze_codebase` description; the orchestrator caught it because the project-specific lens was applied during review. Without the lens, the gap would have shipped. The enum-extension checklist (proposed in the Phase 1 retrospective) needs to explicitly include this item.
- **Phase-status doc sweep was a recurring catch.** Every C# task (2.2–2.6) and every Java task (3.2–3.6) had at least one stale phase-status comment caught by review. Pattern: comments said "Phase X wires Y" in present-tense when shipped or "Phase X will land Y" in future-tense after landing. The sweep is mechanical; a `make doc-stale-check` Makefile target or a CLAUDE.md lens checklist item would automate it.
- **Forward-references to Phase 4.4 worked but required tracking.** C# 2.5 wrote "Phase 4.4's CLAUDE.md `## C# Parser Limitations` section documents this for agent-facing visibility" at a point when that section didn't exist. Same in Java 3.5. The reference chain held — Phase 4.4 did add both sections — but if Phase 4.4 had been deferred, the forward-references would have rotted. The plan template should track these obligations explicitly (proposed: a `documentation_obligations:` field).
- **Apache commons-lang tag-naming convention** (`LANG_3_X_X` → `rel/commons-lang-X.Y.Z` at v3.10+) caught us off-guard. The plan said "latest stable `LANG_3_X_X` tag"; reality was that those tags only exist up to 3.8.1. Implementer correctly investigated and pinned `rel/commons-lang-3.20.0`. **Lesson:** for upstream repos, never assume the tag pattern; the plan review should `git ls-remote` and confirm before pinning.

## Action Items

- [ ] **Implement `/planner:refresh-brief`** — validated 4x now (Phase 1 retro, Phase 7 of RustRewrite, Phase 2 of CSharpJavaSupport, Phase 3 of CSharpJavaSupport). Highest-leverage planner-plugin improvement. Inputs: `<plan>/<phase-doc-path>`. Reads the phase doc + prior debriefs + plan README + design. Invokes `planner:plan-reviewer` with shipped-state context. Outputs: Approve/Revise verdict with specific line/section numbers for drift.
- [ ] **Implement `/planner:carry-forward`** — validated 8x. Inputs: `<plan>/<phase>/<task-id>`. Reads the prior task's quality-scanner findings. Extracts Critical/Major/Minor/Question. Emits an "Address these" subtask block for the next task's brief.
- [ ] **Add a CLAUDE.md "When adding a language" subsection** with the enum-extension checklist:
  1. Match arms (covered by `#[non_exhaustive]` + `_ => ...`)
  2. String-to-enum mappers (e.g., `parse_language`)
  3. **Agent-facing description text** (search `#[tool(description=...)]` + `#[schemars(description=...)]`)
  4. Plugin-internal constants and helpers (mirror sibling-plugin conventions for naming, visibility, doc style)
  5. Match-on-node-kind helpers — include defensive `_ => {}` catch-all
- [ ] **Add `make fmt-clean` Makefile target** — runs `cargo fmt --all`, prints `git diff --stat`, opens an editor for review before commit. Codifies the "fix workspace drift" ritual instead of leaving it ad-hoc.
- [ ] **Add `documentation_obligations:` field to plan-template frontmatter** — tracks forward-references (e.g., "Phase 4.4 will add `## C# Parser Limitations` section") so the close-out can verify they were satisfied.
- [ ] **Add `predictions:` field to plan-template frontmatter** — tracks plan-level predictions (e.g., "Phase 4.3 will generate ~10 new snapshots") so the retrospective can systematically report which held and which didn't.
- [ ] **Document the Agent tool's `isolation: "worktree"` base-branch limitation** in the user's CLAUDE.md or a project-planner README — worktrees are auto-created off the repo's default branch, not the parent's HEAD. Recommend pre-creating worktrees manually for plans that need parallel dispatch.
- [ ] **Pre-flight architecture sanity check during plan review** — before approving any plan step that predicts file-count or assertion-count churn, the planner-plugin's plan-reviewer agent should verify the prediction by reading one canonical instance. Could be a 4th lens in plan-reviewer's review (alongside completeness, feasibility, conventions).

## Key Metrics

| Metric | Value | Notes |
|--------|-------|-------|
| Phases | 4 | Pre-Work, C# Plugin, Java Plugin, Integration & Cutover |
| Tasks | 21 | 2 + 7 + 7 + 4 |
| Commits on rust-main | 31 | from `c0c6517` (Phase 1 close-out, post-1.2 fix) to `0e01507` (Phase 4 final tool-description fix) |
| Plan duration | 4 calendar days | 2026-05-08 (Phase 1 dispatch) → 2026-05-11 (Phase 4 close) |
| Net new tests | ~395 | 109 in C# crate + 101 in Java + new collision tests in tools + new corpus tests + new watch tests |
| Workspace tests passing | 995 | 0 failures, 2 ignored, after Phase 4 close |
| Supported languages | 4 → 6 | +C#, +Java |
| Dogfood submodules | 6 → 8 | +efcore (v8.0.25), +commons-lang (rel/commons-lang-3.20.0) |
| Unsafe blocks (workspace) | 0 → 0 | `unsafe_code = "forbid"` lint held |
| `#[allow(dead_code)]` (new plugin crates) | 0 at close | All four query fields per plugin became live by Wave 5 |
| Review-fix cycles | ~13 | spread across 21 tasks; ~62% of tasks needed one |
| Quality-scan findings flagged | ~30 | many sub-Critical fixed inline; Critical-class bugs all caught pre-merge |
| Brief-vs-shipped-state drift items caught | 8 | naming conventions, missing query patterns, line references, tool descriptions |
| Total time for efcore shallow clone + parse | ~8 seconds | reasonable for CI inclusion |
| Workspace gates at close | all clean | build ✓, test ✓, clippy ✓, fmt ✓, audit ✓, snapshot-clean ✓, submodules ✓ |

## Skill Opportunities

### 1. `/planner:refresh-brief` (validated 4x — highest leverage)

**Pattern observed:** Brief-vs-shipped-state drift was the largest single source of review-fix cycles across this plan (~62% of tasks had at least one drift item). Phase 1's retrospective flagged this; Phase 7 of RustRewrite flagged this; Phases 2 and 3 of this plan re-flagged it. Manual workarounds (carry-forward, manually-applied brief updates) helped but were inconsistent.

**Home:** New `/planner:refresh-brief <plan>/<phase>` slash command.

**Why a skill:** Eliminates the most common review-fix-cycle class. The 5-minute cost of refreshing a brief against shipped state saves ~30-60 minutes per affected task in re-review + fix cycles.

**Rough shape:**
- **Inputs:** `<plan-path>/<phase-doc-path>` (e.g., `Plans/Active/CSharpJavaSupport/02-CSharp-Plugin.md`)
- **Reads:** the phase doc + the prior phase's debrief (if any) + the plan README + any related design doc in `Designs/`
- **Process:** invokes `planner:plan-reviewer` against the phase doc with the prior debrief as additional context AND the codebase as reality reference
- **Output:** Approve/Revise verdict. If Revise, lists the specific phase-doc subsections that need updating (with line numbers) and *why* (e.g., "Task 2.2's verification field references `DEFINITION_QUERY` singular; shipped convention uses `DEFINITION_QUERIES` plural — see `crates/code-graph-lang-python/src/queries.rs`")
- **Invocation point:** hard prerequisite before `/planner:implement` if the phase has any prior shipped tasks in the same plan

### 2. `/planner:carry-forward` (validated 8x)

**Pattern observed:** Per-task quality-scanner findings need to thread into the next task's brief to prevent the same class of bug from recurring. Done manually 8x across CSharpJavaSupport + Phase 7 of RustRewrite. The records-leak prevention in Java 3.2 (because of C# 2.2's lesson) is the canonical success story.

**Home:** New `/planner:carry-forward <plan>/<phase>/<next-task-id>` slash command.

**Why a skill:** Manual carry-forward is reliable when the orchestrator remembers; automating makes the pattern impossible to skip. Reduces deferred-rework debt to zero.

**Rough shape:**
- **Inputs:** `<plan>/<phase>/<next-task-id>` (e.g., `CSharpJavaSupport/03/3.2`)
- **Sub-requirement:** quality-scanner output must land at a known location (e.g., `<plan>/<phase>/scans/<task-id>.md`) — currently scanner output is unstructured agent text. Standardize the scanner-report format first.
- **Process:** extracts Critical/Major/Minor/Question findings from the prior task's scan; filters items already addressed in the next task's draft brief; emits a numbered "Address these" subtask block.
- **Output:** Markdown block to paste at the top of the next task's brief.
- **Invocation point:** between any two sequential tasks where the prior task had non-empty scan findings.

### 3. Enum-extension checklist (validated 2x — propose as CLAUDE.md subsection)

**Pattern observed:** Both the C# 2.3 review (`8a2cde2`) and Phase 4.4 close-out (`0e01507`) added languages to stale `description` strings. Both were caught by the CLAUDE.md "Agent-facing tool descriptions" lens during review, not by the implementer's brief. The naming-convention drift (singular vs plural query constants, `pub` vs `pub(crate)` visibility) caught in Wave 1 falls under the same pattern.

**Home:** New CLAUDE.md subsection titled "When adding a language" (lives in the project's CLAUDE.md, not the planner-plugin).

**Why a skill:** Future language plugin additions (Kotlin, Scala, Swift, etc.) will recur. The checklist makes the surface-area enumeration mechanical.

**Rough shape:** Five-item checklist embedded in CLAUDE.md:
1. Match arms — covered by `#[non_exhaustive]` + `_ => ...` (no action needed)
2. String-to-enum mappers (`parse_language` and equivalents) — update + extend the `_handles_all_<plural>` test
3. **Agent-facing description text** — grep for `#[tool(description=...)]` + `#[schemars(description=...)]` + any module-doc references to "all N languages"; update wherever it appears; refresh the corresponding insta snapshots
4. Plugin-internal constants — mirror sibling-plugin naming (plural `DEFINITION_QUERIES`, `pub(crate)` visibility, etc.)
5. Match-on-node-kind helpers — include defensive `_ => {}` catch-all against future grammar additions

### 4. Defensive `_ => {}` catch-all coding convention (validated 2x)

**Pattern observed:** The C# 2.4 silent-skip bug (`alias_qualified_name` not in match arm) was caught by review. The Java 3.4 implementer applied the catch-all proactively after reading the C# 2.4 review. Both cases would have shipped silent-skip bugs without the catch-all. Coding convention worth elevating.

**Home:** CLAUDE.md "Code Conventions" section.

**Why a skill:** Match-on-tree-sitter-node-kinds is a high-frequency pattern in plugin code. Grammar versions can add new node kinds. Silent-skip is too easy to ship without a catch-all.

**Rough shape:** Convention statement: "Any helper that matches on `tree_sitter::Node::kind()` MUST include a defensive `_ => ...` catch-all. The catch-all may be `{}`, `None`, or whatever the function's noop result is. This guards against grammar updates that introduce new node kinds." Could also be a clippy lint config, but a CLAUDE.md statement plus code-review attention is sufficient for now.

### 5. `make fmt-clean` Makefile target (validated 2x)

**Pattern observed:** Phase 1 flagged the workspace fmt drift (7 files); every wave's review surfaced it; every task correctly avoided fixing it; Phase 4.4 ran the batched sweep. The pattern repeats across plans — `cargo fmt --all` is one command but the workflow (run, review the diff, commit) is multi-step.

**Home:** New `make fmt-clean` Makefile target in the project.

**Why a skill:** Codifies the plan-boundary cleanup ritual. Removes the "is this drift mine?" cognitive tax for any contributor.

**Rough shape:**
```makefile
fmt-clean:
    cargo fmt --all
    @if git diff --quiet; then \
        echo "✓ No fmt drift."; \
    else \
        echo "✗ Fmt sweep touched files — review the diff and commit:"; \
        git diff --stat; \
    fi
```
Doesn't auto-commit; leaves the diff for human review. Plan-boundary task can invoke this; ad-hoc cleanup can also.

### 6. Batched-doc-update directive as plan-template convention (validated this plan across 13 sub-tasks)

**Pattern observed:** The plan deliberately deferred CLAUDE.md count-language updates ("init all six → all eight", "all four languages → all six languages") to Phase 4.4 to avoid noisy per-task partial-updates. The deferral worked: 13 sub-tasks contributed to the batched final update; only 1 stale-text inconsistency surfaced during the plan (Phase 1's `Makefile` size estimate which was outside the deferred batch).

**Home:** Plan-template documentation convention (plan-readme.md's body section).

**Why a skill:** Reduces commit churn by ~70% on cross-task documentation tables. Makes the deferred state legible to reviewers.

**Rough shape:** Wording for the plan-template Architecture section: "**Batched documentation updates:** When a multi-task phase touches the same documentation table across multiple tasks (e.g., a per-language table, a count enumeration, an [extensions] block), designate one task — typically the final phase's docs task — as the documentation-batch owner. Contributing tasks leave the doc visibly stale; the batch task fixes it all at once. Reviewers see one diff instead of N partial-updates."

### 7. Pre-flight architecture sanity check (validated 1x — speculative for now)

**Pattern observed:** Phase 4.3's plan predicted "10 new snapshots"; reality was 0. A 5-minute spike (reading one snapshot test setup) would have caught it. The plan review process currently checks completeness, feasibility, conventions, gap analysis — but doesn't verify predictions against the actual codebase architecture.

**Home:** Extend `planner:plan-reviewer` agent with a "Predictions" check.

**Why a skill:** Plan-level predictions are easy to make and hard to verify without spending implementation time. A pre-flight check catches wrong predictions early.

**Rough shape:** When `planner:plan-reviewer` agent encounters a plan claim like "Phase X will produce N files" or "Phase Y will modify Z assertions", it should spike-read one canonical instance to confirm the prediction. Output: prediction-validity in addition to the existing Approve/Revise verdict.

Speculative until the next plan that makes similar predictions; the snapshot-churn case is the only signal so far.

### 8. Plan-template `documentation_obligations:` and `predictions:` fields

**Pattern observed:** Phase 4.4 had nine documentation obligations accumulated across Phases 1-3 (CLAUDE.md sections, count-language updates, README rows, etc.). These were tracked manually in the orchestrator's running summary. Forward-references from earlier phases ("Phase 4.4 will document this") were a deferred-obligation chain that worked but required orchestrator tracking. Predictions made at plan-draft time (snapshot churn, tag naming) were hidden inside prose.

**Home:** Plan-template frontmatter schema (in `shared/frontmatter-schema.md`).

**Why a skill:** Makes obligations and predictions structured + auditable. The close-out debrief can systematically report whether each was satisfied.

**Rough shape:** Two new optional fields in plan README frontmatter:
```yaml
documentation_obligations:
  - "CLAUDE.md ## C# Parser Limitations section (Phase 4.4)"
  - "CLAUDE.md ## Java Parser Limitations section (Phase 4.4)"
  - "README.md supported-languages table 4 → 6 rows (Phase 4.4)"

predictions:
  - claim: "Phase 4.3 will produce ~10 new .snap files"
    verified: 2026-05-11
    result: "Wrong — produced 0. Snapshot tests use empty-registry construction."
  - claim: "tree-sitter-c-sharp v0.23.x is compatible with tree-sitter core 0.26"
    verified: 2026-05-08
    result: "Confirmed via probe at /tmp/ts-probe."
```

The retro can pull from these fields and report systematically.

## Takeaways

1. **Carry-forward is the single highest-leverage practice for multi-wave plans.** Phase 3 was ~50% faster per-task than Phase 2 because lessons threaded forward systematically. The 8x validation across this plan + Phase 7 of RustRewrite confirms this. `/planner:carry-forward` would automate it.

2. **Per-task quality scan is right-sized.** It caught real bugs (records-leak, silent-skips, stale tool descriptions) without slowing dispatch. The fix-cycle ceiling (max 2 per task) kept the dispatch tempo. Both higher (catch all bugs at end-of-phase) and lower (only catch Critical) cadences would be worse.

3. **The CLAUDE.md project-specific lenses are real and effective.** "Agent-facing tool descriptions" and "Documentation read cold" both caught shipped bugs that the standard 5 lenses wouldn't have. The pattern transfers across plans.

4. **Pre-flight architecture verification beats plan-time prediction.** Phase 4.3's snapshot-churn prediction was wrong; a 5-minute spike would have caught it. Plan reviews should verify predictions, not just check completeness.

5. **Convergent design across independent implementers is a strong signal.** C# 2.2 and Java 3.2 independently arrived at the body-presence Decision 11 rule. Two paths landing on the same rule is more convincing than any single implementer's argument.

6. **The "batched-doc-update directive" works.** Deferring CLAUDE.md count-language updates to a final docs task reduced commit churn by ~70% on this plan's documentation surface. Worth elevating to plan-template convention.

7. **Worktree isolation is fragile across sandbox modes.** Wave 1 worked; Wave 2+ broke. Sequential dispatch is the correct fallback when worktree isolation can't be relied on; the plan's "validates parallel dispatch" experimental goal was only partially achieved. The Agent tool's worktree-base limitation is a documented constraint, not a defect — but worth surfacing.

8. **Run-and-record dogfood baselines are the right default.** No pre-guessed numbers; measure on first run; pin with tag + commit headers. 8 seconds wall-time per submodule for shallow-clone + parse. Acceptable for any CI cadence that opts into the submodules.

9. **The plan template needs structured obligation- and prediction-tracking.** Phase 4.4's nine accumulated documentation obligations were tracked manually; the close-out worked but required vigilance. `documentation_obligations:` and `predictions:` fields would make these auditable.
