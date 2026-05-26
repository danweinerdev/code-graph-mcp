---
title: "Retro: AnalyzeCodebaseAsync"
type: retro
status: draft
created: 2026-05-26
updated: 2026-05-26
tags: [analyze, mcp, async, get_status, single-flight, planner-process, dispatch-cadence]
related:
  - Plans/AnalyzeCodebaseAsync
  - Designs/AnalyzeCodebaseAsync
---

# Retro: AnalyzeCodebaseAsync

Reflection on a 2-phase, 10-task plan that delivered `analyze_codebase_async` (poll-based kickoff via `get_status.analyze_job`) as the structural fix for `MCP_TOOL_TIMEOUT` killing long sync analyses. Sync `analyze_codebase` was rewritten internally to share the slot machinery (wire format byte-identical). Each task was dispatched to a `sdd-planner:code-implementer` agent sequentially by the orchestrator. Closed with a release-binary smoke test against `external/ripgrep` and full `make {lint,fmt-check,test,snapshot-clean,build}` green.

## What Went Well

- **Sequential per-task implementer dispatch held up across 10 tasks with zero rework.** Every task brief carried explicit "Required reading" pointers + "Already landed (do NOT redo)" commit references + "Out of scope" guardrails. Implementers consistently delivered byte-identical wire-format preservation, complete checkbox flips in the plan, and one focused commit per task. No "wait that wasn't supposed to touch X" surprises across the entire feature.
- **The "Already landed (do NOT redo)" commit-reference convention** in each task brief prevented the most likely failure mode of fresh implementer agents — re-doing work from prior tasks. Naming the SHAs and one-line summaries kept each agent's context grounded in current state, not stale planning text.
- **The progress-monotonicity design-vs-impl divergence surfaced by Task 2.3's implementer was caught at the right moment.** The implementer noticed during test design that the design's `// monotonic, files processed` doc-comment and the implementation's per-phase reset disagreed, scoped its test to parse-phase-only via a `progress_message` filter, and flagged the gap explicitly in its report. The orchestrator surfaced it to the user mid-phase, the user picked "fix doc now" over "defer" or "add global counter," and `e955a3f docs(progress)` landed as a 5-file pure-doc commit before 2.4 dispatched. The remediation was cheap, in-scope, and bought a now-coherent contract across `indexer.rs`, `handlers/status.rs`, the design doc, CLAUDE.md, and the test comment.
- **`SLEEP_PER_PARSE_MS` knob in `test_recording_plugin.rs`** (added in Task 2.2) was the load-bearing primitive for deterministic timing across three race-sensitive tests. The implementer used `std::thread::sleep` (NOT `tokio::time::sleep`) because `parse_file` runs inside rayon workers via `spawn_blocking` — getting that right at the prereq step prevented an entire class of "sometimes the test sleeps the runtime instead of the worker" flakes.
- **Task 2.3's implementer caught the knob-Mutex race proactively.** When a third test joined the knob's user set, Cargo's intra-binary parallel test execution could let one test's `Drop` zero the knob while another was depending on it. Added `static SLEEP_KNOB_LOCK: Mutex<()>` to `ParseSleepGuard` and hardened all three tests in the same commit. 2.2's tests benefited too — they had been relying on scheduler luck.
- **2.4's dual-server cross-check** (the stricter version of the spec, which allowed single-server) asserts byte-equality of `files`/`symbols`/`edges`/`root_path` between an async kickoff+poll and a parallel sync `analyze_codebase` against an independent TempDir copy of the same fixture. The dual-server pattern turned out to be straightforward and gives a stronger guarantee than the single-server alternative would have — async terminal `AnalyzeResult` is provably wire-equal to sync.
- **2.5's release smoke caught a poll-cadence calibration bug in the smoke driver, not the server.** Initial 500ms polling missed the in-progress window on `external/ripgrep` because release-mode indexing finishes in ~350ms. The implementer tightened the driver to 50ms cadence + added a fallback "terminal `progress_message` non-empty" assertion, documenting both in the script. The server-side invariant ("progress sink wires through") stays pinned by the unit and integration tests, which use the `SLEEP_PER_PARSE_MS` knob to force a deterministic in-progress window. Good separation: smoke driver tolerates fast corpora; deterministic tests pin the contract.
- **CLAUDE.md edits landed in the same commit as the code at every stage.** Task 1.5 bundled `StatusResult` extension + `get_status` tool description + CLAUDE.md tool-count bump + Response shapes entry + Known cross-cutting limitations bullet in one commit. The "Documentation read cold" lens held — every commit reads coherently.
- **Pre-existing fmt drifts were carried explicitly as "out of scope" across 2.1–2.4 and resolved in 2.5.** Three drifts had accumulated through Task 1.2's `run_analyze_job` lift; rather than each task partially fixing them or one task silently grabbing the cleanup, the orchestrator and the implementers carried them explicitly and 2.5 (the structural-verification task) was the natural home. `make fmt-check` is now green workspace-wide.
- **Wire-format byte-identity invariant on sync `analyze_codebase` held across the entire refactor.** Task 1.3 collapsed the sync handler from ~580 lines to a 24-line slot-protocol shell, but every error variant produces the same wire string and the success body is the same `tool_success_json(&result)`. The snapshot suite at `crates/code-graph-tools/tests/snapshots/` was the regression gate; no rebaseline was needed except for the additive `analyze_job: null` field in `get_status` (Task 2.4).

## What Could Be Improved

- **No phase debriefs landed in `notes/`.** The plan-level retro is now this document, but the per-phase debrief artifacts that would feed future plan retros via `Plans/*/notes/` (and the planner's Skill Opportunities aggregation pattern from prior retros) are missing. Compare against the PaginatedResponseSizeSafety retro which aggregated from 5 debriefs. The orchestrator dispatched 10 implementer tasks without a single `/debrief` invocation. Sequential dispatch without debriefs leaves a thinner historical trail.
- **The "2.2 and 2.3 are parallel-safe" claim in the plan was wrong in practice.** Both tasks wanted the `SLEEP_PER_PARSE_MS` knob; parallel dispatch would have conflicted on `test_recording_plugin.rs`. The orchestrator sequenced them — the right call — but the plan's parallelism claim was misleading. A pre-dispatch "what does each task actually touch?" check would have flagged this. Worth adding to the plan-review checklist.
- **The progress-monotonicity divergence had been latent since Task 1.1.** The design doc, the indexer comment (`/// progress is a monotonic per-job counter`, untouched since pre-RustRewrite), the `AnalyzeJobView.progress` doc-comment, and CLAUDE.md ALL claimed monotonicity. Only the implementer dispatched for the progress-asserting test caught it — and only because writing the assert forced re-reading the contract. Earlier surfacing (e.g., during the design review, or as a pre-flight cold-read of the design vs. the indexer) would have caught it before Task 2.3 had to work around it. Plan-reviewer cold reads typically catch shape gaps but not semantic-vs-implementation gaps in long-standing code.
- **Initial 2.5 smoke-driver assumption was that release-mode indexing would take long enough for 500ms polls to land mid-run.** It doesn't. ripgrep (100 files, release, parallel rayon) finishes in ~350ms. The plan's manual-smoke wording inherited this assumption from the design's "UE / LLVM-scale, 130-200s wall time" framing. For small-to-medium corpora the smoke loop needs sub-100ms cadence to observe the in-progress window. Worth tightening the plan template.
- **Tool description for `analyze_codebase_async` carries two `note` string variants** ("analyze kicked off — …" for the new-job path and "analyze already in progress — args ignored; poll get_status for progress" for the duplicate-kickoff path). The implementer chose this split for clarity; the plan was agnostic. If a future test pins the exact note string, the two-string surface is more API to maintain than a single shared note. Minor — flagged once at the time, not regretted, but worth knowing about.
- **The `tend` (plan hygiene) skill was used once at the start (commit `9695e41 Updated plans with tend`) and not revisited.** Mid-plan status drift (e.g., 1.2's status field staying `in-progress` after the commit landed) was caught by the orchestrator manually before each dispatch, but a periodic `/tend` would have surfaced these in one pass. Especially for plans with this many tasks.
- **CLAUDE.md "Test conventions" says shared helpers live in `super::test_helpers::*`,** but the implementer for Task 2.4's integration test couldn't reach `body_text` (it's `pub(super) #[cfg(test)]`) and used `tests/common/mod.rs::first_text` instead. Two helper layers (`super::test_helpers` for unit tests, `tests/common` for integration tests) is fine, but the convention text in CLAUDE.md only mentions the unit-test one. Worth adding a sentence for integration-test reach.

## Action Items

User can pick or skip; no commitment baked in.

- [ ] Review the captured Skill Opportunities below and pick which (if any) to implement as a follow-up plan.
- [ ] Consider whether `/debrief` should be enforced (or at least prompted) by the orchestrator after each task or each phase, not only at plan boundary.
- [ ] Update CLAUDE.md "Test conventions" to mention `tests/common/mod.rs` integration-test helpers alongside `super::test_helpers::*`.
- [ ] Audit other long-standing doc-comments for similar semantic-vs-implementation drift (the indexer's `progress` was monotonic on the comment, per-phase reset in the code, since well before this plan).

## Key Metrics

| Metric | Value | Notes |
|---|---|---|
| Phases | 2 | Implementation (1.1–1.5) + Testing (2.1–2.5) |
| Tasks | 10 | 5 implementation, 5 testing |
| Commits on `rust-main` | 11 | Including 1 doc-fix interleaved between 2.3 and 2.4 |
| Diff size | +6346 / −496 across 139 files | Includes 1ea0565 plan/design landing → 8b1c4ee close-out |
| Implementer agents dispatched | 10 | One per task, all sequential |
| Tasks landed cleanly first pass | 10 / 10 | Zero re-dispatches; all acceptance gates met on first commit |
| Wire-format byte-identity preserved on sync path | Yes | Snapshot suite was the regression gate; no rebaseline needed except additive `analyze_job: null` in `get_status` |
| Tool count change | 18 → 19 | `analyze_codebase_async` added |
| Analyze handler tests | 9 → 23 | 5 lifecycle + 4 race + 5 rotation/failure/progress |
| New integration tests | +1 | `analyze_async_lifecycle::async_kickoff_poll_then_query_symbols_end_to_end` |
| New per-tool snapshot tests | 25 → 26 | `tools_list_analyze_codebase_async` added |
| Race-test determinism check | 10/10 (handler suite), 15/15 (progress test alone) | No flakes |
| Pre-existing fmt drifts resolved | 3 | Carried from Task 1.2; closed in 2.5 |
| New `unsafe` introduced | 0 | Design forbids it; grep-verified in 2.5 |
| `CACHE_VERSION` bumped | No | On-disk cache shape untouched |
| Design-vs-impl gaps surfaced + fixed | 1 | `progress` monotonicity, fixed via doc edit (`e955a3f`) |
| Manual smoke target | `external/ripgrep` (100 files, release) | Kickoff 1ms; full index 350ms; all 5 canonical `eprintln!` phase prefixes observed in stderr |
| Phase debriefs written | 0 | Process gap — see What Could Be Improved |
| Days elapsed | 3 (2026-05-23 plan → 2026-05-26 retro) | Design landed 2026-05-23; Phase 1 spanned 2026-05-24; Phase 2 spanned 2026-05-25 → 2026-05-26 |

## Skill Opportunities

Patterns that repeated across this plan and would benefit from being enshrined. Listed in rough order of repeat-frequency × leverage.

### 1. Sequential implementer dispatch with "Already landed" + "Out of scope" sections in every brief

- **Pattern observed:** Every one of the 10 task briefs followed the same shape: Required reading (numbered list of files + design + CLAUDE.md), Already landed (named commit SHAs + one-line summaries — DO NOT redo), Your task (verbatim from the plan), Acceptance (cargo build/test/clippy/fmt + checkbox flips + status flip), Out of scope (named, with reason). The implementer agent never re-did prior work and never strayed into adjacent tasks. **Repeated 10×** with the same shape.
- **Home for the skill:** New `/sdd-planner:dispatch-implementer <plan>/<phase>/<task-id>` slash command that auto-generates the brief from the plan + recent git log + CLAUDE.md + design doc. Or extend `/sdd-planner:implement` to emit one brief per task rather than per phase.
- **Why a skill:** The 10× repetition with the same shape suggests automation would compress ~3–5 minutes of brief-authoring per task into ~30 seconds. The "Already landed" section in particular is mechanical — `git log --oneline <plan-start>..HEAD` filtered to the plan's tasks.
- **Rough shape:** Inputs — plan path + task id. Outputs — a fully-formed brief written to stdout (or sent to a `sdd-planner:code-implementer` invocation). Reads — plan task entry + design + CLAUDE.md + `git log` since plan start. Auto-injects the standard "Acceptance" gates from `Makefile`. When to invoke — instead of manually writing each task brief. Wraps — `sdd-planner:code-implementer`.

### 2. Per-task debrief enforcement

- **Pattern observed:** 10 tasks shipped without a single `/debrief` invocation. Plan-level retro (this document) had to be assembled from implementer reports + commit messages + plan checkboxes, with no per-phase intermediary artifact. Prior plans (PaginatedResponseSizeSafety) had 5 debriefs that fed forward. **Process gap observed once at plan-retro time; would have been observed 2× had per-phase debriefs been the norm.**
- **Home for the skill:** Extend `/sdd-planner:implement` (or the new dispatch skill above) to *require* `/debrief` after each task — either by emitting a "debrief skeleton" the implementer fills in alongside the commit, or by gating the next task's dispatch on the prior task's debrief existing.
- **Why a skill:** Debriefs are the canonical Skill-Opportunities-aggregation surface. Without them, future plan retros lose a layer of fidelity. The cost per debrief is small (~30 lines); the leverage compounds across plans.
- **Rough shape:** Inputs — task id + commit SHA. Outputs — `notes/<phase>-<task>.md` debrief from a fixed template. When to invoke — automatically after each implementer task completes. Wraps — a small template renderer; could surface the implementer's "design ambiguities resolved" notes verbatim into the Decisions section.

### 3. Pre-dispatch "what does this task actually touch?" check

- **Pattern observed:** Plan said Tasks 2.2 and 2.3 were "parallel-safe." In reality both wanted `SLEEP_PER_PARSE_MS` on `test_recording_plugin.rs`; parallel dispatch would have conflicted. Orchestrator caught it manually before dispatch. The same shape — "plan says parallel; actually shares a file" — likely recurs elsewhere.
- **Home for the skill:** New `/sdd-planner:check-parallelism <plan>/<phase>` skill that scans the task verifications/subtasks for shared file paths and flags conflicts before the user dispatches in parallel.
- **Why a skill:** Catches "plan claims parallel-safe, isn't" without a manual grep pass per phase. Cheaper than discovering it via a merge conflict during the second parallel implementer's commit.
- **Rough shape:** Inputs — plan path + phase id. Reads — each task's `verification` field + subtasks. Outputs — a table of (task A, task B, shared file paths). Heuristic: grep for `crates/.../<file>.rs` and `tests/.../<file>.rs` substrings. When to invoke — before each `/sdd-planner:implement <phase>` call where the user is considering parallel dispatch.

### 4. Doc-vs-code semantic-drift audit (cold read)

- **Pattern observed:** The `progress` field's "monotonic" claim had been in `indexer.rs:68`, `handlers/status.rs:134`, CLAUDE.md, AND the design doc since well before this plan started. Production reset progress per-phase the whole time. The drift surfaced only because Task 2.3's implementer wrote an assertion that touched the contract. Plan-reviewers catch shape gaps; this was a semantic gap requiring a deep read of both sides.
- **Home for the skill:** New `/sdd-planner:doc-vs-code-audit <module>` skill that takes a doc-comment-bearing module + the implementation file and asks: "does the implementation behave as the doc-comment claims?" Pair with the existing `Documentation read cold` quality lens.
- **Why a skill:** Catches semantic drift that's invisible to grep, snapshot tests, and structural review. The cost per audit is real (LLM read pass) but the cost of a recurring contract gap is higher.
- **Rough shape:** Inputs — module path + key fields/contracts to audit (or auto-derived from doc-comments). Outputs — a list of (claim, observed behavior, divergence?) rows. When to invoke — periodically (e.g., before any plan that touches a long-standing contract), or as a periodic `/tend` extension. Wraps — a quality-scanner-like agent with explicit doc-vs-code framing.

### 5. Standardize "smoke driver tolerates fast corpora; deterministic tests pin the contract" pattern

- **Pattern observed:** 2.5's release smoke initially tried to assert mid-run progress with a 500ms cadence — failed because release-mode indexes a 100-file corpus in 350ms. The fix was to relax the smoke driver (faster cadence + fallback "terminal `progress_message` non-empty" evidence) while leaving the strict in-progress invariant to the deterministic unit/integration tests that force a 200ms window via `SLEEP_PER_PARSE_MS`. **The split is the right pattern — smoke proves the binary works; deterministic tests prove the contract.** Worth elevating into a documented pattern.
- **Home for the skill:** Add a "Smoke vs. deterministic test" section to the plan template's manual-smoke subtask wording. Or new `/sdd-planner:smoke-driver` skill that generates a smoke driver from a plan's Acceptance Criteria with the relax/fallback pattern baked in.
- **Why a skill:** Without explicit guidance, plan authors (including the design author here) inherit the "long indexing time" assumption from the design doc's worst-case framing and write smoke assertions that don't hold for small corpora. The relax pattern is easy once you've seen it, hard if you haven't.
- **Rough shape:** Inputs — plan path + which acceptance criteria are "binary works" vs. "contract holds." Outputs — a smoke driver template + a note for the deterministic-test author about what to pin. When to invoke — at plan-design time and at Phase-final-task time.

### 6. Wire-format byte-identity invariant as a first-class plan field

- **Pattern observed:** Throughout this plan, "wire format byte-identical on the sync path" was a load-bearing invariant called out in nearly every implementer brief and verification field. The snapshot suite served as the regression gate. The orchestrator named this invariant by hand in every Task 1.x brief. **Repeated 5× explicitly.**
- **Home for the skill:** Add a top-level "invariants" field to the plan frontmatter (alongside `tags`, `related`) that the dispatch skill auto-injects into every task brief. Or convention: the plan's README has an "Invariants" section that gets pulled into every brief.
- **Why a skill:** Naming the invariant in every brief is the right move (implementers can't infer it from "Required reading" alone — they need the contract spelled out). But repeating it by hand has 5× redundancy in this plan, would scale poorly to larger plans.
- **Rough shape:** Plan frontmatter `invariants: [..]` field; dispatch skill reads and injects.

## Takeaways

Single-flight slot protocols are simple when the gate is one lock and the worker has no error-return type. The plan landed because the design got the gating right (slot is the gate; `index_lock` is no longer a coordination primitive for analyze callers, only for the worker vs. watch) — every subsequent task was just realizing that pattern in different surfaces (sync handler, async handler, status view, race tests, integration test, smoke).

Sequential per-task dispatch with rigorously-scoped briefs worked. The "Already landed" + "Out of scope" + "Required reading" shape eliminated the most common failure mode of fresh implementer agents (re-doing prior work or wandering into adjacent tasks). The orchestrator did pay a real cost in brief-authoring time, which is the strongest argument for skill opportunity #1.

The progress-monotonicity divergence is the most interesting lesson. A doc-comment about a monotonic counter had survived a major rewrite (the RustRewrite plan from earlier this month moved this code from Go to Rust); the per-phase reset was clearly intentional in the code; nobody noticed the contract gap until a test had to assert against it. **Designs and doc-comments age silently.** Skill opportunity #4 — periodic doc-vs-code audit — is the structural answer; the user's "complete in-scope remediation now" preference is the cultural one, and both worked together to close the gap cheaply.

The smoke-test calibration story is a reminder that test fidelity assumptions inherited from worst-case design framing don't survive real-corpus contact on a release binary. Keep deterministic tests strict (force a window via `SLEEP_PER_PARSE_MS`); keep smoke drivers tolerant (handle the case where the in-progress window is shorter than the poll interval); document the split.

Feature is shippable. The next plan that touches this code can rely on: slot is the gate, `progress` is per-phase, `analyze_job` lives on `get_status` as nullable, async kickoff is `< 1KB` regardless of corpus size.
