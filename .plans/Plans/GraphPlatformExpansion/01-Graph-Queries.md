---
title: "Graph Queries"
type: phase
plan: GraphPlatformExpansion
phase: 1
status: planned
created: 2026-08-08
updated: 2026-08-08
deliverable: "Three new MCP tools — get_symbol_at, find_path, detect_communities — answering position, reachability, and module-structure questions the current surface cannot answer."
tasks:
  - id: "1.1"
    title: "Position lookup: symbols_at_line and get_symbol_at"
    status: planned
    justifies: "FR-21, AC-13, AC-14, AC-15. Every entry point into the graph is name-addressed, but an agent reading a diff hunk, a stack trace, or a compiler error holds file:line — without this the graph is unreachable from the information the agent actually has."
    verification: "cargo test -p code-graph-graph symbols_at_line and cargo test -p code-graph-tools get_symbol_at — a line inside a method nested in a class returns the method before the class (AC-13); a line in no span returns an empty Page, not an error and not a nearest guess (AC-14); an existing pre-change cache file loads unchanged, proving no CACHE_VERSION bump (AC-15); two symbols sharing a span return in a stable order across 20 repeated calls."
  - id: "1.2"
    title: "Shortest path: Dijkstra over lexicographic (hops, heuristic) cost"
    status: planned
    justifies: "FR-22, FR-23, AC-16, AC-17. Answering 'how does A reach B' currently requires the agent to walk get_callers by hand; the confidence-weighted tie-break is what stops the tool from recommending a heuristic chain when a resolved one of equal length exists."
    verification: "cargo test -p code-graph-graph shortest_path — a connected pair returns a sequence starting at the source and ending at the target where every adjacent pair is a real edge (AC-16); an unconnected pair returns not-found; two equal-hop paths where only one is all-resolved return the resolved one (AC-17); a node_cap smaller than the graph requires returns not-found with cap_reached true and never a partial path."
  - id: "1.3"
    title: "File-graph aggregation and community detection"
    status: planned
    justifies: "FR-24, FR-25, FR-44, FR-45, FR-46, NFR-03, AC-18, AC-32, AC-53, AC-54, AC-55. get_coupling gives pairwise weights but nothing tells an agent what the de-facto modules are, which is the first question asked of an unfamiliar codebase."
    verification: "cargo test -p code-graph-graph community — two dense clusters joined by one edge separate; a star graph reports Giant; an edgeless graph reports Atomized; a nine-node collapse reports no degeneracy, pinning the node_count >= 10 floor (AC-55); clusters are ordered by descending size with both caps enforced and echoed, and a member-capped cluster carries truncated plus original_len (AC-32); granularity and termination condition appear in the response (AC-53, AC-54); running file_communities 20 times on one graph yields byte-identical output including labels (NFR-03)."
    depends_on: ["1.1"]
  - id: "1.4"
    title: "Tool wiring, descriptions, and documentation"
    status: planned
    justifies: "FR-26, NFR-11, AC-33 (MCP half), AC-45. Tool descriptions are production behavior that agents pattern-match on; a get_symbol_at description implying goto-definition would cause misuse that no test catches."
    verification: "cargo test -p code-graph-tools --test snapshot_tools_list — the tool-list snapshot rebaselines from 19 to 22 tools and no other snapshot changes; each description names its response envelope, documents every argument with default and ceiling, and get_symbol_at is explicitly not described as goto-definition; make verify passes."
    depends_on: ["1.1", "1.2", "1.3"]
---

# Phase 1: Graph Queries

## Overview

Three self-contained read-only queries added to `code-graph-graph`, with thin handlers and three new MCP tools. No cache-format change, no change to any existing tool's output. This phase depends on nothing else in the plan and can run first, or concurrently with phases 3 and 5. Phase 2 follows it rather than running beside it — both edit the same three handler files, and phase 2's migration is what carries these three queries into the typed core.

## 1.1: Position lookup: symbols_at_line and get_symbol_at

### Subtasks
- [ ] Add `Graph::symbols_at_line(&self, path: &Path, line: u32) -> Vec<Symbol>` in `crates/code-graph-graph/src/queries.rs`, filtering the file's symbols on `line <= L <= end_line`
- [ ] Sort candidates by `(span_lines asc, line desc, symbol_id asc)` so the order is total, not merely mostly-determined
- [ ] Guard malformed spans (`end_line < line`) as zero-width rather than dropping or panicking
- [ ] Add `EnclosingSymbol` and the `get_symbol_at` handler returning `Page<EnclosingSymbol>`
- [ ] Reject `line = 0` as a tool error; return an empty page for a line enclosed by nothing
- [ ] Unit tests for nesting, empty result, malformed span, and repeated-call stability

### Notes
Revision boundary: `get_symbol_at` answers position queries end to end — graph function, handler, and tests — with no tool registered yet. The tool registration lands in 1.4 so the tool-list snapshot moves exactly once for all three queries.

The candidate set is one file's symbols via the `files` PathTrie, so a linear scan is over tens to hundreds of entries. Do not build a line index: `FileEntry` is encoded in `PackedSymbol`, so adding a field forces a `CACHE_VERSION` bump and breaks AC-15.

`symbol_id` ascending is not decoration. Java anonymous-class methods can produce two symbols with the *same* id distinguished only by `line` (CLAUDE.md, Java limitation 5), so without a total order the response varies between runs and cannot be snapshotted.

### Completion Evidence

Pending — not complete.

### Trap
You will want to describe this as goto-definition, or to make it resolve the identifier *at* the position. Don't. It answers "what encloses this line" by span containment; resolving an identifier to its binding needs scope resolution, which the spec rules out as a Non-Goal. The tool description in 1.4 must say so explicitly.

## 1.2: Shortest path: Dijkstra over lexicographic (hops, heuristic) cost

### Subtasks
- [ ] Add `PathHop`, `PathResult`, and `Graph::shortest_path` in `crates/code-graph-graph/src/callgraph.rs`
- [ ] Implement Dijkstra with cost packed as `((hops as u64) << 32) | (heuristic_hops as u64)`
- [ ] Reuse the existing edge semantics exactly: forward `adj` only, `EdgeKind::Calls` only, `is_resolved_node` before enqueue, `min_confidence` filtering per hop
- [ ] Enforce `node_cap` (default 100_000, ceiling 5_000_000), returning not-found with `cap_reached` rather than a partial path
- [ ] Break equal-cost ties on `symbol_id` ascending
- [ ] Add the `find_path` handler returning a single object, not a `Page`
- [ ] Tests: connected, unconnected, equal-hop resolved-vs-heuristic, cap exhaustion, source equals target

### Notes
Revision boundary: `find_path` answers reachability end to end, tool registration deferred to 1.4.

The lexicographic cost is the crux. Hops is the primary key so the result is always genuinely shortest; the heuristic count only breaks ties among equally short paths. Scalar weights (Resolved 1 / Heuristic 3) were rejected because a 2-hop resolved path would beat a 1-hop heuristic one, meaning the tool returns a longer path and calls it shortest.

`Confidence` implements no `Ord` — the existing BFS compares with strict equality against `Some(Confidence::Resolved)`. Match that; do not invent an ordering.

Source equals target is a success with `hop_count: 0`, not an error.

### Completion Evidence

Pending — not complete.

### Trap
`hops << 32` where `hops` is a `u32` is a shift by the full bit width: it panics in debug and evaluates to `0` in release. The widen to `u64` must happen before the shift. This is the single most likely defect in the task and it will pass a casual read.

## 1.3: File-graph aggregation and community detection

### Subtasks
- [ ] Add `crates/code-graph-graph/src/community.rs` with `aggregate_file_edges` — one pass over the `files` trie, assigning indices in iteration order
- [ ] Filter to `EdgeKind::Calls` when reading `adj`; fold include edges separately
- [ ] Skip self-pairs (`i == j`) before the weight table
- [ ] Implement label propagation: ascending-index sweep, max summed neighbour weight, ties to smallest label, keep current label when among maxima
- [ ] Terminate on no-change or `max_iterations` (default 50, ceiling 500); report which
- [ ] Derive labels: longest directory prefix shared by most members, lexicographically smallest prefix on a tie, smallest member path when no shared prefix
- [ ] Detect degeneracy: `Giant` at >= 900 permille with `node_count >= 10`; `Atomized` when communities equal nodes and `node_count > 1`
- [ ] Add the `detect_communities` handler with `Page<Community>` flattened plus envelope fields, per the `SearchSymbolsResponse` precedent
- [ ] Tests per the verification field, including the 20-run determinism check

### Notes
Revision boundary: community detection answers module-structure queries end to end, tool registration deferred to 1.4.

Determinism comes from driving iteration off the `files` PathTrie, whose DFS pre-sorts children by path segment. The aggregator must only ever do *keyed* lookups into `nodes`/`adj` — those are `std::collections::HashMap` with a randomly seeded hasher, and iterating either one anywhere in this file reintroduces nondeterminism that the 20-run test is specifically there to catch.

Do not call `Graph::coupling` per file. `incoming_coupling` scans every file's include list (no reverse index), so a per-file loop is O(F x N x M) and fails NFR-08 on any real corpus.

Two caps are independent and must not be conflated: `limit`/`offset` page over communities against the byte budget; `members_per_community` caps the member list inside each. A member-capped community sets `truncated` and carries `original_len`, mirroring `Cycle` exactly — and as with `Cycle`, neither `truncated` implies the other.

### Completion Evidence

Pending — not complete.

### Trap
Folding same-file edges into the weight table looks harmless and is not. Intra-file calls are typically the majority of all call edges; each becomes a self-loop inflating every node's vote for its own current label on every sweep, biasing propagation toward stasis. The partition will look plausible and be quietly worse — no test fails unless you write the one that pins it.

## 1.4: Tool wiring, descriptions, and documentation

### Subtasks
- [ ] Add `GetSymbolAtArgs`, `FindPathArgs`, `DetectCommunitiesArgs` to `server.rs` with `#[schemars(description)]` on every field
- [ ] Add three `#[tool(description = ...)]` methods, `require_indexed()` first, `spawn_blocking` where path normalization occurs
- [ ] Write descriptions under the agent-facing lens: envelope named, every argument with default and ceiling, `get_symbol_at` explicitly not goto-definition
- [ ] Rebaseline the tool-list snapshot 19 → 22 and confirm no response snapshot moves
- [ ] Update CLAUDE.md: tool count, the three tools in the MCP tools table, response shapes, and the determinism note for `detect_communities`
- [ ] Run the phase 1 performance measurement and record the numbers in `notes/`

### Notes
Revision boundary: the three tools are live on the MCP surface and documented. This is the only commit in the phase that changes `server.rs` or any snapshot.

AC-43 is a recorded-metric criterion, not an automated gate — a wall-clock assertion would be flaky across machines. What *is* automated is cap enforcement, covered in 1.2 and 1.3. Record `find_path` worst-case and `file_communities` timings against the largest initialised dogfood corpus.

AC-33 is only half-satisfiable here: the CLI half needs phase 7. Do not mark AC-33 complete in this phase.

These three handlers land in `handlers/symbols.rs`, `handlers/query.rs`, and `handlers/structure.rs` as ordinary `CallToolResult`-returning functions, matching every existing handler. **Phase 2 migrates them into `core/` along with the originals** — that is what puts them within the CLI's reach and why phase 2 is sequenced after this one. Do not pre-emptively write them against a typed core that does not exist yet.

### Completion Evidence

Pending — not complete.

## Acceptance Criteria

- [ ] **AC-13**: Position lookup on a line inside a method nested in a class returns the method first, the class after.
- [ ] **AC-14**: A line belonging to no symbol returns an empty result — not an error, not a nearest-neighbour guess.
- [ ] **AC-15**: No cache-version bump; a cache built before this phase is readable unchanged.
- [ ] **AC-16**: Shortest path returns a source-to-target chain of real edges, or an explicit not-found distinguishable from cap exhaustion.
- [ ] **AC-17**: Among equal-hop paths, the all-resolved path wins.
- [ ] **AC-18**: Community detection is deterministic across runs and captured by an `insta` snapshot.
- [ ] **AC-32**: Clusters ranked by descending size, both caps enforced and echoed, truncated clusters carry their true total.
- [ ] **AC-53**: Granularity used is present in the response and defaults to file.
- [ ] **AC-54**: Termination condition reported; no tuning parameter required.
- [ ] **AC-55**: Degenerate partitions flagged rather than returned as ordinary results.
- [ ] **AC-43**: Both queries complete within an interactive budget on the largest initialised corpus, timings recorded (NFR-08).
- [ ] **AC-45**: The three new tool descriptions meet the agent-facing-description lens (NFR-11).
- [ ] **AC-27**: `make verify` passes (NFR-04).
- [ ] FR-21, FR-22, FR-23, FR-24, FR-25, FR-26, FR-44, FR-45, FR-46 realized; NFR-03 and NFR-08 satisfied.

## Phase Completion Evidence

Pending — not complete.
