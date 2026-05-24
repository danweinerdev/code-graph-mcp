---
title: "Phase 2 Debrief: get_class_hierarchy ref-dedupe"
type: debrief
plan: "ResponseShapePolish"
phase: 2
phase_title: "get_class_hierarchy ref-dedupe"
status: complete
created: 2026-05-15
updated: 2026-05-15
tags: [mcp, pagination, ue, unreal-engine, ergonomics, hierarchy, diagrams, coupling, dependencies, fuzzy-match]
---

# Phase 2 Debrief: get_class_hierarchy ref-dedupe

## Decisions Made

- **`get_class_hierarchy` description made the bidirectional walk explicit.** `Graph::class_hierarchy(name, depth, max_nodes)` has no `direction` parameter — it unconditionally walks both `bases` (forward `Inherits`/`adj`) and `derived` (reverse `Inherits`/`radj`). The plan's 2.3/2.4 verification text assumed a `direction` arg (`("D", up)`, `direction=Down`). Rather than perpetuate that, Task 2.4's rewrite states the no-direction reality outright ("There is no direction argument. The tree always walks BOTH directions").

- **The diamond fixture deliberately diverged from the literal plan shape.** Plan 2.3(a) specified a minimal D→{B1,B2}→A diamond. With the bidirectional walk, A's ref-stub on that minimal shape is a near-empty node, so the ≥20% byte-savings assertion would prove almost nothing. The implementer added a substantial derived subtree under B2 (Sub1/Sub2/Sub3, Inner→Sub1) so the deduped subtree is non-trivial. Observed **35% reduction (532 → 346 bytes)** — the assertion now actually proves the mechanism reduces size.

- **The depth-0 + sibling-reach asymmetry was accepted as intentional, not "fixed".** The Task 2.2 quality scan flagged: a name first reached at recursion `depth == 0` enters `visited_unique` (charging its budget slot) but returns before recursing; a *sibling* path that later reaches the same name hits the `visited_unique` check and emits a `{name, ref: true}` stub — even though the canonical occurrence is itself only a leaf. This is consistent with the `total_nodes_seen = unique names walked` contract (a name walked at depth 0 still counts). Decision: pin the behavior with a dedicated 4th regression test (`hierarchy_depth_zero_sibling_reach_emits_ref_stub`) and an inline invariant comment at the `if depth == 0 { return node; }` site that names the test — NOT change the walk.

- **Two existing diamond tests were rewritten, not merely supplemented.** `class_hierarchy_diamond_4_level_fixture` and `class_hierarchy_max_nodes_unbounded_matches_legacy` previously asserted the legacy inline-subtree-duplication behavior that this phase removes. Their assertions were rewritten to pin the new canonical-arm-full / second-arm-ref-stub shape. The replacement assertions still pin the load-bearing property (canonical arm fully expands; the second arm doesn't silently vanish — it becomes an explicit stub).

- **Quality-scanner Minor findings were fixed inline by the orchestrator**, not by re-dispatching the implementer. All were small doc-comment / wording / label-hygiene fixes; re-dispatch overhead exceeded the fix cost. Each batch was folded into the next task's commit or the phase-closeout commit (Task 2.5).

- **Per-task commits continued** (the pattern adopted mid-Phase-1). Five commits: `ee34e64`, `2ae1cfa`, `f8e2201`, `2b54428`, `08eb1ac`.

- **Cross-phase decision: bake a "no plan/task labels in source" rule into future implementer dispatch prompts.** Plan/task-label leakage into source comments was a recurring Minor quality finding in BOTH phases. Going forward, Phase 3/4/5 `code-implementer` prompts will carry an explicit up-front constraint: source comments and doc strings describe behavior only — never reference task IDs, phase numbers, or `.plans/` paths.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| `HierarchyNode.ref` field added with correct serde attributes | Met | `#[serde(default, skip_serializing_if = "Option::is_none")] pub r#ref: Option<bool>` after `bases`/`derived`. serde strips `r#` automatically — no `rename` needed. Smoke test pins byte-identical `{"name":"X"}` + a deserialization round-trip (added from a Task 2.1 quality finding). |
| `build_hierarchy` walk implements on_path → visited_unique → first-visit precedence | Met | Three-way branch at `algorithms.rs:271–312`. on_path FIRST (cycle → bare leaf, `r#ref: None`), visited_unique SECOND (diamond → `r#ref: Some(true)`, no recursion), else first visit. RAII `PopGuard` unwinds `on_path` panic-safely; `visited_unique` is global and never removed. |
| 3 test fixtures (diamond, no-diamond, cycle) pin the new behavior | Met + 1 | Four tests: `hierarchy_diamond_emits_ref_stub` (35% byte proof), `hierarchy_no_diamond_omits_ref_field` (literal `"ref":` absent), `hierarchy_cycle_emits_bare_leaf_not_ref` (on_path precedence), plus the extra `hierarchy_depth_zero_sibling_reach_emits_ref_stub` from the Task 2.2 scan. |
| Tool description rewritten and snapshot accepted | Met | `get_class_hierarchy` description names ref-stub semantics, cycle-leaf semantics, client-reconstruction guidance, no-direction fact, and correct defaults. Tool-list snapshot regenerated twice (once in 2.4, once in 2.5 for the quality-fix rewording). CLAUDE.md `### Response shapes` gained two `HierarchyNode`/`ref` sub-bullets. |
| `total_nodes_seen` semantics unchanged | Met | Ref-stubs do not increment `visited_unique`; budget gate consults `visited_unique` only. `class_hierarchy_diamond_counts_unique_names` green: a 4-name diamond with 5 visits reports `total == 4`. |
| `cargo clippy --workspace --all-targets -- -D warnings` clean | Met | Verified at every wave + phase close. |
| `cargo fmt --all --check` clean | Met | Same. |
| `make snapshot-clean` passes | Met | Same. |

Workspace: `cargo test --workspace` 1184 passed / 0 failed / 2 ignored at phase close.

## Deviations

- **Diamond fixture shape divergence (emphasized).** Plan 2.3(a) prescribed a minimal D→{B1,B2}→A fixture. The implementer expanded it with a substantial derived subtree under B2 so the ≥20% byte-savings assertion would actually exercise a non-trivial deduped subtree (a minimal fixture makes the assertion vacuous — the stub it saves is near-empty). This is a *test-design* deviation, not a behavior change: the test still asserts a ref-stub appears and the canonical subtree is fully expanded once. **Principle to carry forward:** a "proves the optimization works" assertion is only meaningful if the fixture is large enough that the optimization has something to save. Apply the same scrutiny to Phase 3's diagram-dedupe size assertion.

- **Two existing tests rewritten, not just added (emphasized).** `class_hierarchy_diamond_4_level_fixture` and `class_hierarchy_max_nodes_unbounded_matches_legacy` pinned the legacy subtree-duplication behavior that Phase 2 deliberately removes. They were rewritten to pin the new shape. This is *implicit scope* of the plan's "every existing hierarchy test passes" verification line — flagged here so a future reader diffing the test file does not mistake the rewritten assertions for unplanned drift. The original tests' regression-guard intent is preserved: a regression of the diamond fix now fails them via a different (still-load-bearing) assertion.

- **depth-0 asymmetry accepted, not fixed (emphasized).** The Task 2.2 quality scan surfaced a subtle inconsistency: a name first reached at `depth == 0` enters `visited_unique` and returns a bare leaf; a sibling path reaching the same name then emits a `{name, ref: true}` stub whose "canonical" is only that leaf. The decision was to treat this as the correct, contract-consistent behavior (`total_nodes_seen` counts names walked, including depth-0 leaves) and pin it with a 4th regression test + an inline invariant comment, rather than alter the walk. A future maintainer tempted to "fix" the asymmetry must first reconcile it with the `total_nodes_seen` contract — the inline comment and the named test exist to force that reckoning.

- **A 4th test fixture beyond the planned 3.** Plan 2.3 specified diamond / no-diamond / cycle. The depth-0 regression test was added on top, originating from the Task 2.2 quality scan, not the original plan.

- **`get_class_hierarchy` description corrected a latent imprecision.** The plan's verification text assumed a `direction` parameter that does not exist. The rewrite states the bidirectional reality. Not drift — a plan-text-vs-codebase gap the implementation surfaced and the description now documents honestly. (Same *class* of gap as Phase 1's `search_symbols(namespace="")`.)

## Risks & Issues Encountered

- **Plan/task-label leakage into source — now a confirmed 2-phase pattern.** Phase 1: a planning-artifact path in a doc comment. Phase 2: `// --- Task 2.3 fixtures ---` separator, `pre-Task-2.2` in a helper doc, stale `Task 2.6 wraps Graph in parking_lot::RwLock` / `Task 2.4` preamble references in `algorithms.rs` + `callgraph.rs`. Every instance was a Minor maintainability finding, fixed inline. **Resolution going forward:** an explicit "no plan/task labels in source comments or doc strings" constraint will be baked into every Phase 3/4/5 implementer dispatch prompt (user-confirmed). This moves the fix from post-hoc per-task to prevention at the source.

- **`ref: false` / `ref?: bool` overclaim (Task 2.4 scan).** The description said "`ref: false` are omitted" and CLAUDE.md notated `ref?: bool`. Because `r#ref` is only ever `None` or `Some(true)`, both phrasings implied an unreachable `"ref": false` wire state that an agent might write a dead deserializer branch for. Corrected to "`ref` is present only when `true` (never emitted as false)" / `ref?: true` in both files.

- **`sed -i` does not persist on harness-Edit-tracked files.** The phase doc had been touched via the Edit tool; a subsequent `sed -i` to bulk-toggle 34 `- [ ]` → `- [x]` checkboxes silently did not persist (the harness restores Edit-tracked file state). Worked around with `Edit(replace_all=true)`. This is the same friction the Phase 1 debrief flagged as a `/close-phase` skill opportunity — now with a concrete root cause: bulk file-munging on Edit-tracked artifacts must go through Edit, not shell tools.

- **`cargo insta accept --snapshot <name>` CLI mismatch (recurring from Phase 1).** Again had to manually `mv` the `.snap.new` to `.snap` for the `get_class_hierarchy` tool-list snapshot. Same brittle CLI wart; same `make snapshot-accept` skill opportunity.

## Lessons Learned

- **"Proves the optimization works" assertions need fixtures big enough for the optimization to bite.** The diamond test's byte-savings assertion would have been vacuous on the literal minimal fixture. The implementer caught this and scaled the fixture up. Generalize: any Phase 3+ "dedupe reduces size by ≥X%" assertion must be reviewed for whether the fixture actually contains a substantial duplicated payload — otherwise the test is green theater.

- **Plan verification text that names API shapes can encode parameters that don't exist.** Phase 1: `search_symbols(namespace="")` as a "global filter". Phase 2: `class_hierarchy("D", up)` / `direction=Down`. Both were plan-author assumptions the codebase did not honor, caught at implementation time. **Carry forward:** the `/implement` readiness audit should cross-check backtick-quoted API call shapes in verification fields against actual function signatures, not just check for forward references.

- **Intent-blind quality scanning keeps catching what intent-aware review forgives.** The plan/task-label leakage is invisible to a reviewer who knows the plan (the labels "make sense" in context); the intent-blind scanner reads them cold and correctly flags them as future-reader noise. This is the designed value of the intent-blind lane — and the recurring nature of this specific finding is what justified the systemic prevention rule rather than continued per-task patching.

- **Accept-and-pin can be the right answer to a quality finding.** The depth-0 asymmetry was a legitimate scanner observation, but the correct response was a regression test + invariant comment, not a code change — because changing it would have violated the `total_nodes_seen` contract. Not every Minor finding wants a code edit; some want a test that says "this is deliberate."

- **Bulk artifact mutation must use the harness's own edit path.** `sed -i` on Edit-tracked files is a silent no-op in this harness. This is now a confirmed, reproducible constraint — relevant to the `/close-phase` skill design (it must toggle checkboxes via the Edit mechanism or run entirely outside the harness's file tracking).

## Impact on Subsequent Phases

- **Phase 3 (`generate_diagram` direction + dedupe + file-leak fix)** is the closest structural sibling to Phase 2 — both add a dedupe mechanism with a "this actually reduces size" proof obligation. Reuse Phase 2's pattern: build a fixture with a *substantial* duplicated payload, serialize both the deduped and re-expanded forms, assert a real percentage reduction. Do NOT accept a minimal fixture where the saved payload is trivial.

- **Phase 3/4/5 implementer prompts will carry the "no plan/task labels in source" constraint up front.** This is a standing change to how this orchestrator dispatches the remaining `code-implementer` agents for this plan, decided in this debrief.

- **Phase 6 (CLAUDE.md sweep)** must keep the `HierarchyNode` shape + ref-stub contract (now in `### Response shapes`) consistent with the final aggregate state, and must NOT let the MCP-tools-table entry for `get_class_hierarchy` re-imply a `direction` argument. The no-direction fact is now load-bearing documentation.

- **Phase 6 readiness check.** When Phase 6's CLAUDE.md sweep runs, re-verify the `get_class_hierarchy` description still matches the handler defaults (`depth` 0→1, `max_nodes` 0→250, >1000→1000) — Task 2.4 verified these against `structure.rs`, but Phase 4's cache-schema work and Phase 5's `detect_cycles` changes could perturb shared helpers.

- **`total_nodes_seen` contract is now test-pinned** (`class_hierarchy_diamond_counts_unique_names`). Any future change to `build_hierarchy`'s `visited_unique` accounting will fail that test — subsequent phases touching this file should expect it as a tripwire.

## Skill Opportunities

Phase 2 reinforced all three Phase 1 skill opportunities and added concrete root-cause detail:

### 1. `/sdd-planner:close-phase` — atomic phase closeout (reinforced + root cause found)

- **What recurred:** end-of-phase requires (a) toggling every `- [ ]` → `- [x]` in the phase doc, (b) `status: complete` in the phase doc frontmatter, (c) `status: complete` in the plan README `phases[]`, (d) `updated:` bumps. Phase 2 added a concrete failure: `sed -i` for the bulk checkbox toggle is a **silent no-op on harness-Edit-tracked files** — the harness restores Edit-tracked state, so shell munging doesn't persist. Had to fall back to `Edit(replace_all=true)`.
- **Where it belongs:** `/sdd-planner:close-phase` slash command.
- **Why a skill:** prevents the silent `sed` no-op trap entirely (the skill would use the Edit path or run outside harness tracking), and prevents the asymmetric phase-doc-vs-README status drift the Phase 1 debrief already flagged.
- **Rough shape:** `/sdd-planner:close-phase <plan> <phase-id>` → bulk-checks subtasks via the Edit mechanism, sets both `status: complete` fields, bumps both `updated:`, reports the diff, and (if final phase) moves the plan to `Complete/`.

### 2. `make snapshot-accept FILE=<name>` — normalize the insta accept dance (reinforced)

- **What recurred:** `cargo insta accept --snapshot <name>` mismatched again for the `get_class_hierarchy` tool-list snapshot; manual `mv .snap.new .snap` required (twice this phase — Task 2.4 and Task 2.5 each regenerated it).
- **Where it belongs:** Makefile target.
- **Why a skill / rough shape:** unchanged from the Phase 1 debrief — `make snapshot-accept FILE=<stem>` finds the `.snap.new`, promotes it, runs `make snapshot-clean` to confirm nothing else pending, errors if zero or >1 match.

### 3. `make test-summary` — aggregated test counts (reinforced)

- **What recurred:** the `cargo test --workspace | awk` one-liner was used at every wave and at phase close. Stable now but still hand-rolled.
- **Where it belongs / rough shape:** Makefile target — unchanged from Phase 1 debrief.

### 4. NEW — "no plan/task labels in source" prevention (decided this debrief)

- **What recurred:** plan/task-label leakage into source comments and doc strings, both phases, every time a Minor quality finding.
- **Where it belongs:** primarily a standing constraint in the `/implement` orchestrator's `code-implementer` dispatch prompt (user-confirmed approach). Optionally also a `make check-no-plan-refs` grep target for mechanical CI enforcement.
- **Why a skill:** moves a recurring post-hoc fix to prevention at the point of code generation. The dispatch-prompt constraint is zero-infrastructure and immediate; the grep target is a defense-in-depth follow-on if leakage persists despite the prompt rule.
- **Rough shape (grep target):** `make check-no-plan-refs` → `grep -rnE '(Task [0-9]+\.[0-9]+|Phase [0-9]+|\.?plans/|Plans/Active)' crates/*/src/` → non-zero exit on any hit, with file:line output.
