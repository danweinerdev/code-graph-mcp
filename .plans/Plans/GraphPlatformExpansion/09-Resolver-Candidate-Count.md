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
    status: in-progress
    justifies: "FR-48, AC-57, D-0007. Confidence::Heuristic is a one-bit projection of 'N candidates competed'. The resolver knows N at the moment it picks, and then throws it away — so the information a caller needs to disambiguate is destroyed at index time and cannot be recovered by any downstream change."
    verification: "cargo test -p code-graph-graph persist:: and cargo test -p code-graph-lang resolve:: — an edge resolved from a single candidate records 1; an edge resolved from N same-named candidates records N; the value survives a cache save/load round-trip; CACHE_VERSION is bumped and an older cache is silently re-indexed rather than misread (existing version-mismatch path); make verify passes."
  - id: "9.2"
    title: "Surface candidate count on the edge-reporting tools"
    status: planned
    justifies: "FR-48, AC-57. Storing the count without exposing it satisfies nothing — AC-57 requires a caller to distinguish 'one candidate, unambiguous' from 'five candidates, one picked by scope rule' without another query."
    verification: "cargo test -p code-graph-tools — get_callers, get_callees, find_path, and generate_diagram each expose the count on the edges they report; a caller can tell a 1-candidate edge from an N-candidate one in a single response (AC-57); existing response snapshots are rebaselined deliberately and the change is additive, so a client reading only today's fields still parses."
    depends_on: ["9.1"]
  - id: "9.3"
    title: "Retire or demote the binary confidence tag on the wire"
    status: planned
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
- [ ] Capture the candidate count in the resolver at the point the target is chosen
- [ ] Add the count to the edge record and to its packed representation
- [ ] Bump `CACHE_VERSION`; confirm the existing version-mismatch path silently re-indexes rather than misreading
- [ ] Round-trip tests through save/load
- [ ] Decide and document what the count is for a declarative edge (Inherits, Overrides, `mod`-resolved Includes) — these have exactly one candidate by construction

### Notes
Revision boundary: edges carry the count and it survives the cache; nothing exposes it yet.

This is the only task in the plan that changes the cache format. That is accepted rather than worked around: the count cannot be reconstructed downstream, because by the time the graph exists the losing candidates are gone. A bump costs a one-time silent re-index, which the loader already handles.

`Confidence` is `#[non_exhaustive]` specifically to allow future resolution variants. Consider whether the count belongs *in* the enum's `Heuristic` variant or as a sibling field — a count of 1 alongside `Resolved` is meaningful and uniform, which argues for a sibling field.

### Completion Evidence

Pending — not complete.

### Trap
Defaulting the count to 0 or 1 for edges written before the bump, to avoid the version change. That silently makes "unambiguous" indistinguishable from "unknown" for every pre-existing cache, which is exactly the failure the count exists to prevent. Bump the version and re-index.

## 9.2: Surface candidate count on the edge-reporting tools

### Subtasks
- [ ] Expose the count on `get_callers` and `get_callees` hops
- [ ] Expose it on `find_path` hops
- [ ] Expose it on `generate_diagram` edges
- [ ] Rebaseline the affected response snapshots deliberately, confirming each change is additive
- [ ] Update CLAUDE.md's Response shapes section

### Notes
Revision boundary: every tool that reports a resolved edge reports how contested it was.

Additive fields only — a client reading today's fields must keep parsing. This is the phase where the snapshots legitimately move, so review each diff rather than accepting in bulk.

### Completion Evidence

Pending — not complete.

## 9.3: Retire or demote the binary confidence tag on the wire

### Subtasks
- [ ] Inventory every wire type carrying a binary confidence tag
- [ ] For each, remove it or record why both signals are warranted
- [ ] Keep `min_confidence` as a *filter* — it is an input, and unaffected
- [ ] Update CLAUDE.md and every affected tool description

### Notes
Revision boundary: one signal per concept on the wire.

`min_confidence` stays. It is a request-side filter with a documented spelling and pruning semantics; nothing here changes it. What is under review is the response-side tag that merely restates a count the response now carries.

### Completion Evidence

Pending — not complete.

## Acceptance Criteria

- [ ] **AC-57**: For an edge whose target was selected from N same-named candidates, the reporting tools expose N; a caller distinguishes an unambiguous edge from a contested one without another query (FR-48).
- [ ] The count survives a cache round-trip, and a pre-bump cache is silently re-indexed rather than misread.
- [ ] Response-shape changes are additive; a client reading only pre-phase fields still parses.
- [ ] CLAUDE.md and the affected tool descriptions state what the count means and how it relates to `min_confidence` (NFR-11).
- [ ] **AC-27**: `make verify` passes (NFR-04).

## Phase Completion Evidence

Pending — not complete.
