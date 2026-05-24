---
title: "Phase 6 Debrief: search_symbols suggestions + UE watcher preset + CLAUDE.md sweep + acceptance regression"
type: debrief
plan: "ResponseShapePolish"
phase: 6
phase_title: "search_symbols suggestions + UE watcher preset + CLAUDE.md sweep + acceptance regression"
status: complete
created: 2026-05-16
updated: 2026-05-16
tags: [mcp, pagination, ue, unreal-engine, ergonomics, hierarchy, diagrams, coupling, dependencies, fuzzy-match]
---

# Phase 6 Debrief: search_symbols suggestions + UE watcher preset + CLAUDE.md sweep + acceptance regression

Final phase of ResponseShapePolish. The plan is fully complete and moved `Plans/ → Plans/`.

## Decisions Made

- **`SearchSymbolsResponse` = `#[serde(flatten)] Page<SymbolResult>` + `skip_serializing_if`-omitted `suggestions` (6.1/6.2).** The flatten-over-generic-`Page<T>` layout compiled cleanly (6.1's compile-smoke proved it), so the documented non-flatten fallback was not needed. `suggestions` is populated ONLY when the **raw** query is `^…$`-anchored AND `total == 0`; the `^$`/`^`/`$` degenerate inputs short-circuit to no-suggestions (empty inner would broad-match the whole graph). Non-anchored or has-results responses are byte-identical to a bare `Page<SymbolResult>` (6.1 proved wire-compat; every existing snapshot uses non-anchored-with-results queries so none regenerated).

- **`count_only=true` deliberately never emits `suggestions` (W2 decision, documented in source).** The count_only early-return is the `<1 KB`, records-free sentinel; a suggestion list would breach that contract. Resolved as correct-by-design and pinned with a behavioral comment so a future change doesn't "fix" it and silently break the sentinel. The `^$` degenerate test was hardened to assert wire-absence (`!body_text.contains("suggestions")`), matching its sibling absence tests.

- **`Graph::search_symbols(&str, Option<SymbolKind>) -> Vec<Symbol>` — the plan's `None` is the KIND filter, not a namespace filter.** 6.2's implementer verified the real signature instead of transcribing the plan prose, used `symbol_id()` to build id strings (same idiom as `suggest_symbols`, which was NOT reused/mutated — it returns a comma-joined `String`, wrong shape, and changing it would break three callers). One more instance of the plan-prose-vs-shipped-API pattern, caught by verify-before-asserting.

- **6.7 source-leak sweep run as a *judged* per-line bucket-1/bucket-2 classification, not a blind grep-replace.** Bucket-1 (a comment telling you *which task/phase added this code* → rots) is rewritten to a behavioral description; bucket-2 (a `//!` preamble documenting *when/how a whole subsystem came to exist under the original RustRewrite* → canonical origin doc) is preserved verbatim. `crates/*/src/` ended at **zero bucket-1**, with exactly two bucket-2 survivors (`server.rs:3`, `handlers/mod.rs:4-5`) — the named exemplars.

- **`tests/` was swept as a user-authorized scope extension (escalation, rule #3).** 6.7's charter was `crates/*/src/`. The sweep surfaced a large `crates/*/tests/*` plan-pointer surface; rather than auto-decide, the orchestrator escalated. The user chose "sweep tests/ now." 184 hits → 169 bucket-1 rewritten / 15 bucket-2 preserved. The bucket-2 set was extended by analogy to the five `watch_*_reindex.rs:1` "Phase N.M watch-mode reindex regression test" headers (the watch-mode analogue of the corpus-origin headers) — independently judged sound by the scan.

- **6.5 diamond modelled via `impl Trait for Type`, not the plan's literal `trait Leaf: D1 + D2` supertrait bounds.** The Rust parser's `INHERITANCE_QUERIES` matches only `impl_item` (impl-block trait impls); supertrait bounds emit NO `Inherits` edge (documented behavior, CLAUDE.md Rust section). The implementer verified the real emitted JSON (`"ref": true` actually present) before writing the assertion. A correct deviation toward shipped behavior, not drift.

- **CLAUDE.md `extra_ignores`→`extra_ignore` typo charter closed (6.4).** The real serde field is singular `extra_ignore` (`config.rs:98`, no `deny_unknown_fields`, so the plural silently no-ops). 6.4 fixed every CLAUDE.md occurrence to singular; verified zero plural survive. The recurring shipped-vs-design reconciliation backlog from all five prior debriefs was folded into 6.4's CLAUDE.md sweep and verified against shipped code (not plan prose).

- **Authoring-time readiness lint adopted (user decision this debrief).** The plan-prose-vs-shipped-API pattern hit ~7 times across the whole plan and was caught every time by stop-and-report + intent-blind scan, but at the cost of implementation-time churn. Decision: future plans add an API-existence readiness check at *authoring* time (prevention over detection). See Skill Opportunities #2.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| `SearchSymbolsResponse` flatten wrapper ships | Met | `#[serde(flatten)] Page<SymbolResult>` + `suggestions` (`skip_serializing_if = "Vec::is_empty"`). Flatten-over-generic compiled; fallback not needed. Wire-compat for non-anchored/has-results proven. |
| Anchored-zero suggestion trigger fires; substring-zero does not | Met | Trigger keys off raw query `^…$` AND `total==0`; non-anchored & has-results omit the key (wire-absent, not `[]`). `^$`/`^`/`$` degenerate guarded; `count_only` never emits. 4+1 behavior-named tests. |
| UE watcher preset in `.code-graph.toml.example` | Met | Fully-commented `[discovery]` UE preset with **singular** `extra_ignore` (6.3); TOML-validity smoke test deserializes the shipped example into `RootConfig`. |
| CLAUDE.md sweep covers all 5 shape changes | Met | 7 tool rows + Response-shapes/Cache-invalidation/Known-limitations updated; `<global>\|CouplingBoth\|HierarchyNode.ref\|DependencyEntry\|max_cycle_size` count = 10 (≥5); `force=true` = 7; zero plural `extra_ignores`. Verified against shipped code, not plan prose. |
| Source-leak sweep: zero plan-artifact leaks | Met | `crates/*/src/` zero bucket-1 (only the 2 canonical-origin preambles survive); `crates/*/tests/*` zero bucket-1 (user-authorized extension; 15 bucket-2 preserved). Behavior-neutral, scan-confirmed across every pass. |
| Acceptance regression fixture pins UE failure modes | Met | `response_shape_acceptance.rs`, 3 tests, real `analyze_codebase` pipeline. Each scan-verified as a true regression pin (would fail if its fix were reverted): summary cap load-bearing (uncapped 152 KB > 102 KB ceiling, capped honest envelope); real diamond emits `ref:true`; diagram has both directions + zero file-node leak. Cycle scenario explicitly skipped, cites `per_cycle_cap_truncates_large_scc` by bare name. |
| Structural verification (6.6) | Met | `make verify` green; 6.5 3/3; C++ dogfood fmt/curl/abseil within ±10%; CLAUDE.md greps pass; source-leak gate clean. |

## Deviations

- **`tests/` sweep beyond 6.7's `crates/*/src/` charter** — a deliberate, user-authorized scope extension (escalated, not silently absorbed). Documented so a future reader understands the test-file plan-pointer rewrites belong to 6.7.

- **6.5 diamond fixture uses `impl Trait for Type`, not the plan's literal supertrait-bound form** — deviation toward shipped behavior (supertrait bounds emit no `Inherits` edge). Verified empirically before asserting; the file documents the rationale.

- **6.5 `per_cycle_cap_truncates_large_scc` cited by bare name + behavioral description, NOT "Phase 5's …" as the plan's 6.5 verification text literally instructed.** The standing no-plan-labels-in-source rule (exhaustively enforced by 6.7 the same phase) overrides the plan's prose; baking the plan's literal wording would have re-introduced the exact rot 6.7 spent five commits removing. Resolved in the dispatch, not escalated, because the rule is settled and the user-recorded preference is unambiguous.

- **Inline W-fix resolution rather than implementer round-trips for trivial scan findings.** All Minor/Question scan findings (the `^$` wire assertion, the count_only doc note, the CLAUDE.md diagram-mode precision, the tests/ banner relabels, the 6.5 predicate generalization + sanity comment) were applied directly by the orchestrator with `make verify` re-run, not bounced back to an implementer. Proportionate for one-line comment/predicate edits; the cycle budget is for implementer review-fix loops, not orchestrator-applied trivia.

## Risks & Issues Encountered

- **The judged leak-sweep's grep pattern was too narrow — recurred 3× within 6.7.** The original sweep matched `Task N.N|Phase N|Plans/…`; a corrective broadened to `in 7.x`/`wired in`/`live in`; a second corrective to `plan Decision`/`design brief`/`per the task brief`; a final one-line orchestrator fix closed the last `design brief` straggler. Each miss was caught by the intent-blind scan judging against the *goal* ("no rotting plan pointers") rather than the *task's grep*. No defect shipped — but the agent review-fix cycle budget for 6.7 was exhausted and the pattern argues strongly for a pre-built broad-pattern leak-scan (Skill Opportunities #1). The user authorized the `tests/` extension on top, which the broadened pattern then handled in a single judged pass.

- **Plan-prose-vs-shipped-API pattern, instance ~#7 (6.5 trait-supertrait diamond).** `trait Leaf: D1 + D2` produces no `Inherits` edges; only `impl Trait for Type` does. Caught by the standing verify-real-API-before-asserting rule. Across the whole plan this pattern appeared ~7× (search_symbols namespace, class_hierarchy direction arg, Phase-3 dedupe key, Phase-4 `.ini` filter scope, `kind_str(EdgeKind)`, `extra_ignores` plural, this). Every instance caught, zero shipped defects — but consistently at implementation-time cost. Motivated the user's decision to add an authoring-time readiness lint.

- **`SendMessage` unavailable — could not resume a prior agent.** Agents reported a `SendMessage`-to-`agentId` resume affordance, but the tool was not available in this session. Every corrective pass was therefore dispatched as a *fresh* `code-implementer` with a fully self-contained brief (prior commit hashes, exact file:line lists, recommended rewrites, constraints). This worked but is verbose and shifts synthesis load onto the orchestrator. Lesson: do not assume agent-resume is available; write corrective dispatches self-contained by default.

- **Large-surface comment sweeps need the test-rewrite-classification + behavior-neutrality scan, not `make verify` alone.** 6.7's `cd17359` (42 files) and `2c18f12` (33 test files) were behavior-neutral, but `make verify` only proves compile/clippy/fmt/test/snapshot — it cannot prove a doc rewrite didn't silently drop a load-bearing caveat, or that a reworded assertion message isn't asserted-on. The intent-blind scan's OLD-vs-NEW caveat audit (20 rewrites sampled per pass) and the per-string asserted-on grep were the load-bearing checks; they found the 3 reworded assert messages were safe and that zero caveats were dropped.

## Lessons Learned

- **A judged classification sweep needs the broadest plausible match pattern up front.** Narrowing the grep to the obvious form (`Task N.N|Phase N`) and iterating cost three corrective cycles on 6.7. The judgment (bucket-1 vs bucket-2) is the irreducible human-ish part; the *discovery* (finding all candidates) should be maximally inclusive from the first pass — over-match and let the per-line judgment filter, never under-match and rediscover. This is the core argument for the pre-built leak-scan skill.

- **Prevention beats detection for the plan-prose-API pattern once it recurs ~7×.** Stop-and-report + intent-blind scan caught every instance with zero shipped defects — a genuinely working safety net — but seven implementation-time interruptions across one plan is a signal the defect should be killed at authoring time. An identifier in a verification field is a falsifiable claim ("this symbol exists"); a readiness lint can check it mechanically before an implementer ever reads it.

- **Escalate scope expansions; don't auto-absorb them — but expect this user to authorize the fuller fix.** The `tests/` surface was outside 6.7's charter. Surfacing it as an explicit decision (rather than silently sweeping or silently skipping) was correct per the escalation rule; the user chose the complete remediation, consistent with the now-recorded standing preference. The transparency is the point — the user got to decide, and the decision is on the record.

- **Verify-real-API-before-asserting is now proven across an entire plan (~7 catches).** Every time an implementer built the fixture, ran the tool, and inspected real emitted JSON before writing the assertion, a plan-prose inaccuracy was caught and resolved correctly without weakening the test. This discipline, plus stop-and-report, plus the intent-blind scan, is a three-layer net that held for six phases. Keep all three; add the authoring-time lint as a fourth, earlier layer.

- **Orchestrator-applied inline fixes are the right tool for trivial scan findings.** Bouncing a one-line comment/predicate fix to an implementer is ceremony without value. Apply directly, re-run `make verify`, commit as a `W<n> quality-scanner fixes` commit. Reserve implementer review-fix cycles (max 2) for findings that require code authoring or judgment.

## Impact on Subsequent Phases

- **None within ResponseShapePolish — this was the final phase; the plan is complete and moved to `Plans/`.** All six phases shipped; the cross-phase shipped-vs-design reconciliation backlog accumulated across debriefs 1–5 was discharged in 6.4's CLAUDE.md sweep and verified against shipped code.

- **Cross-plan carry-forward (the next plan, not this one):**
  - The **authoring-time readiness lint** (user-decided this debrief) should be wired into the next plan's `/plan` + `/implement` readiness audit from the start.
  - The **judged leak-scan** prevention rule held for all *new* code every phase (zero new leaks introduced in six phases). The one-time pre-existing-debt remediation is now DONE for `crates/*/src/` and `crates/*/tests/*`; the standing "no plan/task labels in source" rule continues, now with a reusable scanner to enforce it (Skill Opportunities #1).
  - **`/sdd-planner:close-phase`** was deferred every debrief Phase 1–6; the user has now chosen to build it as a cross-plan tool. It is no longer a ResponseShapePolish item — it is an upstream/cross-plan deliverable.

## Skill Opportunities

User-confirmed this debrief: enshrine #1, #2, and #3 as **actionable** (build), not record-only.

### 1. Judged leak-scan — ACTIONABLE (build) ✓ user-selected

- **What recurred:** the bucket-1/bucket-2 grep→classify→rewrite loop, run 5× in Phase 6 (4 src passes + 1 tests/ pass), with the discovery grep widened three times because the pattern under-matched.
- **Where it belongs:** an in-repo **Makefile target + helper script** (`make leak-scan`) — this is the correct home per the standing project rule that project-specific tooling lives in-repo, NOT in the global plugin ([[feedback-global-vs-project-agent-customization]]). It is repo-coupled (knows the two bucket-2 origin preambles, the `crates/*/{src,tests}` surface) so it does not belong in a generic plugin skill.
- **Why a skill:** collapses the 3-corrective-cycle narrow-grep churn into one broad pass; the human judgment (bucket-1 vs bucket-2) stays manual but the *discovery* is maximally inclusive and reproducible. Also serves as the enforcement mechanism for the standing "no plan/task labels in source" prevention rule.
- **Rough shape:** `make leak-scan` runs the broad union pattern (`Task N.N|Phase N|plan (Decision|task)|per the task brief|design (doc|brief)|Plans/Active|ResponseShapePolish|\.plans/|wired/live/lands/documented/covered/defined in N.N|lowercase phase N.N`) over `crates/*/{src,tests}/*`, prints every hit grouped by file, and exits non-zero if any hit is NOT one of the known bucket-2 allowlist lines (the two `src` preambles + the corpus/watch/concurrent origin headers). Output is a classification worksheet; the rewrite stays a judged human/agent step. Buildable now in-repo.

### 2. Plan-readiness API-existence check — ACTIONABLE (build) ✓ user-selected; user also chose "add authoring-time readiness lint"

- **What recurred:** ~7× across the plan, a task verification field named an API/identifier/argument that did not exist or had a different shape in the shipped code; each surfaced only at implementation time.
- **Where it belongs:** a `/plan` authoring step and the `/implement` readiness audit. NOTE — these are **plugin-level** (`project-planner` skills), and the standing project rule says project-specific extensions do NOT go into the global plugin cache. Therefore this is recorded as a **project-planner upstream recommendation**, to be raised against the plugin, not patched into `~/.claude/plugins/cache/`. The in-repo expression of the same idea (a CI check that greps backtick-quoted identifiers in `.plans/**/*.md` verification fields against the codebase) IS buildable in-repo and is the pragmatic interim form.
- **Why a skill:** prevention over detection — kills the single most recurrent failure class of this plan at authoring time.
- **Rough shape:** given a phase doc, extract backtick-quoted identifiers from each task's `verification` field; `grep`/symbol-search each against the target codebase; flag any with no definition (excluding stdlib/obvious types) as "forward reference or stale API — confirm before implementation." Mirrors the existing `/implement` forward-reference audit, extended from plan-internal to codebase-external.

### 3. `/sdd-planner:close-phase` — ACTIONABLE (build) ✓ user-selected; deferred Phases 1–6

- **What recurred:** every phase closeout — set task statuses + phase status in the phase doc, mirror to README `phases[]`, bump `updated`, check subtask boxes, and (final phase) move `Plans/Active → Plans/Complete`. Done by hand with `Edit` (sed doesn't persist on Edit-tracked files) every single phase.
- **Where it belongs:** plugin-level `/sdd-planner:close-phase`. Same plugin-vs-project caveat as #2 — record as a **project-planner upstream deliverable**, not a cached-plugin patch. The user has explicitly chosen to build it as a cross-plan tool (it is no longer a ResponseShapePolish-scoped item).
- **Why a skill:** removes a repetitive, error-prone, ~6-edit manual dance done identically 6×; enforces phase-doc/README consistency mechanically.
- **Rough shape:** `/sdd-planner:close-phase <plan> <N>` — set phase + all its tasks to `complete`, tick subtasks, bump `updated` in both phase doc and README, and if all phases complete, perform the VCS-appropriate `Active→Complete` move. Idempotent; prints a diff of artifact changes.

### Carried-over (status update)

- **Test-rewrite classification standard in quality-scan dispatch** — ran on every Phase 6 scan; correctly classified 6.1/6.2's new-tests-only commits, 6.7's comment-only test edits (verified zero assertion/name/input changes across 33 files), and 6.5's all-new file. Continue; it is now standing.
- **Deferred-finding tracking in dispatch** (adopted Phase 5) — applied to the 6.4 `extra_ignores` typo charter (carried from a W1 deferral into 6.4's dispatch, verified closed by the 6.4 scan). Worked as intended.
- **`make verify`** (built Phase 5) — was the closeout gate from the start of Phase 6, run after every W-fix and as the 6.6 authoritative gate. No masked-exit recurrence. Standing.
- **`make snapshot-accept FILE=<stem>`** (built Phase 3) — not needed this phase (no snapshot regenerated; the response-shape changes were additive/wire-compatible by design). The full-stem usage-hint friction noted in the Phase 5 debrief remains a minor open polish item.
