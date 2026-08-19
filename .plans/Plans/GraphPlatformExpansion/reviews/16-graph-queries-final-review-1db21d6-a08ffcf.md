---
title: "Phase review: Graph Queries (certification gate)"
type: review
status: resolved
created: 2026-08-18
updated: 2026-08-18
tags: [review, graph-queries, phase-1, final, certification]
related: ["Plans/GraphPlatformExpansion/01-Graph-Queries.md"]
review_of: "Plans/GraphPlatformExpansion/01-Graph-Queries.md"
rev: "1db21d6a2e676ddec7f74b265179f5fb95eb25db..a08ffcf0d2addc8f6f2a49ea05b1a9d8bde890a4"
review_scope: phase
frozen: true
verdict: Aligned
reviewed_planning_revision: "e234a8c9be5c91bfe225293929c45f2ef1f63e16"
review_mode: independent
lane_results:
  - lane: review_plan_drift
    result: PASS/Aligned
    reviewed_identity: "1db21d6a2e676ddec7f74b265179f5fb95eb25db..a08ffcf0d2addc8f6f2a49ea05b1a9d8bde890a4"
    evidence: "All seven tasks carry complete current-template evidence; all nine checkpoints (7 task + 2 review-fix) are non-merge, in-range, and scope-matched on --stat; all nine review findings map finding-to-fix inside the range; every ticked AC traces to a real artifact; the pre-approval plugin commits are verified ancestors of the plan-approval commit and thus out-of-phase interleave."
  - lane: review_quality
    result: PASS/Aligned
    reviewed_identity: "1db21d6a2e676ddec7f74b265179f5fb95eb25db..a08ffcf0d2addc8f6f2a49ea05b1a9d8bde890a4"
    evidence: "Dijkstra cost packing is overflow-safe by construction (widen-before-shift under the 5M cap ceiling); cap semantics, tie-breaking, and both determinism claims are structural properties pinned by dedicated tests; handlers conform to the Page/byte-budget discipline; the only endpoint defect is a test-literal path-separator assumption, immaterial then and fixed since."
  - lane: review_spec_compliance
    result: PASS/Aligned
    reviewed_identity: "1db21d6a2e676ddec7f74b265179f5fb95eb25db..a08ffcf0d2addc8f6f2a49ea05b1a9d8bde890a4"
    evidence: "FR-21/22/23/24/25/44/45/46 and their design decisions implemented faithfully; pre-existing wire shapes untouched (additive-only structs, zero modified snapshots, no dependency or cache-version movement); one partial — AC-32's literal member-cap echo is a spec-vs-design inconsistency implemented faithfully to the design, deserving spec-side reconciliation, not phase rejection."
  - lane: review_blind_spots
    result: PASS/Aligned
    reviewed_identity: "1db21d6a2e676ddec7f74b265179f5fb95eb25db..a08ffcf0d2addc8f6f2a49ea05b1a9d8bde890a4"
    evidence: "Every adversarial axis probed came back clean (overflow, determinism vs HashMap order, trichotomies, degeneracy boundaries, empty graph); two bounded-impact defects survive — find_path bypasses the response byte budget, and cap_reached can false-positive at exact frontier exhaustion — ticketed, not blocking."
findings: []
followups:
  - "find_path bypasses [response].max_bytes entirely: a deep call chain yields one hop per node with no byte-budget consultation — the only phase-1 tool without it, and the exemption is undocumented (unlike detect_cycles'). Either budget it or document the exemption in the tool description and CLAUDE.md."
  - "cap_reached: true false positive when the reachable set is exactly node_cap nodes and the target is unreachable — the search completed but reports 'gave up', instructing one pointless retry. Check heap emptiness before declaring the cap reached."
  - "AC-32 spec reconciliation: the resolved members_per_community value is not echoed in DetectCommunitiesResponse; the code matches the approved design's response shape, so the spec's 'both caps are echoed' wording needs reconciliation or an additive field."
  - "community.rs weight tallies use unchecked u32 += (practically unreachable at 2^32 folded edges); a407b41 saturated other sites — align these for consistency."
  - "shortest_path equal-cost parent selection is deterministic per graph state but can flip across a re-index that merges files in a different order; the doc's 'across runs and process restarts' claim holds only modulo identical graph construction order — tighten the wording."
  - "Task 1.4's ticked measurement subtask is evidenced at 8dac325 (review-fix commit), not at 1.4's own checkpoint 2986df0 — traceable via reviews/01 F-02 but worth knowing when reading the evidence block."
---

# Phase review: Graph Queries (certification gate)

Reviewed `Plans/GraphPlatformExpansion/01-Graph-Queries.md` at frozen
identity `1db21d6..a08ffcf` (planning content at `e234a8c`). **Review
mode:** independent lanes — four parallel, non-inheriting contexts with
isolated inputs; code lanes worked from a detached endpoint worktree.

## Why this gate exists

Phase 1 was code-complete and fully evidenced but uncertified: review
findings F-08/F-09 were fixed (`5a5f9f6`) *after* the phase's last review
cycle, and a material change supersedes a review. This gate reviews the
phase's **full range**, which contains every fix, closing that
supersession loop. Prior coverage corroborates but did not substitute:
artifacts 01 (review at `2986df0`), 12 (adversarial phases 1–3 at
`9989243`), and 14 (full-branch) each passed over this code without being
the per-phase certification artifact.

## Verification

- Verification rides the recorded per-task evidence (each task's
  `make verify` row at its checkpoint) plus the current-tree state: this
  code, as since evolved, is green under `make verify` at `5697222`
  (1,856 tests, clippy `-D warnings`, snapshots, plugin mirrors), with
  docs-only commits from there to the reviewed planning revision.
- Endpoint test runs were deliberately not re-executed on this host: the
  endpoint predates the later Windows test-portability fixes, and its
  recorded verification was performed on the platform of record.

## Resolution Log

No open findings; the follow-ups above are accepted, recorded
non-blockers.
