---
title: "PaginationOverhaul Plan Retrospective — UE-Scale Pagination Retrofit"
type: retro
status: draft
created: 2026-05-07
updated: 2026-05-07
tags: [pagination, mcp, llm-optimization, ue, single-session, plan-retro]
related:
  - Plans/PaginationOverhaul/README.md
  - Plans/PaginationOverhaul/notes/01-Shared-Envelope.md
  - Plans/PaginationOverhaul/notes/02-Get-Orphans-P0-Fix.md
  - Plans/PaginationOverhaul/notes/03-List-Shaped-Tools.md
  - Plans/PaginationOverhaul/notes/04-Tree-Shaped-And-Cutover.md
  - Designs/Pagination/README.md
---

# PaginationOverhaul Plan Retrospective — UE-Scale Pagination Retrofit

4 phases, 32 tasks, single-session dispatch via `/planner:implement`. Plan shipped end-to-end with no rollbacks. The original user-reported P0 (`get_orphans` MCP token-limit failure on Unreal Engine codebases) is closed; five tools now share the `Page<T>` envelope, one tree-shaped tool gained a `max_nodes` budget, and the C++ `CORE_API`/dllexport macro issue observed on the same generic UE project remains tracked in `project_known_gaps_unreal.md` as the natural successor plan.

## What Went Well

- **The plan shipped in a single session.** Design → plan → 4 phases → debriefs → retro all happened end-to-end. Phase 1's foundation (`Page<T>` envelope) unlocked the next three phases mechanically; no phase blocked on rework from a prior phase.
- **The P0 user-reported failure closed.** A 50k-orphan UE result that previously blew the MCP token limit now returns a 20-row page plus `total: 50000`. Verified through new `paginated_offset` snapshot tests with 25-item synthetic fixtures.
- **Quality scanner caught 3 real issues that hand-review missed**, one per code-changing phase: the `search_symbols` clamp gap (Phase 2), the `page_parts` triplication (Phase 3), and the misleading "raise via offset for high fan-in" agent guidance (Phase 4). All three were genuine bugs (spec-compliance, test-code duplication, and agent-misleading copy respectively); none would have been caught by `cargo test` or `cargo clippy`. Per-phase scanning is paying for itself.
- **Per-phase commits give clean bisectability.** 5 commits total (1 per phase + 1 for debriefs); each phase commit is independently revertable. Compared to a single-PR-style mega-commit, this preserves the "Phase 1 was deliberately byte-identical" history for future readers.
- **The snapshot zero-diff audit caught zero accidental cross-tool effects.** The strict `git add <specific files>` discipline + per-tool isolation in handlers held — none of the 10 untouched tools-list snapshots regenerated unexpectedly across any phase. This was a load-bearing assertion in Phase 4's task 4.10 and the discipline in Phases 2/3 made it pay off.
- **The diamond test was a discriminator that actually discriminated.** Plan reviewer's /plan-pass intervention reshaped the test from "passes trivially" to "fails under a naïve visit-counter" (4 unique nodes, 5 visits, `max_nodes=4`, assert `truncated=false`). When the implementer ran it, it confirmed unique-name semantics — exactly the failure mode the reviewer designed it to catch.
- **Test-helper consolidation happened mid-plan (Phase 3) rather than being deferred.** Three byte-identical `page_parts` copies were caught by quality scanner and consolidated into `handlers/mod.rs::test_helpers` in the same phase. This is an improvement over the RustRewrite retro's recurring pattern of "helper consolidations deferred twice before forcing them."
- **Per-phase debriefs written contemporaneously.** Each debrief captures the actual implementation experience (decisions, surprises, deviations) rather than a sanitized summary. The retro you're reading quotes from them rather than re-deriving the context.
- **Plan and design were both revised before /implement started.** The Critical finding from /plan's plan-reviewer pass (design self-contradiction on `max_nodes` default: 100 in the table, 250 in the rationale) was fixed in the design; the Major findings (diamond test discrimination, snapshot audit, did-you-mean preservation) were folded into Phase 4's task verifications. Implementation hit zero "the plan said X but X is wrong" stalls.

## What Could Be Improved

- **The "byte-identical foundation" goal in Phase 1 created a spec-compliance gap that only surfaced in Phase 2.** Phase 1's Acceptance Criteria explicitly forbade behavior changes, but the design specifies `limit ≤ 1000` for *all* paginated tools — including the existing `search_symbols`. Adding `.min(1000)` to `search_symbols` would have broken byte-identicality for any caller passing `limit > 1000`. The contract gap was real and was caught by quality scanner in Phase 2 (one-line fix), but it should have been an explicit Phase 2 task — not a discovery. Lesson: when a foundation phase legitimately defers a contract, the next phase that touches the area must include "harmonize the deferred contract" as a hard subtask.
- **The snapshot harness's `parsed_sorted` quirk tripped doc-comments in Phase 1 AND Phase 4.** Same trap, twice. The harness alphabetizes JSON keys before snapshotting, so snapshot files are not the wire-format-declaration-order ground truth. Both phases' initial doc-comments cited snapshots as the source-of-truth; both were caught by quality scanner. A one-paragraph note in CLAUDE.md would have prevented both. Action item below.
- **Test-file fanout in Phase 3 (8 non-snapshot test files) wasn't anticipated in plan estimates.** The plan's tasks named the production-code changes; the implementation cost was dominated by call-site updates in `integration.rs`, `mixed_language.rs`, and 6 watch tests. The implementer absorbed this transparently, but a future plan touching the wire shape of a tool used across many test files would benefit from listing call-site fanout as an explicit subtask. Heuristic: if `grep -l <tool_name> crates/*/tests/` returns more than 3 files, count them in the estimate.
- **Tool-description copy slipped through three review passes before quality scanner caught it in Phase 4.** The "raise via offset for high fan-in" wording on `get_callers`/`get_callees` was *operationally wrong* — `offset` is a skip-count, not a "give me more results" lever. It survived: the implementer's draft, the implementer's own readability pass, my coordinator review of the Phase 4 implementer report, and the workspace gates. Quality scanner caught it on a careful read of the descriptions. Lesson: agent-facing `#[tool(description=…)]` text is production behavior, not documentation, but it's reviewed like documentation. The standard quality-scan prompt should include explicit checklist items for description copy.
- **Per-phase commit messages drafted by hand 4 times despite being structurally identical.** Each followed the same template: `[PaginationOverhaul/Phase N] <subject>` + body explaining what + why + tests + snapshot delta. The phase frontmatter has the title, deliverable, and acceptance criteria already; the commit message is mostly a templated rendering of those + the snapshot delta. ~5 minutes per commit drafted by hand, 4 times. A `/planner:commit-phase` skill or `make commit-phase PHASE=N` target would render the boilerplate from the phase doc.
- **Snapshot zero-diff audit done by hand 4 times despite being load-bearing each time.** Every phase ran `git diff --stat tests/snapshots/` and visually checked that only the expected files appeared. This worked — caught zero unintended changes — but it's exactly the kind of mechanical check that should be automated. A `scripts/snapshot-audit.sh <expected-paths…>` invoked from each phase's verification step would enforce the gate.

## Action Items

- [ ] **Add a paragraph to `CLAUDE.md`** documenting the `parsed_sorted` snapshot-normalization quirk: snapshot files alphabetize JSON keys, so they cannot verify wire-format declaration order; the struct itself is the source of truth.
- [ ] **Write `scripts/snapshot-audit.sh <expected-paths…>`** that fails if `git diff --name-only crates/codegraph-tools/tests/snapshots/` contains files outside the expected list. Invoke from each phase's structural-verification subtask going forward.
- [ ] **Add `make snapshot-clean` Makefile target** that runs `cargo insta pending-snapshots` and exits non-zero if any `.snap.new` files exist. Suggest `cargo insta review` in the failure message. Wire into the pre-commit hook if one exists.
- [ ] **Extend the standard `planner:quality-scanner` prompt** with explicit checklist items for `#[tool(description=…)]` text: (a) every named arg documented with default and ceiling, (b) the verb in the suggested action operationally produces the claimed result, (c) the response envelope shape is named (not implied), (d) when an agent should pick non-default values is hinted.
- [ ] **Build a shared fixture-builder** at `crates/codegraph-tools/tests/fixtures.rs` that consolidates the three ad-hoc builders this plan added (`build_indexed_fixture_with_many_orphans`, `…_many_file_symbols`, `…_high_fan`). Parameterize by `FixtureSpec { kind, count, pattern }`. Document in `CLAUDE.md` Code Conventions.
- [ ] **Document the test_helpers convention in `CLAUDE.md`:** "When adding a paginated handler test module, `use super::test_helpers::{body_text, page_parts}` rather than defining local copies." Prevents the next contributor from re-creating the duplication that Phase 3 had to consolidate.
- [ ] **Decide on `/planner:commit-phase`** vs. a Makefile alternative for templated phase-commit messages. The phase frontmatter already has title + deliverable + acceptance criteria; the commit body is mostly a rendering of those plus the snapshot delta. ~20 minutes saved per plan.
- [ ] **Open a follow-on plan for the C++ macro-prefixed-class issue** (`class CORE_API MyClass : public UObject`) from the same generic-UE-project run that motivated this plan. Tracked in `project_known_gaps_unreal.md`; design-vs-query approach decision still open.

## Key Metrics

| Metric | Value | Notes |
|--------|-------|-------|
| Phases | 4 | All complete; status `complete` |
| Tasks | 32 | All complete (4+8+9+11) |
| Code commits | 4 | One per phase, plus 1 debrief commit |
| Plan duration | 1 session | Design → plan → 4 phases → debriefs → retro |
| Tools migrated to `Page<T>` | 5 | search_symbols, get_orphans, get_file_symbols, get_callers, get_callees |
| Tools gaining `max_nodes` budget | 1 | get_class_hierarchy |
| Tools intentionally untouched | 9 | analyze_codebase, detect_cycles, generate_diagram, get_dependencies, get_coupling, get_symbol_detail, get_symbol_summary, watch_start, watch_stop |
| Unit tests added | ~36 | Across all 4 phases |
| Snapshot tests added | 7 | 3 orphans + 1 file_symbols + 2 callers/callees + 1 hierarchy-truncated |
| Existing snapshots regenerated | 10 response + 5 tools-list | All approved via `cargo insta accept` |
| Untouched snapshots verified zero-diff | 10 tools-list | analyze_codebase, detect_cycles, generate_diagram, get_dependencies, get_coupling, get_symbol_detail, get_symbol_summary, search_symbols, watch_start, watch_stop |
| Quality scanner findings (real bugs caught) | 3 | search_symbols clamp gap, page_parts triplication, misleading "raise via offset" copy |
| Plan-vs-design contract gaps caught | 1 + 1 | search_symbols clamp gap (Phase 2 quality scan); design self-contradiction on `max_nodes` default (caught in /plan reviewer pass before /implement started) |
| Workspace test count post-plan | 726 passing | Up from ~683 pre-plan baseline |
| Rollbacks | 0 | No phase reverted; no commit amended |
| `cargo fmt --check` / `cargo clippy -D warnings` clean across all phases | Yes | No `#[allow]` attributes added to suppress findings |

## Skill Opportunities

Aggregated across the four debriefs, with strong-signal flags for patterns that appeared in more than one debrief.

### 1. `scripts/snapshot-audit.sh <expected-paths…>` (STRONG SIGNAL — flagged in Phase 2 AND Phase 4 debriefs)

- **Pattern observed:** Every phase ran `git diff --stat tests/snapshots/` by hand to verify only the expected snapshot files changed. Phase 4 specifically asserted "the 10 untouched tools-list snapshots show zero diff" as a load-bearing check. Done 4 times in this plan; will be needed every time a tool's wire shape changes in the future.
- **Home for the skill:** Shell script at `scripts/snapshot-audit.sh`, invoked from each phase's structural-verification step (and ideally a pre-commit hook).
- **Why a skill:** Catches accidental cross-tool effects (e.g. an "improvement" to a shared helper that quietly regenerates an unrelated tool's snapshot). The check is mechanical, easy to skim past, and high-consequence if missed. Doing it by hand creates room for confirmation bias ("looks right, ship it").
- **Rough shape:** `scripts/snapshot-audit.sh response_get_orphans_default_callables tools_list_get_orphans …` exits non-zero if `git diff --name-only crates/codegraph-tools/tests/snapshots/` contains any file not in the expected list. Plain bash; no dependencies.

### 2. `planner:scan-tool-descriptions` skill — agent-usability checklist for `#[tool(description=…)]` (Phase 4 debrief; structural lesson from a real bug)

- **Pattern observed:** Tool descriptions were edited across three phases (2.1, 3.1, 4.3). The "final readability pass" in 4.9 was supposed to consolidate them. Two genuine agent-misleading bugs survived all of this until quality scanner caught them: "raise via offset for high fan-in" (offset is skip-count, not a "more results" lever) and "Default max_nodes is 250 — large enough for typical depth=1/2 walks" (overstates safety on UE-scale codebases).
- **Home for the skill:** Either (a) a new `planner:scan-tool-descriptions` agent that reads `#[tool(description=…)]` strings in `crates/*/src/server.rs` and validates against an explicit checklist; or (b) extension to the existing `planner:quality-scanner` prompt with these checklist items added explicitly.
- **Why a skill:** Tool description copy is *production behavior* — agents pattern-match on it to decide how to call the tool. A misleading description ("raise offset for more results") is functionally a bug. But the writer rarely tests their own copy by following the suggested action. A targeted scan would catch this faster than the existing generic quality-scanner prompt did.
- **Rough shape:** Checklist items per description: every named arg has its default and ceiling documented; the verb in any suggested action operationally produces the claimed result; the response envelope shape is named (not implied); when an agent should pick non-default values is hinted. Simplest implementation: extend `planner:quality-scanner`'s prompt with these as explicit named lenses.

### 3. `/planner:commit-phase` template (Phase 4 debrief; clear time-cost)

- **Pattern observed:** Each phase commit followed the same template — subject `[PlanName/Phase N] <short>`, body with sections for "what changed," "why," "tests added," "snapshot delta," "follow-on notes." Drafted by hand 4 times in this plan, ~5 minutes each, structurally identical.
- **Home for the skill:** A `/planner:commit-phase` slash command or a `make commit-phase PLAN=PaginationOverhaul PHASE=4` Makefile target.
- **Why a skill:** ~20 minutes per plan saved; eliminates inconsistency in commit-message structure across phases; lets the writer focus on the parts that genuinely vary (the "what surprised us" lines) rather than the boilerplate.
- **Rough shape:** Reads `Plans/{Active,Complete}/<Plan>/<NN>-<Phase>.md` frontmatter (title, deliverable, acceptance criteria), runs `git diff --stat HEAD` to get the file change list and snapshot delta, opens an editor with a pre-filled commit message, runs `git commit -F <tmpfile>` after edits.

### 4. Shared `tests/fixtures.rs` builder (Phase 2 debrief; pattern surfaced 3 times this plan)

- **Pattern observed:** This plan added three ad-hoc fixture builders: `build_indexed_fixture_with_many_orphans` (Phase 2, for the page-2 snapshot), `build_indexed_fixture_with_many_file_symbols` and `build_indexed_fixture_with_high_fan` (both Phase 3). All three follow the same shape: TempDir + write template source files + run analyze + return graph.
- **Home for the skill:** A `crates/codegraph-tools/tests/fixtures.rs` shared module exposing one parameterized builder.
- **Why a skill:** The fourth+ paginated tool that needs a custom fixture will reinvent the same pattern. Each implementer learns the temp-dir + write + analyze + parse dance from scratch; consolidating gives them one helper. Marginal cost is low; current cost is moderate (each fixture-builder is ~30 lines of duplicated setup).
- **Rough shape:** `fn build_fixture(spec: FixtureSpec) -> (TempDir, Graph)` where `FixtureSpec` is `{ kind: SymbolKind, count: usize, pattern: FixturePattern }` and `FixturePattern` covers `OrphanFunctions`, `MethodsInOneClass`, `HighFanInTo(symbol_name)`, etc. Documented in `CLAUDE.md` Code Conventions.

### 5. `make snapshot-clean` (Phase 1 debrief; cheap but easy to forget)

- **Pattern observed:** Verified `cargo insta pending-snapshots` reports zero before each commit. Forgetting this leaves `.snap.new` files in the working tree that get accidentally staged or, worse, missed entirely (test passes but the actual snapshot didn't update).
- **Home for the skill:** Makefile target plus optional pre-commit hook.
- **Why a skill:** One-command check; failure mode is silent (you can ship a "passing" plan with stale snapshots if you skip it).
- **Rough shape:** `make snapshot-clean` runs `cargo insta pending-snapshots`; exits non-zero if any pending exist; suggests `cargo insta review`.

### 6. CLAUDE.md note on `parsed_sorted` snapshot quirk (Phase 1 + Phase 4 debriefs — pattern repeated)

- **Pattern observed:** Two phases initially wrote doc-comments claiming snapshot files were the ground truth for wire-format field order. Both got it wrong: the insta harness alphabetizes JSON keys via `parsed_sorted` before recording, so snapshots verify field presence/values but not declaration order. Caught both times by quality scanner.
- **Home for the skill:** One paragraph in `CLAUDE.md` Code Conventions section.
- **Why a skill:** Same trap will catch the next contributor adding a paginated tool. Not a tool, just documentation — but documentation that prevents two sessions of identical back-and-forth.
- **Rough shape:** Add a bullet under Code Conventions: "Wire-format field order is governed by struct declaration order (serde guarantees declaration-order serialization for `derive(Serialize)`). Snapshot files alphabetize JSON keys via the test harness's `parsed_sorted` helper, so the *struct itself*, not the snapshot, is the source of truth for declaration order."

## Takeaways

- **The "byte-identical foundation phase" pattern works, but creates a contract debt that the next phase must explicitly repay.** Phase 1 deliberately deferred adding the design's `limit ≤ 1000` clamp to `search_symbols` because doing so would have broken byte-identicality. Phase 2 had to harmonize. Future foundation phases should explicitly list "deferred contract: …" in their debrief and reference it as a hard subtask of the next phase.
- **Quality scanner per phase pays for itself.** Three Minor findings in this plan, all real bugs (one spec-compliance, one test-code duplication, one agent-misleading copy), all caught before the relevant commit. Without per-phase scanning, these would have shipped silently and surfaced later as either user-reported bugs (the misleading description) or maintenance friction (the duplicated helper).
- **Agent-facing `#[tool(description=…)]` copy is production behavior.** The misleading "raise via offset" wording would have actively misled agents into doing the wrong thing on high-fan-in calls. Treat description text with the same scrutiny as the implementations they document — not as freeform documentation.
- **Discriminator tests beat passing tests.** The diamond test as originally drafted in /plan would have passed regardless of unique-name vs visit counting. Plan reviewer's intervention (specify `max_nodes < total_visits` so a naïve visit counter would truncate) made it a real verification. A passing test that would still pass with the bug is documentation, not verification.
- **Pre-implementation review pays back disproportionately.** Plan reviewer's /plan-pass surfaced one Critical (design self-contradiction) and three Major findings *before /implement started*. All four were addressed in the design or plan revisions; implementation hit zero "the plan said X but X is wrong" stalls. Counterfactual: each of those findings discovered mid-implementation would have cost a phase rewrite or a follow-on commit.
- **Plan size matters for retro shape.** This plan was 4 phases / 1 session; the RustRewrite retro covered 7 phases / ~6 weeks. Both retros use the same template and capture comparable lessons-per-phase, but this one's "What Could Be Improved" is shorter because the feedback loop was tight enough that most issues got fixed inline rather than carried forward. Single-session plans don't accumulate the "deferred to next phase" debt that long plans do.
