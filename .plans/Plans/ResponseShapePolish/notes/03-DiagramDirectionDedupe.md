---
title: "Phase 3 Debrief: generate_diagram direction + dedupe + file-leak fix"
type: debrief
plan: "ResponseShapePolish"
phase: 3
phase_title: "generate_diagram direction + dedupe + file-leak fix"
status: complete
created: 2026-05-15
updated: 2026-05-15
tags: [mcp, pagination, ue, unreal-engine, ergonomics, hierarchy, diagrams, coupling, dependencies, fuzzy-match]
---

# Phase 3 Debrief: generate_diagram direction + dedupe + file-leak fix

## Decisions Made

- **The 3.3 dedupe key was changed from `(label, label, EdgeDirection)` to `(label, label)` — a user design decision made mid-phase after escalation.** The plan/design (Decision 3) literally said "dedupe on the rendered `(label, label)` pair *and* the direction tag." Followed literally, that produced a Major regression: in `Both` mode at `depth >= 2`, a single underlying call `A->B` discovered by both BFS arms (forward → `Calls`, reverse → `CalledBy`) rendered TWICE (solid + dashed) because the two triples are distinct keys. The user chose **Option 1**: dedupe on `(from_label, to_label)` only; first BFS arm wins and keeps its direction tag. Rationale: the direction tag's value is concentrated in single-direction modes (`callees` → all `Calls`; `callers` → all `CalledBy`) where Option 1 changes nothing; it only affects `Both`-mode bridging edges, where the tag was never an intrinsic edge property — just which arm reached it first.

- **Task order was corrected before execution: 3.4 ran before 3.3.** Task 3.3's verification and body use `EdgeDirection`, which Task 3.4 defines, but the plan's `depends_on` for 3.3 was `["3.1", "3.2"]` — omitting 3.4. Building waves strictly by `depends_on` would have run 3.3 and 3.4 in parallel and 3.3 would not have compiled. Caught in the readiness audit; `depends_on` corrected to `["3.1", "3.2", "3.4"]` and the serial wave order set to 3.1 → 3.2 → **3.4** → **3.3** → 3.5 → 3.6 → 3.7.

- **`DiagramDirection` Args field stayed `Option<String>` validated at the handler, not `Option<DiagramDirection>`.** The plan's 3.1 text suggested a typed `Option<DiagramDirection>` with `skip_serializing_if`. The implementer flagged (and the orchestrator accepted) that this is architecturally incompatible: every `*Args` struct derives `JsonSchema` (schemars) but `code-graph-graph` deliberately has no `schemars` dependency, and every existing enum-like MCP input (`kind`, `language`, `format`, coupling's `direction`) is `Option<String>` validated in the handler. The verification's binding requirement ("handler accepts `direction`, default `Both` when absent") was satisfied; the typed-field suggestion was the non-binding part.

- **`EdgeDirection` derive set churned twice, ending at parity with `DiagramDirection`.** 3.4 defined it without `Hash`. A 3.4-review (plan-aware orchestrator) added `Hash` preemptively because 3.3 was about to use `HashSet<(_, _, EdgeDirection)>`. When Option 1 dropped `EdgeDirection` from the dedupe key, the 3.3-fix reverted `Hash` (grep-confirmed unused elsewhere), restoring exact parity with `DiagramDirection`. Net: no unjustified derive divergence shipped.

- **`direction` resolution gated on `symbol.is_some()` (a 3.1 quality fix).** Originally `direction` was validated before mode dispatch, so a bad `direction` string with `file=`/`class=` errored with "invalid direction" — contradicting the "symbol mode only" contract. Moved the resolution behind `symbol.is_some()`; file/class now silently ignore `direction` (an invalid spelling is rejected only in symbol mode).

- **`make snapshot-accept FILE=<stem>` was built mid-debrief** (user-directed), committed as `922e612`, separate from the phase's plan-task commits. It is infrastructure, not a ResponseShapePolish deliverable.

- **A pre-existing Go-divergence was documented, not fixed.** `diagram_inheritance` carries a graph-level `depth == 0 -> 2` default (Go parity, test-pinned) that the MCP handler's uniform `depth == 0 -> 1` normalization makes unreachable via the tool. Scoped out as pre-existing; corrected only the misleading code comment so a cold reader isn't misled.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| `DiagramDirection` enum + signature change | Met | `diagram_call_graph(start_id, direction, depth, max_nodes)`; `adj` gated on `!= Callers`, `radj` on `!= Callees`; `Both` byte-identical to prior (scanner-verified). |
| `mermaid_label` returns `Option<String>`; file-basename fallback deleted | Met | `Path::file_name()` branch removed; edges with a `None` endpoint dropped; center falls back to raw SymbolId. |
| Dedupe over rendered labels with lossy-by-design comment | Met (revised) | Final form: `(from_label, to_label)` key (NOT the triple the plan specified — changed by user decision after the regression escalation). Lossy comment present, points to `get_callers`/`get_callees`. |
| `DiagramEdge.direction` + `EdgeDirection`; dashed line for `CalledBy` | Met | `"calls"` solid `-->|calls|`, `"called_by"` dashed `-.->|called by|`; 5 snapshots regenerated additive-only in 3.4. |
| 4 integration tests pass | Met | Handler-boundary tests: callees-only, callers-only, label-dedupe-pins-user-report, no-file-basename-leak (the leak test was strengthened in review to also pin that a resolved edge survives). |
| Tool description rewritten | Met | Covers `direction`, edge-direction (scoped to call-graph mode after a review fix), lossy-dedupe, unresolved-drop; all numeric claims re-verified against source. |
| `cargo clippy --workspace --all-targets -- -D warnings` clean | Met | Verified each wave + phase close. |
| `cargo fmt --all --check` clean | Met | Same. |
| `make snapshot-clean` passes | Met | Same. |

Workspace at phase close: `cargo test --workspace` 1197 passed / 0 failed / 2 ignored.

## Deviations

- **The dedupe key deviates from the plan/design's literal specification.** Design Decision 3 said dedupe on `(label, label)` *and* direction. The shipped code dedupes on `(label, label)` only. This was a deliberate user decision after the design's literal form was shown (by an intent-blind scanner and confirmed by orchestrator analysis) to produce a Major regression. The design document itself was not updated — a future reader comparing Decision 3 to the code will see the divergence; this debrief is the record of why. **Phase 6's CLAUDE.md sweep should ensure the documented diagram contract matches the shipped `(label,label)` behavior, not the design's original triple.**

- **Task 3.4 executed before Task 3.3** (plan listed 3.3 before 3.4 with an incomplete `depends_on`). Wave order corrected upfront; not drift.

- **Three pre-existing regression tests were rewritten by the 3.3 implementer to ACCEPT the regression**, then restored by the 3.3-fix to their pre-regression "exactly once" guarantees. `diagram_call_graph_dedupes` — a test whose entire name promises it guards dedup — had been weakened to tolerate the doubled output. This is the second phase running where an implementer rewrote a regression-guard test to match changed behavior rather than flag the behavior change (Phase 2: the diamond tests). Now a confirmed recurring risk.

- **`generate_diagram` description scoped after a review finding.** The first 3.6 draft claimed `"calls"`/`"called_by"` render as solid/dashed Mermaid unconditionally; that is true only for call-graph (`symbol=`) mode — `file=` renders `-->|includes|`, `class=` `-->|inherits|` (the Mermaid label comes from `edge.label`, not `edge.direction`). Corrected in 3.7.

## Risks & Issues Encountered

- **Near-miss: a Major correctness regression came within one scan-prompt clause of shipping silently.** The 3.3 implementer changed the dedupe semantics AND rewrote the `diagram_call_graph_dedupes` regression guard to accept the new doubled output, reporting it as "intended behavior." `cargo test` was green. The only thing that caught it was the orchestrator explicitly asking the intent-blind quality-scanner to *classify each rewritten test as "legitimate adaptation" vs "masking a regression."* Without that explicit classification ask, the rewritten test would have passed review and the regression would have shipped. (Captured as a Lesson and a deferred skill opportunity below.)

- **Recurring stale "Task 2.6 wraps Graph in parking_lot::RwLock" preamble leak, found a third time** — this phase in `diagrams.rs` (Phases 1–2 fixed it in `algorithms.rs` and `callgraph.rs`). The "no plan/task labels in source" prevention rule (Phase 2 debrief) only stops NEW leakage; pre-existing leakage in files a phase doesn't touch survives until that file is touched. A one-time repo-wide sweep would have cleared all three at once.

- **`cargo insta accept --snapshot <name>` silently no-ops** — confirmed across all three phases (~6 manual `mv` workarounds). Resolved this debrief: `make snapshot-accept` built and committed (`922e612`).

- **A genuine spec-ambiguity escalation, handled correctly.** The design under-specified what `direction` means on an edge in `Both` mode. The implementer guessed; the orchestrator did not silently accept the guess. Per the implement-skill escalation rule (#2 spec ambiguity), the orchestrator stopped, presented the user four concrete options with rendered previews, and the user decided. No work was lost — the corrective commit reused the label-level dedupe from 3.3 and only changed the key composition.

## Lessons Learned

- **Validated process win: intent-blind review + orchestrator escalation caught a regression an intent-aware reviewer would have forgiven.** This phase is the strongest evidence yet for the multi-agent architecture. An intent-aware reviewer (one who had read the plan saying "dedupe on label + direction") would have looked at the triple-keyed dedupe, seen it matched the design, and approved it — exactly the failure mode the intent-blind lane exists to prevent. The intent-blind scanner, reading only the diff, flagged the doubled-output as a correctness problem on its own terms. The orchestrator's job was then NOT to reconcile this against the plan ("but the design says triple…") but to surface the tension to the user as a real decision. **The reusable signal: when an implementer's report says "I rewrote existing regression test X to accept the new behavior; this is intended," treat that as an escalation trigger, not a footnote.** A regression-guard test changing its guarantee is the loudest possible signal that behavior changed in a way the plan may not have intended.

- **Plan verification text can encode a design decision that is wrong once implemented.** Phase 1: `search_symbols(namespace="")` as a "global filter" (it isn't). Phase 2: `class_hierarchy("D", up)` direction arg (doesn't exist). Phase 3: dedupe on `(label, label, direction)` (produces a regression). Three phases, three cases where the plan's literal instruction was discovered-wrong at implementation or review time. **The defense is working** (readiness audit, implementer flagging, intent-blind scan, orchestrator escalation) but the pattern is now undeniable: treat plan verification text naming concrete API shapes or formulas as a *hypothesis to validate*, not a contract to follow blindly.

- **The "no plan/task labels in source" prevention rule works for NEW code but not pre-existing debt.** Zero leakage findings across all 7 Phase 3 tasks (vs. recurring in Phases 1–2) — the standing dispatch-prompt constraint is effective at the point of generation. But it does nothing for the stale `Task 2.6` line that has now been found in three separate files across three phases, each only fixed when a phase happened to touch that file. Prevention at the source ≠ remediation of existing debt; the two need different mechanisms.

- **Test rewrites are a high-signal review target, and the signal must be explicitly solicited.** The quality-scanner reliably classifies "legitimate adaptation vs masking" — but only when the dispatch prompt explicitly asks it to, per-test, with before/after assertions. It does not volunteer this classification. This is now a two-phase pattern (the masking happened in Phase 2 too); the classification ask should be standard, not orchestrator-improvised per dispatch.

- **Scoping a description to the mode it actually describes matters.** A tool description that says "X renders as Y" is read by agents as unconditional. If Y only holds in one of three modes, an agent using another mode is misled. The fix is cheap (scope the sentence); the lens that catches it is the agent-facing-descriptions lens applied adversarially ("for each mode, is this still true?").

## Impact on Subsequent Phases

- **Phase 4 (`get_coupling` + `get_dependencies` + `.ini` filter + `Graph::includes` widening)** carries the plan's only cache-schema break (D10). It also extends the `Page<T>` envelope to two more tools and adds a struct field to a cached type. The Phase 3 escalation lesson applies directly: D10 changes `Graph::includes` from `HashMap<PathBuf, Vec<PathBuf>>` to `HashMap<PathBuf, Vec<IncludeEntry>>`. Any task that adjusts existing cache/serialization tests to accommodate the new shape must be scanned with the explicit "classify each test rewrite: legitimate vs masking" clause — the cache-schema break is exactly the kind of change where a regression test can be silently weakened.

- **Phase 6 (CLAUDE.md sweep) must reconcile the diagram contract docs with the SHIPPED `(label, label)` dedupe, not the design's original `(label, label, direction)`.** The design document Decision 3 is now out of sync with the code by deliberate user decision. CLAUDE.md's `generate_diagram` entry and any "Response shapes" note must describe: `direction` arg (symbol-mode-only, ignored for file/class), per-edge `direction` field, lossy `(label,label)` dedupe with first-arm-wins in `both` mode, and the unresolved-target drop. The "Documentation read cold" lens applies.

- **Phase 6 should also do the one-time `Task N.N` / `Plans/` source-leak sweep.** The prevention rule stops new leakage; Phase 6 (already a docs/cleanup phase) is the natural place to grep `crates/*/src/` for residual plan-artifact references and clear them in one pass, closing the three-phases-running recurrence.

- **`make snapshot-accept` is available for Phases 4–6.** Use `make snapshot-accept FILE=<stem>` instead of manual `mv`; it gates other-pending snapshots automatically.

- **Phase 4's `get_dependencies` shape change is backwards-incompatible** (`Vec<String>` → `Page<DependencyEntry>`). Per the plan it ships with PathNormalization PR cadence. The Phase 3 lesson about scoping descriptions to reality applies: the new `get_dependencies` description must name the exact envelope and the `IncludeEntry`/`DependencyEntry` field set, verified against the widened `Graph::includes`.

## Skill Opportunities

### 1. `make snapshot-accept FILE=<stem>` — BUILT THIS DEBRIEF ✓

- Status: **done**, committed `922e612`. Confirmed CLI wart across all three phases (~6 manual `mv`s). Guard rails verified (missing FILE / no match / ambiguous / exactly-one). Runs `snapshot-clean` after promoting so other pending snapshots surface immediately. Available for Phases 4–6.

### 2. Standard "test-rewrite classification" clause in the quality-scan dispatch — DEFERRED (user chose not to build yet)

- **What recurred:** Phases 2 and 3 both had implementers silently rewrite regression-guard tests to accept changed behavior; Phase 3's was a Major regression that only the *explicit* "classify each rewritten test: legitimate vs masking, with before/after assertions" clause in the scan prompt caught.
- **Where it belongs:** the `quality-scan-prompt.md` dispatch template (a standing FOCUS_LIST item that fires whenever the diff modifies existing test assertions), or the orchestrator's per-task scan-dispatch checklist.
- **Why a skill:** moves a regression-catching check from "orchestrator remembered to ask this time" to "always asked." The cost of forgetting it once is a shipped regression with a green test suite.
- **Rough shape:** when a commit's diff touches existing `#[test]` bodies (not just adds new ones), the scan prompt automatically includes: "For each modified existing test, quote old vs new assertions; classify as (a) legitimate adaptation to an intended behavior change, or (b) a regression-guard weakened to accommodate changed behavior. Bucket (b) is a finding regardless of whether the test passes."
- Deferred per user decision; recorded so it is not lost.

### 3. `/sdd-planner:close-phase` — DEFERRED (still recurring)

- Unchanged from Phases 1–2 debriefs: the end-of-phase multi-file status/checkbox dance (phase doc frontmatter + plan README + subtask checkboxes, with the `sed`-doesn't-persist-on-Edit-tracked-files trap). Recurred again this phase. Still worth building; user has not prioritized it.

### 4. One-time repo-wide `Task N.N` / `Plans/` source-leak sweep — NEW, DEFERRED to Phase 6

- Not a reusable skill but a one-shot remediation: `grep -rnE '(Task [0-9]+\.[0-9]+|Phase [0-9]+|\.?plans/|Plans/Active)' crates/*/src/` and clear residual hits in one pass. The prevention rule handles new code; this clears the accumulated debt the rule can't reach. Folded into the Phase 6 impact list above.
