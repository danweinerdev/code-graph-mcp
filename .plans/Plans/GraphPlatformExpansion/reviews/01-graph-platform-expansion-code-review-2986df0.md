---
title: "Code Review: GraphPlatformExpansion Phase 1 — Graph Queries"
type: review
status: resolved
created: 2026-08-08
updated: 2026-08-08
tags: [review, phase-1, track-c]
related:
  - Plans/GraphPlatformExpansion
  - Specs/GraphPlatformExpansion
  - Designs/GraphQueries
review_of: "Plans/GraphPlatformExpansion"
rev: "2986df0"
findings:
  - id: F-01
    severity: major
    title: "find_path dispatches inline while its sibling tools use spawn_blocking, holding the graph read lock across an unbounded Dijkstra"
    status: fixed
  - id: F-02
    severity: major
    title: "AC-43 performance measurement never run or recorded"
    status: fixed
  - id: F-03
    severity: major
    title: "AC-18's snapshot half missing — no snapshot_responses.rs coverage for the three new tools"
    status: fixed
  - id: F-04
    severity: major
    title: "Phase doc marks tasks 1.2-1.4 planned though the code is merged and tested"
    status: fixed
  - id: F-05
    severity: minor
    title: "Shipped PathResult has two fields where the design documents four"
    status: fixed
  - id: F-06
    severity: minor
    title: "Saturating arithmetic not applied at the two sites the design names"
    status: fixed
  - id: F-07
    severity: minor
    title: "PathHop per-hop confidence on the wire sits in tension with FR-23's internal-weighting wording"
    status: answered
  - id: F-08
    severity: major
    title: "detect_communities holds the graph read lock across whole-graph label propagation with no documented tradeoff and no async escape"
    status: fixed
  - id: F-09
    severity: minor
    title: "get_symbol_at reports an indexed file with zero symbols as file-not-found"
    status: fixed
followups:
  - id: FU-01
    finding: F-08
    summary: "Decide whether detect_communities needs an async job plus polling shape like analyze_codebase_async, or a work budget independent of max_iterations, for UE4/LLVM-scale graphs"
    tracked_in: ""
---

# Code Review: GraphPlatformExpansion Phase 1 — Graph Queries

**Reviewed state:** 2986df0 (range 63c8d9ba82bd00a4d58aa39d8cacd9159fd70647..2986df0e192517e51fa4f9f59717c58f9a4dd9c6, clean worktree)
**Review mode:** independent — four fresh-context lanes, no project lanes configured

**Alignment:** Moderate. No critical issues. Two findings were reached independently by two lanes each; no lane contradicted another.

## Findings

### F-01 — Major: find_path dispatches inline, holding the graph read lock across an unbounded Dijkstra
**Impugns:** `crates/code-graph-tools/src/server.rs:1308`, NFR-08
**Caught by:** review_quality, review_blind_spots
**Scenario:** An agent calls `find_path` on a large graph with an unreachable target and a high `node_cap` (up to 5,000,000, or the 100,000 default on a 770k-symbol corpus). The search must exhaust the reachable component before returning. There is no `.await` anywhere in the chain and a `parking_lot` read guard is held throughout, so on the multi-thread runtime one worker is occupied end to end while the graph lock is held — stalling a concurrent watch-driven reindex that needs the write lock. The two sibling tools added in the same commit both use `spawn_blocking`.
**Why it matters:** All 15 new tests call the handler directly and never the `#[tool]` wrapper, so no existing test would ever surface this.
**Recommendation:** Wrap the dispatch in `tokio::task::spawn_blocking`, matching the sibling precedent.

### F-02 — Major: AC-43 performance measurement never run or recorded
**Impugns:** AC-43, NFR-08, task 1.4
**Caught by:** review_plan_drift, review_spec_compliance
**Scenario:** `notes/` contains only `.gitkeep`. No timing was recorded for `shortest_path` worst-case or `file_communities` against any corpus.
**Why it matters:** AC-43 was deliberately written as a recorded-metric criterion rather than an automated gate, on the grounds that a human reading the number is the check. With no number, nothing checks it.
**Recommendation:** Measure both against the largest initialised dogfood corpus and record in `notes/`.

### F-03 — Major: AC-18's snapshot half is missing
**Impugns:** AC-18, NFR-03, `crates/code-graph-tools/tests/snapshot_responses.rs`
**Caught by:** review_spec_compliance
**Scenario:** The design states AC-18 needs both the 20-run determinism unit test and a committed `insta` snapshot of the handler response, and that "neither alone closes AC-18". The unit test landed; `snapshot_responses.rs` was never touched, and none of the three new tools appear there.
**Why it matters:** The unit test proves cross-run stability but pins no golden output, so a change in field order, label derivation, or ranking would pass silently.
**Recommendation:** Add response snapshots for `detect_communities`, `get_symbol_at`, and `find_path`, covering the member-capped case and the granularity/termination fields.

### F-04 — Major: phase doc is behind the tree
**Impugns:** task 1.2, task 1.3, task 1.4, `.plans/Plans/GraphPlatformExpansion/01-Graph-Queries.md`
**Caught by:** review_plan_drift
**Scenario:** Only task 1.1 was marked complete. Tasks 1.2 through 1.4 still read `status: planned` with unchecked subtasks and pending evidence, though their code is merged, tested, and snapshot-covered.
**Why it matters:** A reader of the plan alone would conclude three of four tasks had not started.
**Recommendation:** Record completion evidence for 1.2 through 1.4.

### F-05 — Minor: shipped PathResult diverges from the documented interface
**Impugns:** `Designs/GraphQueries` Interfaces, `crates/code-graph-graph/src/callgraph.rs`
**Caught by:** review_spec_compliance
**Scenario:** The design lists `PathResult { hops, heuristic_hops, nodes_examined, cap_reached }`; the shipped struct has the first two, with the other values carried on the returned tuple.
**Why it matters:** The change was deliberate — holding those values in two places let the copies disagree — but the design text was not updated, so design and code now describe different interfaces.
**Recommendation:** Reconcile the design to the shipped shape and state why.

### F-06 — Minor: saturating arithmetic not applied where the design names it
**Impugns:** `Designs/GraphQueries` Structural Verification, `crates/code-graph-graph/src/{callgraph,community}.rs`
**Caught by:** review_spec_compliance
**Scenario:** The design calls for saturating arithmetic at the packed Dijkstra cost and the permille share. Both use plain arithmetic. Values are bounded well below overflow by the `node_cap` ceiling, so no bug is observed.
**Why it matters:** The design's stated mitigation for "a pathological graph degrades rather than panicking" is not in force.
**Recommendation:** Apply `saturating_*` at both sites.

### F-07 — Minor: per-hop confidence on the wire vs FR-23's internal-weighting wording
**Impugns:** FR-23, `PathHop`
**Caught by:** review_spec_compliance
**Scenario:** FR-23 says the weighting "shall be internal; it shall not add a numeric confidence field to any wire type." `PathHop.entered_by` is non-numeric so it satisfies the letter, but it does expose per-hop confidence.
**Why it matters:** Spec intent and shipped shape could be read as at odds.
**Recommendation:** Resolved — see the Resolution Log.

## Resolution Log

### F-07 — answered (2026-08-08)
Kept, and the requirement clarified rather than the field removed. These responses are consumed by agents, and per-hop detail lets a caller identify the weak link without a second query — a field earns its place by removing a round-trip (D-0007). FR-23 was amended to distinguish the *weighting* (internal: no numeric value, score, or cost on the wire) from *which hops were heuristically resolved* (permitted, and useful).

The review also surfaced that a binary resolved/heuristic tag is a one-bit projection of the signal a caller can actually act on — how many candidates competed. That is now FR-48 / AC-57, with phase 9 (`09-Resolver-Candidate-Count.md`) carrying the work, since it needs a resolver change and a cache-format bump and cannot be reconstructed downstream. Governing facts: D-0007, FR-23, FR-48, AC-57.

### F-01 — fixed (2026-08-08)
Wrapped `find_path`'s `#[tool]` dispatch in `tokio::task::spawn_blocking`, matching the `get_coupling` and `detect_communities` precedent. Landed as task 1.5, commit 8d3e7ca. No behaviour or response-byte change.

### F-02 — fixed (2026-08-08)
Added an `#[ignore]`-gated `query_perf` test — the existing bench harness measures indexing only, which is why AC-43 was never runnable — and recorded the numbers in `notes/phase-1-query-performance.md`: `file_communities` 177.3 ms and `shortest_path` 0.4 ms against `external/abseil-cpp/absl` (841 files, 9,879 symbols, 91,874 edges). The path figure is recorded with an explicit caveat that it is not a worst case: the search exhausted after 307 nodes. Task 1.4's subtask plus commit 8dac325.

### F-03 — fixed (2026-08-08)
Added six response snapshots covering all three tools, including a member-capped community pinning `truncated: true` with `original_len: 5` (AC-32) and the granularity/termination fields (AC-53, AC-54). Landed as task 1.6, commit 28ac556. Only new `.snap` files; no existing snapshot moved.

### F-04 — fixed (2026-08-08)
Recorded completion evidence and flipped status for tasks 1.2 through 1.7; the phase doc now matches the tree.

### F-05 — fixed (2026-08-08)
Reconciled the design's four-field `PathResult` sketch with the shipped two-field shape, stating why the change was made. Commit 0820c59.

### F-06 — fixed (2026-08-08)
Applied `saturating_add` to the hop and heuristic accumulation and `saturating_mul` to the permille share, per the design's Structural Verification section. The `u64` widen-before-shift in the cost packing is deliberately untouched. Landed as task 1.7, commit a407b41.

### F-08 — fixed (2026-08-08)
Documented rather than re-architected. `detect_communities` now carries the same acknowledgement `find_path` has: label propagation holds the read guard over the whole aggregated file graph so a concurrent watch reindex waits, `spawn_blocking` protects the tokio scheduler but not the client's wall-clock `MCP_TOOL_TIMEOUT`, and 177 ms on 841 files is the only measured point. Commit 5a5f9f6.

Whether the tool needs the async-job-plus-polling shape `analyze_codebase_async` uses, or a work budget independent of `max_iterations`, is a scope decision tracked as FU-01 rather than settled here. The concern is real at UE4/LLVM scale, which no measurement covers.

### F-09 — fixed (2026-08-08)
Added `Graph::has_file` backed by the trie's `contains_path` and used it for the file-not-found branch. `merge_file_graph` inserts a `FileEntry` even for a file that parses to zero symbols, so the old emptiness check reported a header of forward declarations as unindexed — while the doc comment claimed it distinguished the two. Regression test merges a `FileGraph` with an empty symbols vec. `get_file_symbols` deliberately untouched: same conflation, but pre-existing and documented as intentional. Commit 5a5f9f6.
