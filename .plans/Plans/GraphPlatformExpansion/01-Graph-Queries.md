---
title: "Graph Queries"
type: phase
plan: GraphPlatformExpansion
phase: 1
status: in-progress
created: 2026-08-08
updated: 2026-08-08
deliverable: "Three new MCP tools — get_symbol_at, find_path, detect_communities — answering position, reachability, and module-structure questions the current surface cannot answer."
tasks:
  - id: "1.1"
    title: "Position lookup: symbols_at_line and get_symbol_at"
    status: complete
    justifies: "FR-21, AC-13, AC-14, AC-15. Every entry point into the graph is name-addressed, but an agent reading a diff hunk, a stack trace, or a compiler error holds file:line — without this the graph is unreachable from the information the agent actually has."
    verification: "cargo test -p code-graph-graph symbols_at_line and cargo test -p code-graph-tools get_symbol_at — a line inside a method nested in a class returns the method before the class (AC-13); a line in no span returns an empty Page, not an error and not a nearest guess (AC-14); an existing pre-change cache file loads unchanged, proving no CACHE_VERSION bump (AC-15); two symbols sharing a span return in a stable order across 20 repeated calls."
  - id: "1.2"
    title: "Shortest path: Dijkstra over lexicographic (hops, heuristic) cost"
    status: complete
    justifies: "FR-22, FR-23, AC-16, AC-17. Answering 'how does A reach B' currently requires the agent to walk get_callers by hand; the confidence-weighted tie-break is what stops the tool from recommending a heuristic chain when a resolved one of equal length exists."
    verification: "cargo test -p code-graph-graph shortest_path — a connected pair returns a sequence starting at the source and ending at the target where every adjacent pair is a real edge (AC-16); an unconnected pair returns not-found; two equal-hop paths where only one is all-resolved return the resolved one (AC-17); a node_cap smaller than the graph requires returns not-found with cap_reached true and never a partial path."
  - id: "1.3"
    title: "File-graph aggregation and community detection"
    status: complete
    justifies: "FR-24, FR-25, FR-44, FR-45, FR-46, NFR-03, AC-18, AC-32, AC-53, AC-54, AC-55. get_coupling gives pairwise weights but nothing tells an agent what the de-facto modules are, which is the first question asked of an unfamiliar codebase."
    verification: "cargo test -p code-graph-graph community — two dense clusters joined by one edge separate; a star graph reports Giant; an edgeless graph reports Atomized; a nine-node collapse reports no degeneracy, pinning the node_count >= 10 floor (AC-55); clusters are ordered by descending size with both caps enforced and echoed, and a member-capped cluster carries truncated plus original_len (AC-32); granularity and termination condition appear in the response (AC-53, AC-54); running file_communities 20 times on one graph yields byte-identical output including labels (NFR-03)."
    depends_on: ["1.1"]
  - id: "1.4"
    title: "Tool wiring, descriptions, and documentation"
    status: complete
    justifies: "FR-26, NFR-11, AC-33 (MCP half), AC-45. Tool descriptions are production behavior that agents pattern-match on; a get_symbol_at description implying goto-definition would cause misuse that no test catches."
    verification: "cargo test -p code-graph-tools --test snapshot_tools_list — the tool-list snapshot rebaselines from 19 to 22 tools and no other snapshot changes; each description names its response envelope, documents every argument with default and ceiling, and get_symbol_at is explicitly not described as goto-definition; make verify passes."
    depends_on: ["1.1", "1.2", "1.3"]
  - id: "1.5"
    title: "Dispatch find_path off the async runtime (review F-01)"
    status: complete
    justifies: "Review finding F-01, reached independently by review_quality and review_blind_spots. Prevents one tokio worker being occupied for the length of an unbounded Dijkstra while holding the graph read lock, which stalls a concurrent watch-driven reindex needing the write lock."
    verification: "cargo test -p code-graph-tools find_path:: and make verify — find_path's #[tool] wrapper dispatches through tokio::task::spawn_blocking and maps a join error through tool_error, matching the get_symbol_at and detect_communities precedent set in the same phase; behaviour and response bytes are unchanged."
  - id: "1.6"
    title: "Add handler response snapshots for the three new tools (review F-03)"
    status: complete
    justifies: "Review finding F-03, and AC-18/NFR-03 as the design states them: determinism needs the 20-run unit test AND a committed insta golden file, 'neither alone closes AC-18'. Without a golden, a change to field order, label derivation, or community ranking passes silently."
    verification: "cargo test -p code-graph-tools --test snapshot_responses — committed snapshots exist for get_symbol_at, find_path, and detect_communities using the established build_indexed_fixture/parsed_sorted/settings_with_path_redaction helpers, including a member-capped community showing truncated plus original_len (AC-32) and the granularity and termination fields (AC-53, AC-54); make snapshot-clean passes."
  - id: "1.7"
    title: "Apply saturating arithmetic at the two sites the design names (review F-06)"
    status: complete
    justifies: "Review finding F-06. The design's Structural Verification section requires saturating arithmetic at the packed Dijkstra cost and the permille share so a pathological graph degrades rather than panicking in debug or wrapping in release; both currently use plain arithmetic."
    verification: "cargo test -p code-graph-graph and make verify — the packed cost accumulation in shortest_path and the permille share in detect_degeneracy use saturating operations; existing tests still pass, confirming no behavioural change at realistic magnitudes."
---

# Phase 1: Graph Queries

## Overview

**Status: all seven tasks complete; the phase is deliberately left `in-progress`.** Formal completion requires a four-lane review returning Aligned across every lane. Two cycles ran: cycle 1 returned Moderate with seven findings, cycle 2 returned Strong on drift, quality, and spec-compliance but Elevated on blind spots with two more. All nine are fixed and recorded in `reviews/01-graph-platform-expansion-code-review-2986df0.md`. A third cycle would be needed to certify, and was skipped by explicit decision — the returns were diminishing and seven phases follow. The phase is therefore code-complete and review-evidenced, but not certified, and `Phase Completion Evidence` below stays pending rather than claiming a gate that was not run.

Three self-contained read-only queries added to `code-graph-graph`, with thin handlers and three new MCP tools. No cache-format change, no change to any existing tool's output. This phase depends on nothing else in the plan and can run first, or concurrently with phases 3 and 5. Phase 2 follows it rather than running beside it — both edit the same three handler files, and phase 2's migration is what carries these three queries into the typed core.

## 1.1: Position lookup: symbols_at_line and get_symbol_at

### Subtasks
- [x] Add `Graph::symbols_at_line(&self, path: &Path, line: u32) -> Vec<Symbol>` in `crates/code-graph-graph/src/queries.rs`, filtering the file's symbols on `line <= L <= end_line`
- [x] Sort candidates by `(span_lines asc, line desc, symbol_id asc)` so the order is total, not merely mostly-determined
- [x] Guard malformed spans (`end_line < line`) as zero-width rather than dropping or panicking
- [x] Add `EnclosingSymbol` and the `get_symbol_at` handler returning `Page<EnclosingSymbol>`
- [x] Reject `line = 0` as a tool error; return an empty page for a line enclosed by nothing
- [x] Unit tests for nesting, empty result, malformed span, and repeated-call stability

### Notes
Revision boundary: `get_symbol_at` answers position queries end to end — graph function, handler, and tests — with no tool registered yet. The tool registration lands in 1.4 so the tool-list snapshot moves exactly once for all three queries.

The candidate set is one file's symbols via the `files` PathTrie, so a linear scan is over tens to hundreds of entries. Do not build a line index: `FileEntry` is encoded in `PackedSymbol`, so adding a field forces a `CACHE_VERSION` bump and breaks AC-15.

`symbol_id` ascending is not decoration. Java anonymous-class methods can produce two symbols with the *same* id distinguished only by `line` (CLAUDE.md, Java limitation 5), so without a total order the response varies between runs and cannot be snapshotted.

### Completion Evidence

- Verified: 2026-08-08
- Repository: `~/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `50450d5065bcf4254cbca4624d196fdb9389189f`
- Identity recheck: `git rev-parse HEAD` at 2026-08-08T14:05, matching `50450d5065bcf4254cbca4624d196fdb9389189f`
- Focused review: `git show 50450d5065bcf4254cbca4624d196fdb9389189f`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `50450d5065bcf4254cbca4624d196fdb9389189f`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-graph` | `.` | PASS (`exit 0`) | 142 passed, incl. nesting order, empty result, malformed span, 20-run stability |
| `cargo test -p code-graph-tools` | `.` | PASS (`exit 0`) | all suites pass; new get_symbol_at cases for line 0, unknown file, empty page |
| `make verify` | `.` | PASS (`exit 0`) | clippy -D warnings, rustfmt, full workspace tests, no pending snapshots, plugin mirrors in sync |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Diff read of `symbols_at_line` | `crates/code-graph-graph/src/queries.rs` | PASS | sort is `(span asc, line desc, symbol_id asc)`; malformed span clamped via `end_line.max(line)`; saturating span arithmetic |
| `git show --stat` | `.` | PASS | touches only queries.rs, handlers/mod.rs, handlers/symbols.rs — `server.rs` untouched, so no snapshot movement |

### Trap
You will want to describe this as goto-definition, or to make it resolve the identifier *at* the position. Don't. It answers "what encloses this line" by span containment; resolving an identifier to its binding needs scope resolution, which the spec rules out as a Non-Goal. The tool description in 1.4 must say so explicitly.

## 1.2: Shortest path: Dijkstra over lexicographic (hops, heuristic) cost

### Subtasks
- [x] Add `PathHop`, `PathResult`, and `Graph::shortest_path` in `crates/code-graph-graph/src/callgraph.rs`
- [x] Implement Dijkstra with cost packed as `((hops as u64) << 32) | (heuristic_hops as u64)`
- [x] Reuse the existing edge semantics exactly: forward `adj` only, `EdgeKind::Calls` only, `is_resolved_node` before enqueue, `min_confidence` filtering per hop
- [x] Enforce `node_cap` (default 100_000, ceiling 5_000_000), returning not-found with `cap_reached` rather than a partial path
- [x] Break equal-cost ties on `symbol_id` ascending
- [x] Add the `find_path` handler returning a single object, not a `Page`
- [x] Tests: connected, unconnected, equal-hop resolved-vs-heuristic, cap exhaustion, source equals target

### Notes
Revision boundary: `find_path` answers reachability end to end, tool registration deferred to 1.4.

The lexicographic cost is the crux. Hops is the primary key so the result is always genuinely shortest; the heuristic count only breaks ties among equally short paths. Scalar weights (Resolved 1 / Heuristic 3) were rejected because a 2-hop resolved path would beat a 1-hop heuristic one, meaning the tool returns a longer path and calls it shortest.

`Confidence` implements no `Ord` — the existing BFS compares with strict equality against `Some(Confidence::Resolved)`. Match that; do not invent an ordering.

Source equals target is a success with `hop_count: 0`, not an error.

### Completion Evidence

- Verified: 2026-08-08
- Repository: `~/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `35381a247b98367f34df2e085c2171c30d0f541f`
- Identity recheck: `git rev-parse 35381a2` at 2026-08-08T15:20, matching `35381a247b98367f34df2e085c2171c30d0f541f`
- Focused review: `git show 35381a247b98367f34df2e085c2171c30d0f541f`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `35381a247b98367f34df2e085c2171c30d0f541f`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-graph shortest_path` | `.` | PASS (`exit 0`) | 7 path tests pass, incl. the equal-hop resolved-vs-heuristic discriminator (AC-17) and cap exhaustion returning not-found (AC-16) |
| `make verify` | `.` | PASS (`exit 0`) | clippy -D warnings, rustfmt, full workspace tests, no pending snapshots |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Diff read of the cost packing | `.` | PASS | `((new_hops as u64) << 32) | (new_heuristic as u64)` — widened before the shift, so no full-width shift |
| Diff read of edge semantics | `.` | PASS | forward adj only, EdgeKind::Calls only, is_resolved_node before enqueue, min_confidence by strict equality — matches the existing bfs |

### Trap
`hops << 32` where `hops` is a `u32` is a shift by the full bit width: it panics in debug and evaluates to `0` in release. The widen to `u64` must happen before the shift. This is the single most likely defect in the task and it will pass a casual read.

## 1.3: File-graph aggregation and community detection

### Subtasks
- [x] Add `crates/code-graph-graph/src/community.rs` with `aggregate_file_edges` — one pass over the `files` trie, assigning indices in iteration order
- [x] Filter to `EdgeKind::Calls` when reading `adj`; fold include edges separately
- [x] Skip self-pairs (`i == j`) before the weight table
- [x] Implement label propagation: ascending-index sweep, max summed neighbour weight, ties to smallest label, keep current label when among maxima
- [x] Terminate on no-change or `max_iterations` (default 50, ceiling 500); report which
- [x] Derive labels: longest directory prefix shared by most members, lexicographically smallest prefix on a tie, smallest member path when no shared prefix
- [x] Detect degeneracy: `Giant` at >= 900 permille with `node_count >= 10`; `Atomized` when communities equal nodes and `node_count > 1`
- [x] Add the `detect_communities` handler with `Page<Community>` flattened plus envelope fields, per the `SearchSymbolsResponse` precedent
- [x] Tests per the verification field, including the 20-run determinism check

### Notes
Revision boundary: community detection answers module-structure queries end to end, tool registration deferred to 1.4.

Determinism comes from driving iteration off the `files` PathTrie, whose DFS pre-sorts children by path segment. The aggregator must only ever do *keyed* lookups into `nodes`/`adj` — those are `std::collections::HashMap` with a randomly seeded hasher, and iterating either one anywhere in this file reintroduces nondeterminism that the 20-run test is specifically there to catch.

Do not call `Graph::coupling` per file. `incoming_coupling` scans every file's include list (no reverse index), so a per-file loop is O(F x N x M) and fails NFR-08 on any real corpus.

Two caps are independent and must not be conflated: `limit`/`offset` page over communities against the byte budget; `members_per_community` caps the member list inside each. A member-capped community sets `truncated` and carries `original_len`, mirroring `Cycle` exactly — and as with `Cycle`, neither `truncated` implies the other.

### Completion Evidence

- Verified: 2026-08-08
- Repository: `~/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `84c65661384d8e6b75765d66b26c4ebb26cf9073`
- Identity recheck: `git rev-parse 84c6566` at 2026-08-08T15:20, matching `84c65661384d8e6b75765d66b26c4ebb26cf9073`
- Focused review: `git show 84c65661384d8e6b75765d66b26c4ebb26cf9073`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `84c65661384d8e6b75765d66b26c4ebb26cf9073`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-graph community` | `.` | PASS (`exit 0`) | 16 tests pass, incl. self-loop bias, edge-kind filter, the nine-node degeneracy floor, and 20-run byte-identical determinism |
| `make verify` | `.` | PASS (`exit 0`) | all structural checks pass |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| grep for HashMap iteration in community.rs | `.` | PASS | no `for .. in self.nodes/adj/radj` and no .iter()/.values()/.keys() on them — keyed lookups only, so hash order cannot reach the output |
| Diff read of aggregate_file_edges | `.` | PASS | `edge.kind != EdgeKind::Calls` filter present; `i == j` self-pairs skipped before the weight table |

### Trap
Folding same-file edges into the weight table looks harmless and is not. Intra-file calls are typically the majority of all call edges; each becomes a self-loop inflating every node's vote for its own current label on every sweep, biasing propagation toward stasis. The partition will look plausible and be quietly worse — no test fails unless you write the one that pins it.

## 1.4: Tool wiring, descriptions, and documentation

### Subtasks
- [x] Add `GetSymbolAtArgs`, `FindPathArgs`, `DetectCommunitiesArgs` to `server.rs` with `#[schemars(description)]` on every field
- [x] Add three `#[tool(description = ...)]` methods, `require_indexed()` first, `spawn_blocking` where path normalization occurs
- [x] Write descriptions under the agent-facing lens: envelope named, every argument with default and ceiling, `get_symbol_at` explicitly not goto-definition
- [x] Rebaseline the tool-list snapshot 19 → 22 and confirm no response snapshot moves
- [x] Update CLAUDE.md: tool count, the three tools in the MCP tools table, response shapes, and the determinism note for `detect_communities`
- [x] Run the phase 1 performance measurement and record the numbers in `notes/`

### Notes
Revision boundary: the three tools are live on the MCP surface and documented. This is the only commit in the phase that changes `server.rs` or any snapshot.

AC-43 is a recorded-metric criterion, not an automated gate — a wall-clock assertion would be flaky across machines. What *is* automated is cap enforcement, covered in 1.2 and 1.3. Record `find_path` worst-case and `file_communities` timings against the largest initialised dogfood corpus.

AC-33 is only half-satisfiable here: the CLI half needs phase 7. Do not mark AC-33 complete in this phase.

These three handlers land in `handlers/symbols.rs`, `handlers/query.rs`, and `handlers/structure.rs` as ordinary `CallToolResult`-returning functions, matching every existing handler. **Phase 2 migrates them into `core/` along with the originals** — that is what puts them within the CLI's reach and why phase 2 is sequenced after this one. Do not pre-emptively write them against a typed core that does not exist yet.

### Completion Evidence

- Verified: 2026-08-08
- Repository: `~/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `2986df0e192517e51fa4f9f59717c58f9a4dd9c6`
- Identity recheck: `git rev-parse 2986df0` at 2026-08-08T15:20, matching `2986df0e192517e51fa4f9f59717c58f9a4dd9c6`
- Focused review: `git show 2986df0e192517e51fa4f9f59717c58f9a4dd9c6`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `2986df0e192517e51fa4f9f59717c58f9a4dd9c6`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `make verify` | `.` | PASS (`exit 0`) | all checks pass; tool-list snapshot rebaselined 19 -> 22 |
| `git status on tests/snapshots` | `.` | PASS (`exit 0`) | three new .snap files only; no existing snapshot modified |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Read of the three tool descriptions | `.` | PASS | each names its envelope, documents every arg with default and ceiling; get_symbol_at explicitly disclaims goto-definition (NFR-11, AC-45) |

## 1.5: Dispatch find_path off the async runtime (review F-01)

### Subtasks
- [x] Wrap `handlers::query::find_path` in `tokio::task::spawn_blocking` in its `#[tool]` method
- [x] Clone the `Arc<ServerInner>` and move owned arguments into the closure, as `get_coupling` does
- [x] Map the join error through `handlers::tool_error`
- [x] Confirm no response bytes change

### Notes
Revision boundary: all three phase 1 tools dispatch consistently.

`find_path` was the outlier: no `.await` exists anywhere in its chain and it holds a `parking_lot` read guard for the whole search, so the scheduler cannot preempt it. Its two siblings, added in the same commit, already do this.

The tests cannot catch this class of bug as written — every one of them calls the handler function directly and never goes through the `#[tool]` wrapper. That is worth knowing before trusting a green suite here.

### Completion Evidence

- Verified: 2026-08-08
- Repository: `~/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `8d3e7ca9478898f4293429ba6fdfe0bfd868d291`
- Identity recheck: `git rev-parse 8d3e7ca` at 2026-08-08T15:20, matching `8d3e7ca9478898f4293429ba6fdfe0bfd868d291`
- Focused review: `git show 8d3e7ca9478898f4293429ba6fdfe0bfd868d291`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `8d3e7ca9478898f4293429ba6fdfe0bfd868d291`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools` | `.` | PASS (`exit 0`) | all suites pass; response bytes unchanged |
| `make verify` | `.` | PASS (`exit 0`) | all structural checks pass |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Diff read of the find_path #[tool] method | `.` | PASS | dispatches via tokio::task::spawn_blocking with a join error mapped through tool_error, matching the get_coupling and detect_communities precedent |

## 1.6: Add handler response snapshots for the three new tools (review F-03)

### Subtasks
- [x] Add `snapshot_responses.rs` cases for `get_symbol_at`, `find_path`, and `detect_communities`
- [x] Reuse `build_indexed_fixture`, `parsed_sorted`, and `settings_with_path_redaction` — no new snapshot infrastructure
- [x] Cover a member-capped community carrying `truncated` and `original_len` (AC-32)
- [x] Cover the `granularity` and `termination` fields (AC-53, AC-54)
- [x] Accept the new snapshots and confirm no existing snapshot moved

### Notes
Revision boundary: AC-18 is closed by both halves — the 20-run determinism test and a committed golden.

The unit test proves the output is stable across runs; the golden proves it is the output we meant. A field-order change or a different label tie-break satisfies the first and fails the second, which is exactly why the design insisted on both.

### Completion Evidence

- Verified: 2026-08-08
- Repository: `~/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `28ac556e26e0e3a2734c49bc5429db2577671c05`
- Identity recheck: `git rev-parse 28ac556` at 2026-08-08T15:20, matching `28ac556e26e0e3a2734c49bc5429db2577671c05`
- Focused review: `git show 28ac556e26e0e3a2734c49bc5429db2577671c05`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `28ac556e26e0e3a2734c49bc5429db2577671c05`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools --test snapshot_responses` | `.` | PASS (`exit 0`) | 59 pass, incl. six new goldens for the three phase 1 tools |
| `make snapshot-clean` | `.` | PASS (`exit 0`) | no pending snapshots |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| git status on tests/snapshots | `.` | PASS | six new .snap files; no existing snapshot modified |
| Read of the member-capped golden | `.` | PASS | pins truncated: true with original_len: 5 — AC-32's evidence |

## 1.7: Apply saturating arithmetic at the two sites the design names (review F-06)

### Subtasks
- [x] Use saturating accumulation for hops and heuristic hops in the packed Dijkstra cost
- [x] Use saturating operations for the permille share in degeneracy detection
- [x] Confirm existing tests pass unchanged

### Notes
Revision boundary: the design's stated overflow mitigation is actually in force.

No bug is observed today — both values are bounded well below overflow by the `node_cap` ceiling and realistic corpus sizes. This closes the gap between what the design promises and what the code does, so a future change to the ceiling cannot quietly turn a documented mitigation into a panic.

### Completion Evidence

- Verified: 2026-08-08
- Repository: `~/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `a407b4108a8ee5870f1f92759d631063081dbb64`
- Identity recheck: `git rev-parse a407b41` at 2026-08-08T15:20, matching `a407b4108a8ee5870f1f92759d631063081dbb64`
- Focused review: `git show a407b4108a8ee5870f1f92759d631063081dbb64`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `a407b4108a8ee5870f1f92759d631063081dbb64`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-graph` | `.` | PASS (`exit 0`) | 165 pass unchanged, confirming no behavioural change at realistic magnitudes |
| `make verify` | `.` | PASS (`exit 0`) | all structural checks pass |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Diff read of the two named sites | `.` | PASS | saturating_add on the hop and heuristic accumulation; saturating_mul on the permille share; the u64 widen-before-shift left untouched |

## Acceptance Criteria

- [x] **AC-13**: Position lookup on a line inside a method nested in a class returns the method first, the class after.
- [x] **AC-14**: A line belonging to no symbol returns an empty result — not an error, not a nearest-neighbour guess.
- [x] **AC-15**: No cache-version bump; a cache built before this phase is readable unchanged.
- [x] **AC-16**: Shortest path returns a source-to-target chain of real edges, or an explicit not-found distinguishable from cap exhaustion.
- [x] **AC-17**: Among equal-hop paths, the all-resolved path wins.
- [x] **AC-18**: Community detection is deterministic across runs and captured by an `insta` snapshot.
- [x] **AC-32**: Clusters ranked by descending size, both caps enforced and echoed, truncated clusters carry their true total.
- [x] **AC-53**: Granularity used is present in the response and defaults to file.
- [x] **AC-54**: Termination condition reported; no tuning parameter required.
- [x] **AC-55**: Degenerate partitions flagged rather than returned as ordinary results.
- [x] **AC-43**: Both queries complete within an interactive budget on the largest initialised corpus, timings recorded (NFR-08).
- [x] **AC-45**: The three new tool descriptions meet the agent-facing-description lens (NFR-11).
- [x] **AC-27**: `make verify` passes (NFR-04).
- [x] FR-21, FR-22, FR-23, FR-24, FR-25, FR-26, FR-44, FR-45, FR-46 realized; NFR-03 and NFR-08 satisfied.

## Phase Completion Evidence

Pending — not complete.
