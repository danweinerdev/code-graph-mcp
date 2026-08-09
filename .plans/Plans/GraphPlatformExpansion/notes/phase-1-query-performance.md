---
title: "Phase 1 Query Performance Measurement"
type: debrief
status: complete
plan: GraphPlatformExpansion
phase: 1
phase_title: "Graph Queries"
created: 2026-08-08
updated: 2026-08-08
tags: [performance, ac-43, nfr-08]
related:
  - Plans/GraphPlatformExpansion
  - Specs/GraphPlatformExpansion
---

# Phase 1 Query Performance Measurement

Records AC-43 / NFR-08. AC-43 is deliberately a **recorded-metric** criterion, not an automated gate: a wall-clock assertion would be flaky across machines and CI load, so the pass condition is a human reading these numbers. What *is* automated is cap enforcement, covered by unit tests in tasks 1.2 and 1.3.

Reproduce with:

```
cargo test -p code-graph-tools --release --test query_perf -- --ignored --nocapture
```

The test auto-skips with a hint if the corpus is uninitialised, per the dogfood-baseline convention.

## Corpus

`external/abseil-cpp/absl` — 841 files, 9,879 symbols, 91,874 edges. Release build, single machine, single run.

## Results

| Query | Elapsed | Detail |
|---|---|---|
| `file_communities` (max_iterations 50) | **177.3 ms** | 841 file nodes, 5,474 file-level edges, 87 communities, converged after 5 iterations |
| `shortest_path` (node_cap 100,000) | **0.4 ms** | 307 nodes examined, `cap_reached: false`, `found: false` |

## Reading these numbers

**`file_communities` is the meaningful one and it is comfortably interactive.** 177 ms over 91,874 raw edges, converging in 5 sweeps, confirms the near-linear expectation in FR-45 and validates the file-granularity decision: the aggregation collapses 9,879 symbols to 841 nodes and 91,874 edges to 5,474 weighted pairs before propagation starts. Symbol granularity would have run propagation over an order of magnitude more nodes.

**The `shortest_path` number is weak evidence and should not be quoted as a worst case.** The test picks an unconnected pair so the search must exhaust rather than short-circuit, but it exhausted after only 307 nodes — the chosen source's reachable component is small. A genuine worst case starts from a high-fan-out symbol in a densely connected component and would examine orders of magnitude more nodes. What this run does establish is that the not-found path terminates correctly and reports `cap_reached: false` when the component is exhausted below the cap, which is the AC-16 behaviour.

**Neither corpus nor run count is what the design imagined.** `abseil-cpp/absl` at 841 files is the largest initialised C++ corpus here, but it is far from the UE4/LLVM scale CLAUDE.md discusses elsewhere. These numbers say "not slow at this size", not "scales".

## Decisions Made

- **AC-43 is recorded, not gated.** No wall-clock assertion was added; a timing threshold would be flaky across machines and CI load. Cap enforcement is separately automated in tasks 1.2 and 1.3.
- **abseil-cpp/absl was used** as the largest initialised C++ corpus available. It is not large in the sense CLAUDE.md means when it discusses UE4/LLVM scale, and the note says so rather than implying coverage it does not have.
- **The `shortest_path` figure is published with its limitation attached** rather than withheld or quoted plainly. It exhausted after 307 nodes, so it demonstrates correct not-found termination, not scaling.

## Follow-Ups

### Worth considering

A better `shortest_path` worst case — source selected by maximum transitive fan-out rather than by orphan status — would make this measurement meaningful rather than merely present. Not filed as a task: AC-43 asks for a recorded metric and this records one, with its limits stated. Worth revisiting if the query is ever reported as slow.
