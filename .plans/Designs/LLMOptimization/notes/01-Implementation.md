---
title: "LLMOptimization Implementation Debrief"
type: debrief
plan: "LLMOptimization"
phase: 1
phase_title: "Implementation"
status: complete
created: 2026-04-28
---

# LLMOptimization Implementation Debrief

## Decisions Made

- **Run as design-only, no formal Plan phases.** All seven changes were small, mostly handler-level, and shared a single principle ("default to LLM-optimized output, verbose behind a flag"). A multi-phase plan would have been ceremony for what was effectively one cohesive diff.
- **`brief=true` as the default for both `search_symbols` and `get_file_symbols`.** The design's Decision 1 articulated this principle but only nailed down `search_symbols`. During implementation `get_file_symbols` shipped with `brief=false`; code review caught the inconsistency and it was flipped to `true`.
- **Per-DFS-path cycle protection in `buildHierarchy`.** The original implementation used a global `visited` map to prevent cycles, which silently truncated diamond inheritance on the second branch. Switched to per-DFS-path tracking (`onPath` + `defer delete`) so siblings each fully expand a shared ancestor while true cycles still break.
- **Wider candidate pool in `suggestSymbols`.** When `Search` was reworked with a default `Limit: 20`, the legacy `SearchSymbols(name, "")` wrapper used by did-you-mean inherited that default and silently shrank from 100 to 20 candidates. Fixed by passing `Limit: 100` explicitly at the call site rather than changing the default.
- **Optional `file` parameter on `get_symbol_summary`.** Decision 3 in the design called for it but it was missed during implementation. Added in the post-review pass; design doc updated to match the new signature `SymbolSummary(file string)`.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| Signature truncation at `{`/`;` with byte fallback | Met | UTF-8 boundary bug found in review (`s[:200]` could slice mid-rune); fixed to `s[:i] + "..."`. New test covers both fallback and UTF-8 boundary. |
| `namespace` filter on `search_symbols` | Met | Case-insensitive substring match. Empty-namespace = no-filter test added in review pass. |
| `brief` mode on `search_symbols` (default true) | Met | Verified by `TestSearchBriefDefault` / `TestSearchBriefFalse`. |
| Pagination on `search_symbols` | Met | `{results, total, offset, limit}` envelope; default Limit=20. |
| `top_level_only` on `get_file_symbols` | Met | Plus unplanned `brief` parameter (default true) for consistency with Decision 1. |
| New `get_symbol_summary` tool | Met | Optional `file` scope parameter added in post-review pass. |
| `depth` on `get_class_hierarchy` | Met | Plus diamond-inheritance fix beyond design scope. New `TestClassHierarchyDiamond` regression test (4-level chain to actually exercise the fix). |

## Deviations

- **Added `brief` parameter to `get_file_symbols` (not in design).** Aligned with Decision 1's principle but not enumerated under Change 5. Design doc updated retroactively.
- **Diamond-inheritance bug fix is beyond design scope.** The design only asked for `depth` traversal; the existing single-level cycle guard had a latent bug that depth amplified. Fix landed in the same diff.
- **UTF-8 boundary fix in `truncateSignature` is beyond design scope.** Design called for byte-count fallback; the spec said "200 chars" but the implementation used byte offsets, which sliced mid-rune. Fixed to a rune-safe boundary in the same diff.
- **`null` vs `[]` JSON encoding in `handleGetFileSymbols`.** Refactor introduced `var results []symbolResult` (nil slice) which marshalled as `null` when `top_level_only` filtered everything. Fixed to `make([]symbolResult, 0, len(symbols))`. Not a design deviation — just a regression to catch.

## Risks & Issues Encountered

- **First version of `TestClassHierarchyDiamond` was insufficient.** The 3-class diamond at depth=2 produces identical output under both buggy and fixed code because the shared node bottoms out as a leaf either way. Asked back to verify, traced through both implementations, and rewrote the test as a 4-level chain (`Root ← Base ← {MixinA, MixinB} ← Derived ← Leaf`) at depth=3 so the shared node has its own children that the second arm must expand. Verified by temporarily reverting the fix and watching the test fail.
- **Code review surfaced four real defects post-implementation.** All four (diamond, UTF-8, null array, suggestSymbols cap) shipped in the same diff. Three of the four had clean reproducers attached by the reviewers (one in `/tmp/visitedtest`, one in `/tmp/trunctest`, one in `/tmp/nulljson`). Reviewers building executable reproducers turned out to be a strong forcing function for separating real bugs from theoretical concerns.
- **`drift-detector` and `spec-compliance` disagreed on severity of `brief` default mismatch.** `drift-detector` called it Minor approach drift (defensible per-tool exception); `spec-compliance` called it a Major contract violation. The disagreement itself was the finding: the design principle was clear but the implementation block for Change 5 was silent. Resolved by aligning code to the principle and updating the design.

## Lessons Learned

- **Design-only workflow works for cohesive small-batch changes**, but make sure the design's "Implementation" subsections restate cross-cutting principles, not just declare them once at the top. Decision 1's "default to LLM-optimized" was clear in the abstract but absent from Change 5's implementation block, which is exactly where it would have caught the `brief=false` mismatch before code was written.
- **Cycle guards in tree-shaped output should be per-DFS-path, not global.** Global `visited` is right for true graph traversal where every node's identity matters once; for tree expansion where each occurrence is its own subtree, it silently truncates DAGs. Worth searching the rest of the graph code for the same pattern next time.
- **Tests need to actually expose the bug they're guarding against.** The first `TestClassHierarchyDiamond` *passed for both implementations* — it would have provided false confidence. Always include a "verify the test fails on the broken code" step when adding regression tests for newly-fixed bugs.
- **`Limit` defaults are load-bearing for downstream callers.** Changing `Search`'s default from "no cap" (effectively 100) to 20 is correct for the new tool surface but silently regressed the legacy wrapper. When introducing defaults that change behavior, audit existing call sites rather than assuming they'll keep working.
- **Adversarial fresh-eyes review pulls weight.** Three of the four post-review defects came from `quality-scanner` and `blind-spot-finder` together; the spec/drift lanes alone would have caught the missing `file` parameter and the brief-default mismatch but missed the UTF-8/null/cap bugs. The diff-only adversarial reviewer with no plan/spec context independently rediscovered three of them.

## Impact on Subsequent Phases

No subsequent phases — this design is implemented and complete. Carry-overs for related future work:

- **Other languages adopting the parser interface** (Go, Python, Rust per `Plans/Active/{GoParser,PythonParser,RustParser}/`) inherit `truncateSignature` semantics. The byte-fallback path is now UTF-8 safe, but each language parser may have additional signature shapes that benefit from earlier truncation hooks.
- **`generate_diagram` already has a `max_nodes` cap;** `get_class_hierarchy` does not. If users start passing very large `depth` values this becomes a real concern. Tracked as an open question, not blocking.
- **`Search` materializes all matches before sorting/slicing.** Fine for current graph sizes; if a 100K-symbol codebase becomes a hot path through search, an early-exit approach may be needed.

## Skill Opportunities

- **What:** Repeated manual sequence — running `/code-review`, then translating each finding into a TaskCreate entry, then implementing them one by one.
  **Where:** New `/planner:apply-review` slash command, in the `planner` plugin.
  **Why:** The mapping from review output → task list → code edits is mechanical but currently done by hand. A skill that consumes the synthesized `/code-review` output and seeds a task per finding would remove the bookkeeping step and make the loop tighter.
  **Rough shape:** input is the synthesis section of a `/code-review` report (or its raw sub-reports); output is a populated task list with one task per finding (subject = severity + summary, description = location + recommendation), then the user invokes `/implement` (or just works the list directly).

- **What:** Reviewers building standalone reproducers in `/tmp` to validate findings before reporting.
  **Where:** Update the `planner:blind-spot-finder` and `planner:quality-scanner` agent prompts to make this explicit, not implicit.
  **Why:** It happened naturally for three of the four bugs in this review and was the difference between "real defect with clear repro" and "theoretical concern." Codifying it raises the floor for adversarial reviews.
  **Rough shape:** add a one-line directive in the agent prompts: "When a finding depends on runtime behavior (encoding, JSON output, traversal order), build a minimal reproducer with `go run` (or equivalent) and include the observed output as evidence."

- **What:** Verifying a regression test actually fails on the buggy code before declaring it a regression test.
  **Where:** `/planner:implement` should remind the implementer to do this when they're adding a test for a bug fix; or it could be a small `/planner:verify-regression-test` skill.
  **Why:** The first diamond-inheritance test passed under both old and new implementations and would have given false confidence. The fix is mechanical (revert the patch hunk, run the test, watch it fail, restore) but it's also forgettable.
  **Rough shape:** input is a test name + the patch hunk that fixed the bug; output is a confirmation that the test fails when the patch is reverted and passes when restored. Could be a `make verify-regression TEST=... HUNK=...` target.

- **What:** Adapting `/code-review` (and `/debrief`) to design-only workflows where there's no formal Plan with phases.
  **Where:** Documentation update in the `planner` skills, or a small fork: `/planner:design-review` and `/planner:design-debrief` that take a Design path instead of a Plan path.
  **Why:** Both skills hard-code "Plans/Active/<PlanName>" assumptions. We adapted by treating the design as the plan and the diff as the phase, but it required orchestrator-level interpretation. Other small features will hit the same friction.
  **Rough shape:** the existing skills accept a `--design <path>` flag (or auto-detect a Design vs Plan input); debrief output lands at `Designs/<Name>/notes/<NN>-Implementation.md` instead of `Plans/Active/<Name>/notes/`.
