---
title: "Resolver Candidate Count"
type: phase
plan: GraphPlatformExpansion
phase: 9
status: in-progress
created: 2026-08-08
updated: 2026-08-19
deliverable: "Edges record how many same-named candidates competed for their target, and the tools that report edges surface it — replacing a one-bit heuristic tag with the number a caller can act on."
tasks:
  - id: "9.1"
    title: "Record candidate count on the edge and bump the cache format"
    status: complete
    justifies: "FR-48, AC-57, D-0007. Confidence::Heuristic is a one-bit projection of 'N candidates competed'. The resolver knows N at the moment it picks, and then throws it away — so the information a caller needs to disambiguate is destroyed at index time and cannot be recovered by any downstream change."
    verification: "cargo test -p code-graph-graph persist and cargo test -p code-graph-lang resolve (substring filters — the suites live in in-file tests modules, so a module-path :: filter would select zero; wording corrected at completion per the phase-6/7 filter-drift lesson) — an edge resolved from a single candidate records 1; an edge resolved from N same-named candidates records N; the value survives a cache save/load round-trip; CACHE_VERSION is bumped and an older cache is silently re-indexed rather than misread (existing version-mismatch path); make verify passes."
  - id: "9.2"
    title: "Surface candidate count on the edge-reporting tools"
    status: complete
    justifies: "FR-48, AC-57. Storing the count without exposing it satisfies nothing — AC-57 requires a caller to distinguish 'one candidate, unambiguous' from 'five candidates, one picked by scope rule' without another query."
    verification: "cargo test -p code-graph-tools — get_callers, get_callees, find_path, and generate_diagram each expose the count on the edges they report; a caller can tell a 1-candidate edge from an N-candidate one in a single response (AC-57); existing response snapshots are rebaselined deliberately and the change is additive, so a client reading only today's fields still parses."
    depends_on: ["9.1"]
  - id: "9.3"
    title: "Retire or demote the binary confidence tag on the wire"
    status: complete
    justifies: "D-0007, FR-23. Once N is available, carrying both N and a derived one-bit tag gives an agent two overlapping signals and invites it to reason from the weaker one. Leaving both is the outcome D-0007 exists to prevent."
    verification: "Review each wire type that currently carries a binary confidence tag and either remove it or document why both are needed; cargo test -p code-graph-tools passes with the resulting shapes; CLAUDE.md's Response shapes section and every affected tool description state what the count means and how it relates to min_confidence filtering (NFR-11)."
    depends_on: ["9.2"]
---

# Phase 9: Resolver Candidate Count

## Overview

`Confidence::Heuristic` means "at least two indexed candidates shared this name and the resolver picked one by scope rule." The count of competing candidates — the thing a caller could actually act on — is computed at resolve time and immediately discarded.

This phase preserves it. It arrives from the Phase 1 review: `find_path` shipped a per-hop resolved/heuristic tag, and the question "what is actually valuable to an agent consuming this?" produced D-0007 — an indicator earns its place by removing a round-trip or naming the next action. "Three candidates competed, and here is the one chosen" does both. "Heuristic" does neither.

Independent of every other phase except its own ordering; `depends_on: [1]` only because `find_path` is one of the surfaces it changes.

## 9.1: Record candidate count on the edge and bump the cache format

### Subtasks
- [x] Capture the candidate count in the resolver at the point the target is chosen
- [x] Add the count to the edge record and to its packed representation
- [x] Bump `CACHE_VERSION`; confirm the existing version-mismatch path silently re-indexes rather than misreading
- [x] Round-trip tests through save/load
- [x] Decide and document what the count is for a declarative edge (Inherits, Overrides, `mod`-resolved Includes) — these have exactly one candidate by construction

### Notes
Revision boundary: edges carry the count and it survives the cache; nothing exposes it yet.

This is the only task in the plan that changes the cache format. That is accepted rather than worked around: the count cannot be reconstructed downstream, because by the time the graph exists the losing candidates are gone. A bump costs a one-time silent re-index, which the loader already handles.

`Confidence` is `#[non_exhaustive]` specifically to allow future resolution variants. Consider whether the count belongs *in* the enum's `Heuristic` variant or as a sibling field — a count of 1 alongside `Resolved` is meaningful and uniform, which argues for a sibling field.

**Decisions made at implementation.** (1) Sibling field, as the note argues: `candidates: u32` on `Edge`, `EdgeEntry`, and `PackedEdge`. (2) Declarative edges are `1` by construction — parse-time edges carry a provisional `1` and only the resolve pass overwrites call/include edges; the Rust `mod`-decl override returns `Resolved`/1 unconditionally. (3) A suffix-disambiguated include reports the REAL N with `Confidence::Resolved` — the count says how contested the name was, the confidence says whether the pick was structural; the two axes are deliberately independent. (4) The resolver signatures (`resolve_call`/`resolve_include`, trait + defaults + overrides) widened to carry the count, and BOTH resolve paths (analyze-path indexer loop, watch-path inline resolver) stamp it. (5) serde defaults (1) exist for hand-written fixtures only; cache safety is the v11 bump — the trap's default-instead-of-bump failure is explicitly rejected in the field docs.

### Completion Evidence

- Verified: 2026-08-19
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `a8d1e2afae7d3b33194009d366e72de75aed0a3a`
- Identity recheck: `git rev-parse HEAD` at 2026-08-19 16:36 matched `a8d1e2afae7d3b33194009d366e72de75aed0a3a`
- Focused review: `git show a8d1e2afae7d3b33194009d366e72de75aed0a3a`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `a8d1e2afae7d3b33194009d366e72de75aed0a3a`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-graph persist && cargo test -p code-graph-lang` | `.` | PASS (`exit 0`) | `29 persist tests passed including the new round_trip_preserves_candidate_count (a NON-default count of 3 survives save/load in both adjacency directions — a default-riding pass could not distinguish persisted from reconstructed) and the pre-existing load_version_mismatch_returns_false covering the pre-v11 silent re-index. 65 lang tests passed with the resolver suites now asserting count 1 for sole-candidate picks, 2 for contested picks, and 2 for suffix-disambiguated includes (Resolved + real N).` |
| `make verify` | `.` | PASS (`exit 0`) | `clippy -D warnings, fmt, full workspace tests, snapshots, and plugin mirrors all green after the trait-signature change rippled through both resolve paths and all six plugins.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `focused diff review` | `git show a8d1e2a` | PASS | `20 files: the count is captured at the resolver's pick site (the only moment it exists — the comment names FR-48/D-0007); CACHE_VERSION 10->11 with a history entry explaining why defaulting is rejected (the phase trap); the two merge sites copy the count into both adjacency directions; every construction site carries an explicit count (1 for parse-time/declarative, real N in resolver tests); no wire surface changed — 9.2 owns that.` |

### Trap
Defaulting the count to 0 or 1 for edges written before the bump, to avoid the version change. That silently makes "unambiguous" indistinguishable from "unknown" for every pre-existing cache, which is exactly the failure the count exists to prevent. Bump the version and re-index.

## 9.2: Surface candidate count on the edge-reporting tools

### Subtasks
- [x] Expose the count on `get_callers` and `get_callees` hops
- [x] Expose it on `find_path` hops
- [x] Expose it on `generate_diagram` edges
- [x] Rebaseline the affected response snapshots deliberately, confirming each change is additive
- [x] Update CLAUDE.md's Response shapes section

### Notes
Revision boundary: every tool that reports a resolved edge reports how contested it was.

Additive fields only — a client reading today's fields must keep parsing. This is the phase where the snapshots legitimately move, so review each diff rather than accepting in bulk.

**Shape decisions made at implementation.** `CallChain.candidates` is a plain `u32` (every hop was reached by a real traversed edge). `PathHop.candidates` is `Option<u32>` mirroring `entered_by`'s convention EXACTLY — `null` only for `hops[0]`, which no edge reached. `DiagramEdge.candidates` is `Option<u32>` with `skip_serializing_if`: present on `symbol=` call edges, ABSENT (not `null`) on `file=`/`class=` edges, which come from the include map and the name-keyed hierarchy walk and carry no resolver metadata — deliberately the same boundary `min_confidence` already draws. `find_overrides` inherits the field through `CallChain` (declarative edges report 1). All eight snapshot diffs were reviewed individually: purely additive.

### Completion Evidence

- Verified: 2026-08-20
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `a7631c5f5d46b07c6965129bce0770c1f4c0bb5c`
- Identity recheck: `git rev-parse HEAD` at 2026-08-20 10:08 matched `a7631c5f5d46b07c6965129bce0770c1f4c0bb5c`
- Focused review: `git show a7631c5f5d46b07c6965129bce0770c1f4c0bb5c`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `a7631c5f5d46b07c6965129bce0770c1f4c0bb5c`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools --test candidate_count && cargo test -p code-graph-tools --test snapshot_responses` | `.` | PASS (`exit 0`) | `4 new tests pin AC-57 end to end on one fixture (two same-named definitions vs one unique helper): get_callees distinguishes 1 from 2 in a single response; get_callers carries the contested count through reverse adjacency; find_path hops mirror entered_by's null-for-source convention; generate_diagram symbol-mode edges report the real N. All 59 response snapshots green after the 8 deliberate rebaselines.` |
| `make verify` | `.` | PASS (`exit 0`) | `clippy -D warnings, fmt, full workspace tests, snapshots, and plugin mirrors all green.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `snapshot rebaseline review` | `8 .snap diffs, each read individually` | PASS | `Every diff is purely additive: candidates: 1 on the fixture's unambiguous hops/edges, null on find_path's source hop (mirroring entered_by), no field removed or reordered — a client reading only pre-phase fields still parses.` |
| `focused diff review` | `git show a7631c5` | PASS | `12 files: the three wire types gain the field with doc comments naming FR-48/D-0007 and the None conventions; the Dijkstra bookkeeping record carries the count alongside confidence so both stamp from the same traversed edge; the file=/class= diagram arms document WHY the field is absent there; no query logic changed — the diff is field plumbing, snapshots, tests, and docs.` |

## 9.3: Retire or demote the binary confidence tag on the wire

### Subtasks
- [x] Inventory every wire type carrying a binary confidence tag
- [x] For each, remove it or record why both signals are warranted
- [x] Keep `min_confidence` as a *filter* — it is an input, and unaffected
- [x] Update CLAUDE.md and every affected tool description

### Notes
Revision boundary: one signal per concept on the wire.

`min_confidence` stays. It is a request-side filter with a documented spelling and pruning semantics; nothing here changes it. What is under review is the response-side tag that merely restates a count the response now carries.

**Inventory (complete).** Exactly two wire surfaces carry a binary confidence tag: `PathHop.entered_by` (`find_path` hops) and the derived aggregate `FindPathResponse.heuristic_hops`. `CallChain` and `DiagramEdge` never carried one — the count is their FIRST resolver signal, so there is nothing to retire there. `min_confidence` is request-side and unaffected.

**Disposition: demote and document, not remove.** Both fields stay, with the rationale recorded on `PathHop`'s doc (the authoritative statement) and in CLAUDE.md's renamed "Edge confidence and candidate count" section: (a) removing a shipped field breaks the phase's own additive-response AC; (b) the axes are independent by design — suffix-disambiguated includes already emit `Resolved` with count 2, and `Confidence` is `#[non_exhaustive]` precisely so a future type-inference variant can mark a multi-candidate pick as definitively resolved, at which point deriving the tag from the count would be wrong; (c) `heuristic_hops` is the fewest-heuristic-edges tie-break cost — it explains WHY `find_path` chose this path, which per-hop counts cannot replace without re-deriving the tie-break client-side. The DEMOTION is in the agent-facing prose: all four tool descriptions now present `candidates` as the signal to reason from ("N ≥ 2 means the scope rule picked one of N" + the exact `min_confidence` relationship), with the one-bit tag positioned as the filter's mechanism rather than a signal to read. This is the outcome D-0007 prescribes: the indicator that names the next action leads; the derived bit is documented as derived.

### Completion Evidence

- Verified: 2026-08-20
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `7e581cc5655da7c10d82f5de2d71e96f060ed9d1`
- Identity recheck: `git rev-parse HEAD` at 2026-08-20 10:46 matched `7e581cc5655da7c10d82f5de2d71e96f060ed9d1`
- Focused review: `git show 7e581cc5655da7c10d82f5de2d71e96f060ed9d1`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `7e581cc5655da7c10d82f5de2d71e96f060ed9d1`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools --test snapshot_tools_list && cargo test -p code-graph-tools` | `.` | PASS (`exit 0`) | `All 33 tools-list snapshots green after the four deliberate description rebaselines (get_callers, get_callees, find_path, generate_diagram); the full code-graph-tools suite passes with the resulting shapes — no wire type changed in this task, only doc comments, descriptions, and the two snapshot surfaces they feed.` |
| `make verify` | `.` | PASS (`exit 0`) | `clippy -D warnings, fmt, full workspace tests, snapshots, and plugin mirrors all green.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `wire-type inventory` | `rg Confidence across graph/tools wire structs` | PASS | `Exactly two response-side surfaces carry the one-bit tag: PathHop.entered_by and FindPathResponse.heuristic_hops. CallChain and DiagramEdge never carried one. min_confidence is request-side and unaffected. The keep-both rationale is recorded on PathHop's doc and in CLAUDE.md's 'Edge confidence and candidate count' section; the demotion is in the four tool descriptions, which now lead with candidates and state the exact min_confidence relationship (NFR-11).` |
| `focused diff review` | `git show 7e581cc` | PASS | `7 files, no behavior change: PathHop doc carries the authoritative three-part disposition (additive contract, independent axes with the suffix-disambiguation precedent and the #[non_exhaustive] future, heuristic_hops as tie-break cost); CLAUDE.md's section renamed and extended with the count bullet and the min_confidence-to-count relationship; each description edit presents the count as the signal to reason from.` |

## Acceptance Criteria

- [ ] **AC-57**: For an edge whose target was selected from N same-named candidates, the reporting tools expose N; a caller distinguishes an unambiguous edge from a contested one without another query (FR-48).
- [ ] The count survives a cache round-trip, and a pre-bump cache is silently re-indexed rather than misread.
- [ ] Response-shape changes are additive; a client reading only pre-phase fields still parses.
- [ ] CLAUDE.md and the affected tool descriptions state what the count means and how it relates to `min_confidence` (NFR-11).
- [ ] **AC-27**: `make verify` passes (NFR-04).

## Phase Completion Evidence

Pending — not complete.
