---
title: "Retro: PaginatedResponseSizeSafety"
type: retro
status: draft
created: 2026-05-13
updated: 2026-05-13
tags: [pagination, byte-budget, mcp, planner-process]
related:
  - Plans/PaginatedResponseSizeSafety
  - Designs/Pagination
  - Plans/PaginationOverhaul
---

# Retro: PaginatedResponseSizeSafety

Reflection on a 5-phase plan that delivered byte-budget enforcement, `count_only`, and `SymbolResult.file` slimming across the MCP paginated tool surface. Triggered by a real user-reported failure mode (1031-orphan response, ~74K tokens, harness rejection). Closed with an acceptance regression test pinning that exact failure mode against a 1500-orphan fixture.

## What Went Well

- **Plan reviewer pre-flagging Critical risks paid off.** The reviewer named two non-snapshot JSON-key consumers (`mixed_language.rs:204`, `symbols.rs:541`) ahead of Phase 3.4. Without that flag, the `SymbolResult.file` drop would have shipped with two runtime test failures that compile-time checks couldn't catch. Captured pre-flight, fixed in-flight.
- **The "polish after every quality scan" cadence kept the foundation tight.** ~12 polish commits across the plan, each addressing the scan's non-Critical findings inline before moving on. Phase 2.5's `byte_budget_take` got a `debug_assert!(limit > 0)` from this loop that would have been a Phase 2 wiring trap otherwise.
- **Per-task commit messages with `(N.M)` and `(N.M polish)` suffixes** made `git log` a usable execution trace of the plan. Replayable by humans and future agents.
- **The architectural-exception decision for `search_symbols` (Decision 12) held up.** Handler-layer trim instead of pushing byte-budget into `Graph::search` kept the Graph layer byte-blind. One tool's quirk didn't propagate.
- **Sentinel-substitution polish in Phase 2.0** (`NO_BYTE_BUDGET` replacing 134 `usize::MAX` literals) was the right reaction to a scanner Minor finding. Named the intent at every call site without changing behavior.
- **Phase 5's empirical fixture probe was load-bearing.** The 5.1 implementer ran a throwaway probe to confirm the fixture would actually force truncation. Without it, the acceptance test could have shipped as a silent false-green. Cheap insurance.
- **Consolidated end-of-phase cold-read scan caught Major findings per-task scans missed.** Phase 4.5's polish scan found the Pagination design doc Architecture section still denying `truncated`/`next_offset` existence after D8 was appended. Per-task scans evaluated "is this commit good?"; cross-section scan evaluated "do these N commits add up to a coherent document?"
- **The `#[cfg(test)] thread_local!` heap-push counter in Phase 3.3** pinned a cost win to behavior. A future refactor that re-introduces heap construction on the count_only path fails immediately. Stronger than asserting only externally-visible results.
- **The `64K → 74K-token harness rejection` failure mode is now a single test name** (`get_orphans_under_budget_at_limit_1000`). When it fails, the regression contract is grep-able and named in the top-of-file doc. Future engineers see the bug AND the fix simultaneously.

## What Could Be Improved

- **Stale findings re-surfaced 6+ times across Phases 1–3.** "Tool descriptions advertise 4-field envelope" was correctly deferred each time as planned Phase 4.1 work. But each scan cost LLM tokens + reviewer attention without delivering new signal. A scan-deduplication mechanism would have compressed this to a single defer-tracking entry.
- **Verification fields in the plan were sometimes wrong about existing call-site counts.** Phase 2.0's verification said "no callers outside server.rs" — actually there were 14 (13 test files + watch.rs). The implementer correctly adapted; the verification field was stale. Worth a pre-implementation grep before writing the verification claim.
- **Per-task quality scans on doc-only commits returned low signal.** Phase 4.2/4.3/4.4 were doc-only and the per-task scans mostly returned "Accept" with self-reviewed implementer flags forwarded. Skipping per-task scans and running a single consolidated cold-read at phase end was strictly better for Phase 4.
- **Some plan verification fields drifted from reality during implementation.** Phase 5.1's verification field said "~80–100 bytes per record"; the empirical probe revealed ~102 bytes (paths inflate `id`). Not a blocker, but the verification field should be a target the implementer can audit against, not a stale estimate.
- **Sequential-only wave execution had real wall-clock cost.** Phase 2 ran 5 tasks fully sequential (per user choice) over file-overlap on `query.rs` and `symbols.rs`. The user chose isolation over speed; the cost was real (~40% longer wall-clock). Worth re-asking on each phase since some have no genuine overlap.
- **Stale comments after wire-format changes were a recurring polish target.** Phase 3.4 dropped `SymbolResult.file` and made several "fits ~3 records" comments stale (now "~4 records"). Phase 3.2 had a comment referencing `byte_budget_take`'s `debug_assert` from a function that doesn't call it. Catchable by the polish loop, but better caught at write time.
- **`ResponseConfig` re-export in Phase 5.2 was speculative.** Added alongside `DEFAULT_RESPONSE_MAX_BYTES` without a consumer. Dropped in polish. Tendency to expand public API surface speculatively is worth watching.

## Action Items

User chose "capture all skill opportunities; no action commitment" — so the Action Items section is operational only. See **Skill Opportunities** below for the captured patterns.

- [ ] Review the 10 captured Skill Opportunities and pick which (if any) to implement as a follow-up plan or one-off project.
- [ ] Consider whether `/sdd-planner:retro`'s "Skill Opportunities" section should auto-aggregate from debriefs in this repo (meta — this retro had to manually consolidate from 5 debrief files).

## Key Metrics

| Metric | Value | Notes |
|--------|-------|-------|
| Phases | 5 | Envelope+Helpers, WireHandlerBudget, CountOnly+Slim, Docs+Addendum, AcceptanceRegression |
| Commits on `rust-main` | 37 | Including 13 polish commits responding to scanner findings |
| New tests added | +47 | 1024 → 1082 workspace pass count (with snapshot tests) |
| Snapshot files regenerated | 15 SymbolResult-emitting | Lost `file` key (Phase 3.4); gained truncated/next_offset (Phase 1.1) |
| New snapshots added | 11 | 5 byte-budget-truncated + 3 count_only + 3 others |
| Quality scans dispatched | 16 | 12 per-task + 4 polish + 0 phase-end (cold-read was its own dispatch) |
| Critical findings shipped | 0 | All Critical surfaced by plan review pre-flight |
| Major findings caught + fixed | 3 | Including the Phase 4.5 design-doc contradiction |
| Days elapsed | 2 (2026-05-11 → 2026-05-13) | Plan, design review, implementation, debrief |
| Largest behavioral change | `SymbolResult.file` drop (Phase 3.4) | Wire-format breaking; pre-1.0 acceptable per D10 |
| Originally-reported failure mode | Pinned by integration test | `get_orphans_under_budget_at_limit_1000` |

## Skill Opportunities

Aggregated from the 5 phase debriefs. Patterns are listed in rough order of repeat-frequency × leverage.

### 1. Polish-after-scan cadence
- **Pattern observed:** After every quality-scanner returns non-Critical findings, render the findings table → ask user fix-policy ("fix all" / "fix only Major+ " / "defer") → if fix-all, dispatch a small polish implementer OR apply inline → commit with `(N.M polish)` suffix → re-verify → continue. **Repeated 12+ times** across the plan; the user picked "fix all" almost every time.
- **Home for the skill:** New `/sdd-planner:fix-findings` slash command, OR an option in `/sdd-planner:implement` that auto-applies Minor/Question findings and surfaces Major/Critical for human gate.
- **Why a skill:** Each invocation is mechanical (parse scanner table, identify file:line targets, apply documented fix). The 12× repetition with the same shape suggests automation would compress ~5 minutes of context per task into ~30 seconds.
- **Rough shape:** Inputs — quality-scanner findings table + the commit hash being polished. Outputs — a polish commit (or no commit if all findings are Critical/blocking). When to invoke — automatically after every `quality-scanner` returns, with a `--defer` override. Wraps — `sdd-planner:code-implementer` agent in narrow-scope mode.

### 2. Stale-finding deduplication
- **Pattern observed:** "Tool descriptions advertise 4-field envelope" was returned by 6+ quality scans across Phases 1–3 before Phase 4.1 fixed it. Each time the orchestrator (me) had to recall "this is the planned Phase 4.1 work" and defer. The mental tracking degraded with multiple parallel deferrals.
- **Home for the skill:** New `/sdd-planner:track-findings` skill maintaining a per-plan `notes/deferred-findings.md` list. Subsequent scans cross-reference and emit "(already deferred to Phase X)" rather than re-listing.
- **Why a skill:** When N scans return the same finding across M phases, each invocation incurs LLM token cost + reviewer attention without delivering new signal. Dedup would compress 6 surfacings to 1 entry.
- **Rough shape:** Inputs — quality-scan findings table + plan path. Outputs — deduplicated table with `(already deferred)` annotations where matches exist. When to invoke — automatic inside `/sdd-planner:implement` after each quality scan, before presenting to the user. Wraps — a simple findings-fingerprint matcher (file:line + finding-text hash).

### 3. Tool description audit (as `#[cfg(test)]` test)
- **Pattern observed:** Phase 4.1's work was 5 separate rewrites following the same 7-item checklist (envelope shape, args + defaults/ceilings, paging-resume protocol, count_only-if-applicable, byte-budget cap source, length-vs-truncated warning, operationally-correct verbs). The 4.1 polish then audited `detect_cycles` against the same checklist and found it stale. The audit logic is mechanical.
- **Home for the skill:** A Rust test in `crates/code-graph-tools/src/server.rs` (NOT a slash command). The test parses `Page<T>` field set, scans `#[tool(description=...)]` strings, asserts each paginated tool's description mentions every field, every documented arg, and a length-vs-truncated warning.
- **Why a skill:** The "tool description still 4-field envelope" stale-finding ran 6+ times because no automated check existed. A Rust test would catch the drift at PR time, not at planner-skill time. Strict mechanical convergence: write once, run on every commit.
- **Rough shape:** A `#[cfg(test)]` test in `server.rs` that pattern-matches each paginated tool's description against the canonical envelope shape AND the tool's Args struct. Fails CI on drift. Optional second mode: a `/sdd-planner:audit-tool-descriptions` slash command for planning-time review.

### 4. Cold-read end-of-phase doc scan
- **Pattern observed:** After a series of doc-only commits within a phase, dispatch one consolidated `quality-scanner` reading the whole touched-doc surface area for framing contradictions. Caught a Major in Phase 4.5 (design doc Architecture still denying truncated/next_offset) + 2 Minors that per-task scans missed.
- **Home for the skill:** New `/sdd-planner:cold-read` slash command, OR an option to `/sdd-planner:code-review` that scopes the four-lane review to docs with the cold-read lens emphasized.
- **Why a skill:** Per-task scans evaluate "is this commit good?"; cross-section cold-read evaluates "do these N commits add up to a coherent document?" Fundamentally different perspective; per-task scans cannot catch what cold-read can.
- **Rough shape:** Inputs — commit range + optional file glob (e.g., `*.md`). Outputs — quality-scan findings table emphasizing framing contradictions, dangling references, stale examples, lens checklists that contradict their own content. When to invoke — at end of any phase whose primary deliverable was docs, OR after any docs-heavy commit series (>= 3 doc commits in a row).

### 5. Empirical-probe template for acceptance fixtures
- **Pattern observed:** Phase 5.1 implementer ran a throwaway probe test to confirm the 1500-orphan fixture would actually force byte-budget truncation. Without it, the acceptance test could have shipped sized-just-below-threshold — a false-green (the most dangerous test failure mode: silent, looks like success).
- **Home for the skill:** Documented recipe in CLAUDE.md "Test conventions" section, OR a `/sdd-planner:probe-fixture` skill that generates a throwaway test, runs it, prints observed behavior, and deletes itself.
- **Why a skill:** Acceptance tests with mis-sized fixtures are the highest-risk test failure mode. Probe + delete is mechanical; encoding it as a recipe ensures every acceptance test does the probe step.
- **Rough shape:** Inputs — fixture builder + target tool call + expected failure-mode signal (truncated, error message, etc.). Outputs — observed values printed so implementer can confirm or adjust before locking assertions. When to invoke — at the start of any acceptance-regression test task.

### 6. Sentinel-substitution refactor (`NO_BYTE_BUDGET` pattern)
- **Pattern observed:** When a refactor introduces a "no enforcement" or "default" value at many call sites, bulk-substitute the literal with a documented named constant. Phase 2.0 did this for 134 `usize::MAX` sites across 15 files (~80 minutes of implementer time).
- **Home for the skill:** A `/sdd-planner:promote-sentinel` slash command OR an inline Edit recipe documented in CLAUDE.md.
- **Why a skill:** Bulk literal-substitution is error-prone by hand. Encoding the workflow (read literal+meaning → define const at canonical module → substitute call sites → verify count match) would compress to a fixed-cost operation.
- **Rough shape:** Inputs — `(literal_value, const_name, target_module, intended_meaning)`. Outputs — a polish commit with const definition + N substituted sites + verification that pre-count == post-count. When to invoke — when a quality scan flags "raw literal repeated N times with same intent."

### 7. Plumbing-first commit cadence
- **Pattern observed:** Phase 2.0 introduced `max_bytes: usize` through 5 handler signatures with `let _ = max_bytes;` suppressions and "consumed in 2.1+" comments, before any task wired the behavior. Made incremental compilation true through 2.1–2.5.
- **Home for the skill:** A documented pattern in CLAUDE.md's quality lenses section, OR a `/sdd-planner:plumbing` skill that generates the introduce-signature-suppress-uses-tag-future commit.
- **Why a skill:** Multi-task refactors need a signature-change commit before behavior commits. Doing it wrong (param added at one site, not another) breaks the build mid-phase. Formalizing the pattern enforces correctness.
- **Rough shape:** Inputs — `(parameter_name, type, target_functions, suppression_comment)`. Outputs — a commit adding the param to each function, updating all call sites with default value, inserting `let _ = <param>;`. When to invoke — at the start of any phase that needs to extend handler signatures.

### 8. Wire-format-break sweep
- **Pattern observed:** When dropping a wire field (Phase 3.4 dropped `SymbolResult.file`), `cargo check`/`clippy` don't catch JSON-key consumers (`serde_json::Value` indexing, `.get("field")`, `.expect("...field...")`). The plan reviewer pre-flagged two named consumers; the implementer ran `rg --type rust '"file"'` to find others.
- **Home for the skill:** A `/sdd-planner:wire-break` slash command OR a CLAUDE.md recipe.
- **Why a skill:** The compile-error pathway misses JSON-key consumers. A formalized audit recipe (grep for literal "field", grep for `.expect`-pattern matches, grep for `as_str().unwrap()` on suspected paths) would surface every consumer in one pass — preventing silent runtime failures.
- **Rough shape:** Inputs — `(field_name, struct_name, scope_directory)`. Outputs — a checklist of consumer sites grouped by typed-access (caught by compiler) vs JSON-key-access (NOT caught). When to invoke — at the start of any wire-format-break task.

### 9. Heap-not-touched / cost-pinning test pattern
- **Pattern observed:** Phase 3.3 added a `#[cfg(test)] thread_local!` counter on heap pushes to pin the cost win. This pattern would apply elsewhere — any optimization that avoids work could use the same scaffold.
- **Home for the skill:** Documented pattern in CLAUDE.md's test conventions section, OR a small `cost_counter!` macro in `code-graph-core`.
- **Why a skill:** Manually scaffolding (thread-local counter, increment sites, reset/read helpers, two-phase assertion) is ~30 lines. A macro compresses to ~5 lines per cost-pin.
- **Rough shape:** Inputs — `(counter_name, increment_sites)`. Outputs — the thread-local counter, the `#[cfg(test)]` bump helper, and reset/read helpers. When to invoke — when implementing any optimization that "skips work" and a future refactor could silently re-introduce the work.

### 10. Compile-time floor guards
- **Pattern observed:** Phase 5 polished a fixture's `assert!(ORPHAN_COUNT >= 1500)` runtime test to `const { assert!(ORPHAN_COUNT >= 1500) }` — clippy-recommended AND strictly stronger (a future engineer setting `ORPHAN_COUNT < 1500` now gets a build error, not a test error).
- **Home for the skill:** A pattern in CLAUDE.md's test conventions section, OR a project-wide macro `floor_assert!(const_name >= floor_value)`.
- **Why a skill:** A runtime test asserting a constant's value duplicates the constant. Compile-time form removes the duplication AND makes the failure mode louder (build error, not test error).
- **Rough shape:** Inputs — `(constant_path, floor_value)`. Outputs — a `const { assert!(...) }` block at the right module level. When to invoke — when adding a floor guard to a fixture or scale-dependent test.

## Takeaways

1. **The polish-after-scan cadence is the single most impactful workflow pattern from this plan.** Twelve repetitions, every one mechanical, all producing tighter code. Strongest candidate for skill-ification.

2. **Plan-review-time risk surfacing has high leverage.** The reviewer's pre-flag of two JSON-key consumers in Phase 3.4 prevented a Critical runtime regression. The plan-review step pays for itself when reviewers actually flag risks (not just verify completeness).

3. **Quality-scanner is intent-blind by design, which is its core value AND a cost source.** Same finding returned 6+ times because the scanner doesn't know "this is deferred." Dedup would preserve the value (catching real bugs) while compressing the cost (re-flagging known-deferred items).

4. **Cross-section cold-read scans catch what per-task scans cannot.** The Phase 4.5 polish scan found Major contradictions that 4.2/4.3/4.4 per-task scans missed. Worth promoting from ad-hoc orchestrator judgment to a documented workflow.

5. **Empirical probes for acceptance test fixtures are cheap insurance against silent false-greens.** Phase 5.1's probe step prevented a non-catching test from shipping. Trivial cost, catastrophic-failure-mode prevention.

6. **The `code-graph-mcp` project specifically needs:** a tool-description-audit Rust test (closes the loop on the stale-envelope finding pattern), and probably a CLAUDE.md update documenting the now-tested wire-format invariants (`Page<T>` 6 fields, `SymbolResult` no `file`, `id_to_file` as inverse contract). Both already done in the plan; the test would close the verification loop.
