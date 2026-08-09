---
title: "Phase 1 Debrief: Graph Queries"
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

# Phase 1 Debrief: Graph Queries

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

### Worth considering later

A better `shortest_path` worst case — source selected by maximum transitive fan-out rather than by orphan status — would make this measurement meaningful rather than merely present. Not filed as a task: AC-43 asks for a recorded metric and this records one, with its limits stated. Worth revisiting if the query is ever reported as slow.

## Requirements Assessment

Every in-scope requirement is implemented and evidenced: FR-21 through FR-26, FR-44, FR-45, FR-46, NFR-03, NFR-08, NFR-11, and acceptance criteria AC-13 through AC-18, AC-32, AC-43, AC-45, AC-53 through AC-55. AC-33 is deliberately half-open — its CLI arm cannot close until phase 7, and the phase criteria never claimed it.

Two requirements were added *during* the phase rather than satisfied by it. FR-48/AC-57 (candidate count) and FR-49/AC-58 (async whole-graph queries) both came out of review and are carried by phases 9 and 4.

## Deviations

- **`PathResult` shipped with two fields, not the four the design sketched.** `nodes_examined` and `cap_reached` live on the returned tuple, where they exist on both the found and not-found paths. Holding them in both places let two copies disagree. The design was reconciled to the shipped shape.
- **AC-43 could not be satisfied as written.** It named `code-graph-bench`, which measures indexing and has no notion of a query. A dedicated `#[ignore]`-gated `query_perf` test was added instead. The criterion's intent was met; its stated mechanism was wrong.
- **Three tasks were added mid-phase** (1.5, 1.6, 1.7) to carry review fixes, per the rule that a review-driven code fix gets its own task revision.
- **The phase is not formally complete.** All tasks are done and all nine review findings are fixed, but certification needs an all-lanes-Aligned review and the third cycle was skipped by decision.

## Risks & Issues Encountered

- **The test suite could not have caught the worst bug in the phase.** F-01 — `find_path` holding a `parking_lot` read guard across an unbounded Dijkstra with no await point — was invisible to all 15 of its tests, because every one calls the handler directly and never the `#[tool]` wrapper. Green meant nothing there.
- **Two silent-correctness contracts in community detection** would have degraded results without failing anything: folding self-pairs into the weight table biases label propagation toward stasis, and omitting the `EdgeKind::Calls` filter lets inheritance edges become community weight. Both are now pinned by dedicated regression tests.
- **`EdgeKind` has four variants, not the three CLAUDE.md claimed.** Found while writing the aggregator's filter. Two further stale claims surfaced in the same sweep — the cache is v10 not v8, and the workspace is not C-compiler-free.

## Lessons Learned

- **Ask what the consumer does with a field.** The most valuable output of this phase was not code: reframing "does `entered_by` violate FR-23" into "what actually helps an agent" produced D-0007 and FR-48, and changed the API for the better. A binary confidence tag is a one-bit projection of the number a caller can act on.
- **An acceptance criterion naming a tool should be checked against that tool.** AC-43 named a harness that could not measure what the criterion asked for, and nobody noticed until a review lane went looking.
- **Amending commits breaks paper trails.** The F-05 resolution cited a SHA that no longer existed after two amendments. The drift lane caught it; nothing else would have.

## Impact on Subsequent Phases

- **Phase 2** must migrate the three new handlers alongside the original nineteen — that is what puts them in the typed core where the CLI can reach them, and why phase 2 now depends on phase 1.
- **Phase 4** absorbs FU-01 as task 4.4, generalizing the job slot it is already reshaping.
- **Phase 9** is new, carrying candidate count.
- **The four-lane review is worth its cost but it compounds.** Every material fix supersedes the review, so certification is a fixed-point iteration. Later phases should batch fixes before re-reviewing rather than fixing findings one at a time.

## Skill Opportunities

- A convention that tests exercise the `#[tool]` wrapper, not only the handler, would have caught F-01 at write time.
- The stale-CLAUDE.md pattern recurred three times in one phase; a periodic doc-vs-code audit would find these before a reviewer does.
