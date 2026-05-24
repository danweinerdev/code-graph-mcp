---
title: "Phase 4 Debrief: Tree-shaped get_class_hierarchy + cutover"
type: debrief
plan: PaginationOverhaul
phase: 4
phase_title: "Tree-shaped get_class_hierarchy + cutover"
status: complete
created: 2026-05-07
updated: 2026-05-07
tags: [pagination, mcp, llm-optimization, scale, ue, unreal-engine]
---

# Phase 4 Debrief: Tree-shaped get_class_hierarchy + cutover

## Decisions Made

- **`max_nodes` budget counts unique class names, not visits.** Diamond inheritance was the load-bearing test case: a 4-unique-node hierarchy where a shared ancestor is reachable via 2 paths (5 total visits). The discriminating test (`max_nodes=4`, assert `truncated=false` + all 4 names present) would fail under a naïve visit-counter. Plan reviewer flagged this exact failure mode during /plan; the test was designed specifically to catch it. Verified passing.
- **`PopGuard` RAII for `on_path` cleanup.** The implementer added a small RAII struct to ensure the per-path cycle-guard set's pop happens even on panic during recursion. Locally scoped, not exported; defensive but not over-engineered.
- **`ClassHierarchyResponse` stayed private to `handlers/structure.rs`** — not promoted to a sibling of `Page<T>`. Tree shape is too different from list shape to share a generic without contortion (different fields, different semantics, different agent UX).
- **Default `max_nodes = 250`** (not 100). The plan reviewer caught a design self-contradiction during /plan (table said 100, rationale said 250); design was harmonized to 250 before /implement started. UE's `UObject` hierarchy at depth=2 still trips `truncated: true` — that's expected and the agent retry path handles it.
- **Tool description rewrites caught misleading agent guidance.** Quality scanner flagged two real bugs in the post-readability-pass `#[tool(description=…)]` text:
  1. `get_callers`/`get_callees` said "raise via offset for high fan-in" — but `offset` is a skip-count, not a "more results" lever. Correct guidance is "raise `limit`, use `offset` to page." Fixed.
  2. `get_class_hierarchy` said "Default `max_nodes` is 250 — large enough for typical depth=1/2 walks." Implies the default is a safe ceiling for depth-2, which it isn't on UE-scale codebases. Reworded to "sized to fit most hierarchies under the MCP token ceiling, but a single deep inheritance tree (e.g. UE's UObject) can exceed it. Watch for `truncated: true` and raise `max_nodes` (or narrow `depth`) when it fires."

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| `Graph::class_hierarchy` accepts `max_nodes`; tracks unique names; returns `(root, total_nodes_seen, truncated)` | Met | Algorithm + diamond test pass |
| Diamond fixture passes with no semantic change; new diamond+max_nodes test confirms unique-name counting | Met | Discriminator test verified by quality scanner |
| `GetClassHierarchyArgs` has `max_nodes`; handler wraps response in `{hierarchy, truncated, max_nodes, total_nodes_seen}` | Met | Field order matches design |
| Four existing class_hierarchy snapshots regenerated; one new truncated snapshot added; tools-list snapshot regenerated | Met | All approved via `cargo insta accept` |
| CLAUDE.md and README.md updated to reflect the new tool surface | Met | New "Response shapes" section in CLAUDE.md |
| Workspace-wide: zero pending snapshots, clippy-clean, fmt-clean, all tests passing | Met | 726 tests passing |
| Manual UE-scale smoke test confirms the original `get_orphans` token-limit failure is resolved | Deferred | Optional per plan; validated indirectly via new `paginated_offset` snapshot tests with 25-item synthetic fixtures |

## Deviations

- **Tool descriptions rewritten across 5 tools, not just `get_class_hierarchy`.** Task 4.9 specified "final readability pass on tool descriptions" for the 5 changed tools. The implementer did this and tightened wording across all of them — generated 5 tools-list snapshot updates instead of just 1 for class hierarchy. The 4 extra ones are intentional consolidation of inconsistencies introduced across phases 2/3/4. Worth flagging because the plan's snapshot count estimate ("1 modified hierarchy tools-list snapshot") undercounted by 4.
- **Two more tools-list snapshot updates after my own description fix.** The "raise via offset" → "raise limit" wording fix touched callers/callees descriptions, regenerating those tools-list snapshots a second time within the phase. Total tools-list snapshots in this phase's diff: 5 (callers, callees, class_hierarchy, file_symbols, orphans).

## Risks & Issues Encountered

- **Misleading agent-facing copy slipped through three review passes.** The "raise via offset for high fan-in" wording was wrong (offset is skip-count, raising it doesn't get more results). It survived the implementer's own readability pass, the per-task self-verification, and the workspace gates. Quality scanner caught it on a careful read. The cost was small (one more snapshot regeneration round) but the lesson is structural: agent-facing copy in `#[tool(description=…)]` macros is *production behavior* — agents read it and act on it — but it's reviewed like documentation. Need a quality-scan checklist that explicitly includes "every doc string that names an arg by name is operationally accurate."
- **Did-you-mean error path preservation was a real risk.** The Graph layer's signature changed from `Option<HierarchyNode>` to `Option<(HierarchyNode, u32, bool)>`; the handler had to thread the tuple unpack through the existing `if let Some(...) = ... else { suggest_class_symbols(...) }` shape. If the implementer had inverted the branch or dropped the `None` arm, agents would lose the helpful "Did you mean: AActor, AController?" suggestions. Verified preserved via reading the handler diff.
- **The plan's snapshot-audit step (4.10) caught zero accidental cross-tool changes.** Confirms that the strict file-by-file `git add` discipline + per-tool isolation in handlers worked. The 10 untouched tools-list snapshots showed zero diff, exactly as planned.

## Lessons Learned

- **Agent-facing tool descriptions are production code, reviewed like docs.** `#[tool(description=…)]` text isn't comments — agents pattern-match on it to decide how to call the tool. A misleading description ("raise offset for more results") is functionally a bug. Need to treat description copy with the same scrutiny as the implementations they document. Quality scanner caught it; manual review didn't.
- **Tree-shaped tools need `max_nodes` + flag, not pagination.** The decision to model class hierarchy with a budget rather than offset/limit was correct in retrospect. Trees can't be naively page-sliced (drop a middle branch and you've broken the parent-child invariants); a budget with a `truncated` flag lets the agent decide whether to retry with a larger budget or narrow the query. The `total_nodes_seen` field is small but load-bearing — without it, an agent has no signal of how big the cap-vs-reality gap is.
- **Field-declaration order is the wire-format contract; snapshots can't verify it.** Same lesson as Phase 1, now applied to the new envelope. The doc-comment on `ClassHierarchyResponse` makes this explicit. The pattern of declaring it inline in the source rather than relying on snapshot order is now consistent across all tool envelopes.
- **Plan reviewer's "discriminator test" framing was load-bearing.** The diamond test as originally drafted (`max_nodes=4` on a 4-node diamond) passed trivially regardless of unique-name vs visit counting. Reviewer caught this in /plan. The fix (specify `max_nodes < total_visits`) made the test actually discriminate. Worth remembering: a passing test doesn't mean the feature works — it means the test's assertions are satisfied. If the test would still pass with the bug present, it's documentation, not verification.

## Impact on Subsequent Phases

- **No subsequent phases.** This was the final phase. Plan moved Active → Complete; status `complete`.
- **Plan close-out prerequisites for the next plan:** the C++ macro-prefixed-class issue (also reported by the user during the same dogfooding session — `class CORE_API MyClass : public UObject` confuses the parser) is still open and tracked in `project_known_gaps_unreal.md`. That'd be a natural successor plan.

## Skill Opportunities

- **What you did repeatedly:** Reviewed `#[tool(description=…)]` text for agent-usability properties (does it document the new arg? does it explain when to deviate from defaults? is the suggested action operationally correct?). Did this implicitly across all 4 phases; got it wrong in two places that quality scanner caught.
  **Where it belongs:** A new `planner:scan-tool-descriptions` skill, or extension to the existing `quality-scanner` agent prompt with explicit checklist items: "every named arg is documented with default and ceiling," "the verb in the suggested action operationally produces the claimed result," "envelope shape is named, not implied."
  **Why a skill:** Tool description copy is high-leverage and easy to get subtly wrong. The cost of a misleading description is real (agent makes a wrong call) but the writer rarely tests their own copy by trying to follow it. A targeted scan would catch this faster.
  **Rough shape:** Reads `#[tool(description=…)]` strings in `crates/*/src/server.rs`, validates each against a checklist, flags deviations.

- **What you did repeatedly:** Hand-checked that the 10 unchanged tools-list snapshots showed zero diff after each phase's changes.
  **Where it belongs:** Already noted in Phase 2 debrief — `scripts/snapshot-audit.sh <expected-paths…>` would automate the "only these snapshots should change" assertion.
  **Why a skill:** This was load-bearing for Phase 4's confidence that no cross-tool effects leaked. Cheap to script; expensive (and easy to skip) to do by hand each time.
  **Rough shape:** Same as Phase 2's note — invoke from a phase's verification step or as a pre-commit hook.

- **What you did repeatedly:** Wrote per-phase commit messages by hand following the same conventions: `[PlanName/Phase N] <subject>` + body explaining what + why + tests + snapshot delta. Did this 4 times in the same session.
  **Where it belongs:** A `/planner:commit-phase` slash command, or a `make commit-phase PHASE=N` Makefile target that templates the commit message from the phase doc's frontmatter and acceptance criteria.
  **Why a skill:** Each commit took ~5 minutes to draft and was structurally identical. The phase frontmatter has the title, deliverable, and acceptance criteria already; the commit message is mostly a templated rendering of those + the snapshot delta. Automating the boilerplate frees attention for the parts that genuinely vary (the "what surprised us" lines).
  **Rough shape:** Reads `Plans/<Plan>/<NN>-<Phase>.md` frontmatter and acceptance criteria, opens an editor with a pre-filled commit message, runs `git commit -F <tmpfile>` after edits.
