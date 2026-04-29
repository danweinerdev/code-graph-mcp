---
title: "Graph Engine & LLM Optimizations"
type: phase
plan: RustRewrite
phase: 2
status: in-progress
created: 2026-04-28
updated: 2026-04-28
deliverable: "codegraph-graph crate with the full query surface, all algorithms (BFS callers/callees, iterative Tarjan SCC, diamond-safe class hierarchy), LLM-optimized search (brief mode, pagination envelope, namespace filter, language filter, namespace summary), and the RenderMermaid Mermaid string generator"
tasks:
  - id: "2.1"
    title: "Graph struct, FileEntry with Language, merge/remove/clear"
    status: complete
    verification: "Graph::new() initializes all maps non-null; FileEntry carries both `language` and `symbol_ids`; merge_file_graph adds nodes and edges, replaces stale data on re-merge from the same path, splits Calls/Inherits into adj+radj and Includes into the includes map; remove_file deletes nodes, adj entries originating from path, radj entries originating from path, includes for path, and the files entry; clear() resets to empty; tests cover merge-one-file, merge-two-files, re-merge-same-file (idempotent), remove-file, clear, and stats reporting consistent counts"
  - id: "2.2"
    title: "Symbol queries: file_symbols, symbol_detail, search with pagination, symbol_summary"
    status: complete
    depends_on: ["2.1"]
    verification: "file_symbols returns Vec (never Option) for known and unknown paths — empty for unknown so JSON serializes as []; symbol_detail returns Some for known IDs and None for unknown; SearchParams supports pattern (regex with case-insensitive substring fallback), kind, namespace (substring), language (NEW — exact match, optional), limit (default 20), offset (default 0); SearchResult has `symbols` and `total`; results sorted by SymbolID for stable pagination; tests cover regex match, substring fallback when regex invalid, kind filter, namespace filter, language filter, all-filters-combined, empty pattern, offset beyond total returns empty with correct total; symbol_summary groups by namespace and kind, handles file=None (whole graph) and file=Some(path) (scoped); empty graph returns empty map (not null)"
  - id: "2.3"
    title: "Call graph: callers, callees BFS; orphans; file_dependencies"
    status: complete
    depends_on: ["2.1"]
    verification: "callers and callees BFS visit each node at most once via a HashSet visited; depth=0 treated as 1 (matches Go); cycles do not infinite-loop (verified by a 3-node cycle fixture); CallChain carries SymbolID, file, line, depth; orphans returns symbols with no incoming Call edges, defaults to callables only when kind=None, accepts kind filter; file_dependencies returns Vec<PathBuf> never Option, empty Vec for unknown paths"
  - id: "2.4"
    title: "Tarjan SCC (iterative) and diamond-safe class hierarchy"
    status: complete
    depends_on: ["2.1"]
    verification: "detect_cycles uses an iterative Tarjan implementation (no recursion — explicit Vec stack so deep include graphs don't overflow); reports only SCCs of size > 1; tested on acyclic, 2-node cycle, 3-node cycle, mixed cyclic-and-acyclic graphs; class_hierarchy accepts root symbols whose kind is in {Class, Struct, Interface, Trait} (the widened filter from the design — without this, Rust traits and Go interfaces hierarchy lookups return None); diamond inheritance fixture (4-level chain Root←Base←{MixinA, MixinB}←Derived←Leaf, depth=3) fully expands the shared ancestor under both arms — verified by reverting the per-DFS-path tracking and watching the test fail (matches the Go regression-test discipline)"
  - id: "2.5"
    title: "Coupling and Diagrams (call/file/inheritance) with RenderMermaid"
    status: planned
    depends_on: ["2.3", "2.4"]
    verification: "coupling returns outgoing cross-file edge counts (calls + includes), incoming_coupling returns reverse direction; both return HashMap<PathBuf, u32> never Option; DiagramCallGraph performs BFS bounded by depth and max_nodes, includes both forward and reverse edges, deduplicates; DiagramFileGraph BFS over includes; DiagramInheritance BFS over Inherits edges starting from the widened kind filter; RenderMermaid produces valid Mermaid graph syntax with shortened node IDs (n0, n1...), preserves edge labels (calls/includes/inherits), supports `styled` flag adding center-node CSS class; empty diagrams render as empty string (or empty edges array — same wire-format invariant)"
  - id: "2.6"
    title: "Concurrency safety with parking_lot::RwLock"
    status: planned
    depends_on: ["2.2", "2.3", "2.4", "2.5"]
    verification: "Graph wrapped behind parking_lot::RwLock for production use; concurrent test spawns 10 reader threads (calling search/callers/symbol_summary in a loop) and 2 writer threads (calling merge_file_graph in a loop) for at least 1s; test passes under `cargo test` and `cargo +nightly miri test` (where applicable); no deadlocks; results from readers are never partially-merged states (every merge_file_graph is atomic from the reader's perspective via the write lock)"
  - id: "2.7"
    title: "Structural verification"
    status: planned
    depends_on: ["2.6"]
    verification: "`cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean across all crates added in this phase; `cargo test --workspace` green including all graph engine tests, LLM-optimization tests, diamond hierarchy regression test, Tarjan SCC tests, concurrent reader/writer test; no new unsafe blocks; no #[allow] attributes suppressing clippy findings"
---

# Phase 2: Graph Engine & LLM Optimizations

## Overview

Build the in-memory graph engine that stores all extracted symbols and edges, with the full query and algorithm surface plus every LLM-optimized output behavior the Go binary ships today: brief mode by default, pagination envelope, namespace filter, language filter (new), namespace summary, and the diamond-safe class hierarchy traversal that closes the regression caught in `Designs/LLMOptimization/notes/01-Implementation.md`. The Mermaid string renderer also lives here so Phase 3's `generate_diagram` handler stays a thin parameter-marshalling wrapper with no graph logic of its own.

This phase has zero MCP dependency — `codegraph-graph` is unit-testable with no async runtime and no rmcp surface. That isolation is intentional: the engine is the heart of the binary and gets exercised in the most adversarial (concurrent reader/writer + race detector) test environment we can build for it.

## 2.1: Graph struct, FileEntry with Language, merge/remove/clear

### Subtasks
- [x] `Graph` struct in `codegraph-graph::graph` with fields: `nodes: HashMap<SymbolId, Node>`, `adj: HashMap<SymbolId, Vec<EdgeEntry>>`, `radj: HashMap<SymbolId, Vec<EdgeEntry>>`, `files: HashMap<PathBuf, FileEntry>`, `includes: HashMap<PathBuf, Vec<PathBuf>>`
- [x] `FileEntry { language: Language, symbol_ids: Vec<SymbolId> }` — Language is captured per-file so cache v2 can record it without extension re-derivation
- [x] `Node { symbol: Symbol }` plus `EdgeEntry { target: SymbolId, kind: EdgeKind, file: PathBuf, line: u32 }`
- [x] `Graph::new() -> Self` initializes all maps with `HashMap::new()` (never returns Option)
- [x] `merge_file_graph(&mut self, fg: FileGraph)` removes any pre-existing data for fg.path, then adds all symbols as nodes and routes edges into adj/radj/includes by kind
- [x] `remove_file(&mut self, path: &Path)` — implementation iterates adj/radj entries by source file (matches Go's `e.File != path` filter), trims empty key entries
- [x] `clear(&mut self)` resets all maps
- [x] Tests: merge-one-file produces correct node and edge counts; merge-two-files; re-merge same path is idempotent; remove-file cleans up all four storage maps; clear produces empty Stats

## 2.2: Symbol queries: file_symbols, symbol_detail, search, symbol_summary

### Subtasks
- [x] `file_symbols(&self, path: &Path) -> Vec<Symbol>` (cloned Vec, never Option, never null in JSON)
- [x] `symbol_detail(&self, id: &SymbolId) -> Option<Symbol>` (None is fine — handler converts to McpError with did-you-mean)
- [x] `SearchParams { pattern, kind: Option<SymbolKind>, namespace, language: Option<Language>, limit, offset }`
- [x] `SearchResult { symbols: Vec<Symbol>, total: u32 }` — symbols always Vec; total always present
- [x] `Search` collects matches → sorts by SymbolID → applies offset/limit; returns total before slicing for pagination correctness
- [x] Regex match: `Regex::new("(?i)" + pattern)` first; on compile error fall back to case-insensitive substring contains
- [x] Namespace filter: case-insensitive substring (matches Go); empty namespace = no filter
- [x] **Language filter (NEW):** exact-match `Option<Language>`; None = no filter
- [x] `SearchSymbols` legacy wrapper for `suggestSymbols` did-you-mean — passes `Limit: 100` explicitly so the candidate pool isn't capped at 20 (LLMOptimization debrief carry-forward)
- [x] `symbol_summary(&self, file: Option<&Path>) -> HashMap<String, HashMap<SymbolKind, u32>>` — empty graph returns empty map, file=Some scopes to that file
- [x] Tests: regex hit; substring fallback when regex compile fails; kind filter; namespace filter (substring, case-insensitive); language filter — Rust-only matches don't return Cpp results; pagination boundaries (offset=total, offset>total, limit=0 treated as default 20); summary empty-graph; summary file-scope

## 2.3: Call graph: callers, callees BFS; orphans; file_dependencies

### Subtasks
- [x] `callers(&self, id: &SymbolId, depth: u32) -> Vec<CallChain>` — BFS over radj filtering by `EdgeKind::Calls`
- [x] `callees(&self, id: &SymbolId, depth: u32) -> Vec<CallChain>` — BFS over adj
- [x] BFS uses HashSet<SymbolId> visited to handle cycles without infinite loop
- [x] depth=0 normalized to 1 (matches Go behavior; otherwise an agent passing depth=0 gets empty results which is confusing)
- [x] CallChain { symbol_id, file, line, depth }
- [x] `orphans(&self, kind: Option<SymbolKind>) -> Vec<Symbol>` — symbols with no incoming Calls edges; default (kind=None) = callables only (Function or Method)
- [x] `file_dependencies(&self, path: &Path) -> Vec<PathBuf>` — never Option; empty Vec for unknown paths
- [x] Tests: linear chain (3 nodes, depth=2); diamond; 3-node cycle; unknown symbol returns empty; depth=0 normalized; orphan defaults exclude classes/structs; orphan with kind=Class returns class symbols; file_dependencies for known path; file_dependencies for unknown path returns []

## 2.4: Tarjan SCC and diamond-safe class hierarchy

### Subtasks
- [x] `detect_cycles(&self) -> Vec<Vec<PathBuf>>` — iterative Tarjan on the file include graph; uses an explicit stack to avoid recursive call-depth overflow on large include graphs
- [x] Only SCCs with size > 1 are reported (single-node SCCs are not cycles)
- [x] `class_hierarchy(&self, name: &str, depth: u32) -> Option<HierarchyNode>` — root lookup checks `kind in {Class, Struct, Interface, Trait}`
- [x] HierarchyNode { name, bases: Vec<HierarchyNode>, derived: Vec<HierarchyNode> }
- [x] DFS uses **per-DFS-path tracking** (`HashSet<&str>` inserted on enter, removed on leave) — NOT a global visited set. Diamond inheritance must fully expand the shared ancestor under both arms.
- [x] depth=0 normalized to 1
- [x] Tests: acyclic include graph (no SCCs reported); 2-node cycle (one SCC of size 2); 3-node cycle; mixed graph; class_hierarchy with depth=1 returns direct only; class_hierarchy widened-filter test (a Rust trait root resolves correctly); **diamond regression: 4-level chain Root←Base←{MixinA, MixinB}←Derived←Leaf at depth=3** — the test must fail when the per-DFS-path logic is reverted to a global-visited implementation (verify by temporary patch revert)

### Notes
The 4-level diamond fixture is the same one used in the Go regression test (`TestClassHierarchyDiamond`). The 3-class diamond at depth=2 produces identical output under both buggy and fixed code because the shared node bottoms out as a leaf either way; the 4-level chain at depth=3 is the minimal fixture that actually exposes the bug.

## 2.5: Coupling and Diagrams with RenderMermaid

### Subtasks
- [ ] `coupling(&self, path: &Path) -> HashMap<PathBuf, u32>` — outgoing: cross-file calls + includes; never Option
- [ ] `incoming_coupling(&self, path: &Path) -> HashMap<PathBuf, u32>` — reverse direction
- [ ] `DiagramResult { center: String, edges: Vec<DiagramEdge> }`; `DiagramEdge { from, to, label }`
- [ ] `diagram_call_graph(&self, id: &SymbolId, depth, max_nodes) -> Option<DiagramResult>` — BFS over both adj and radj filtering by Calls; deduplicates edges; bounded by max_nodes
- [ ] `diagram_file_graph(&self, file: &Path, depth, max_nodes) -> Option<DiagramResult>` — BFS over includes
- [ ] `diagram_inheritance(&self, name: &str, depth, max_nodes) -> Option<DiagramResult>` — uses the widened {Class, Struct, Interface, Trait} root filter
- [ ] `DiagramResult::render_mermaid(&self, direction: &str, styled: bool) -> String` — emits valid Mermaid `graph DIR` syntax; node IDs shortened to `n0`, `n1`, ...; preserves edge labels; styled mode adds `classDef center fill:#f96,stroke:#333` and tags the center node
- [ ] Empty `DiagramResult::edges` renders as empty string (matches Go); empty edges JSON serializes as `[]` (LLMOptimization invariant)
- [ ] Tests: each of the three diagrams against a known fixture; empty case; max_nodes truncation; styled vs unstyled; direction TD vs BT for inheritance

## 2.6: Concurrency safety with parking_lot::RwLock

### Subtasks
- [ ] Re-export `parking_lot::RwLock as RwLock` in `codegraph-graph::lib` so callers don't accidentally import std's
- [ ] Concurrent test in `tests/concurrent.rs`: spawn 10 reader threads calling `search` / `callers` / `symbol_summary` in a tight loop; spawn 2 writer threads calling `merge_file_graph` with different files in a tight loop; run for ≥ 1 second; assert no panics, no deadlocks, all readers see consistent snapshots (no half-merged state)
- [ ] Run under `cargo test` (default) — Rust's borrow checker + parking_lot semantics catch most issues; `loom` is not introduced for this phase (overkill)

### Notes
Rust doesn't need a `-race` flag — data races are compile-time errors via the borrow checker. The concurrent test's purpose is to exercise the locking *correctness* (no deadlock, fair access patterns, no logical race in code that uses the lock) rather than detect raw memory races.

## 2.7: Structural verification

### Subtasks
- [ ] `cargo fmt --check` clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` green — all Phase 1 tests still pass; all Phase 2 tests pass
- [ ] No new `#[allow]` attributes; no new `unsafe` blocks
- [ ] `cargo doc --workspace --no-deps` builds without warnings (doc comments are valid)

## Acceptance Criteria
- [ ] Graph struct + algorithms ported with full coverage
- [ ] Diamond-inheritance regression test passes (and verified to fail under reverted patch)
- [ ] LLM optimizations: brief default semantics, pagination envelope, namespace filter, language filter, summary all working
- [ ] Mermaid renderer in `codegraph-graph` produces valid output for all three diagram types
- [ ] Concurrent reader/writer test passes
- [ ] Lint, format, and test gates green
