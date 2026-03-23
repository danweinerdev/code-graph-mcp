---
title: "Graph Engine"
type: phase
plan: CodeGraphMCP
phase: 4
status: planned
created: 2026-03-22
updated: 2026-03-22
deliverable: "In-memory directed graph with add/remove/query methods, BFS traversal, cycle detection, orphan detection, and concurrent access safety"
tasks:
  - id: "4.1"
    title: "Graph struct and core types"
    status: planned
    verification: "Graph struct compiles with all fields from the design (nodes, adj, radj, files, includes maps). New() constructor initializes all maps. Node and EdgeEntry types defined."
  - id: "4.2"
    title: "MergeFileGraph and RemoveFile"
    status: planned
    verification: "MergeFileGraph: adds all symbols as nodes (keyed by file:name), adds all edges to adj/radj, populates files map. Calling MergeFileGraph twice for the same file replaces (not duplicates) entries. RemoveFile: removes all nodes from that file, removes all edges where source file matches, cleans up files map. Tested with: merge one file, merge two files, merge same file twice (idempotent), remove a file and verify nodes/edges gone."
    depends_on: ["4.1"]
  - id: "4.3"
    title: "FileSymbols and SymbolDetail"
    status: planned
    verification: "FileSymbols(path) returns all symbols in that file; returns empty slice for unknown files. SymbolDetail(id) returns the full node; returns nil for unknown IDs. Both tested."
    depends_on: ["4.2"]
  - id: "4.4"
    title: "SearchSymbols"
    status: planned
    verification: "SearchSymbols with a plain substring matches symbol names case-insensitively. SearchSymbols with a regex pattern works (e.g., `Engine::.*`). Returns empty slice for no matches. Kind filter works when provided (empty string = all kinds). Tested with: exact match, substring, regex, kind filter, no match."
    depends_on: ["4.2"]
  - id: "4.5"
    title: "Callers and Callees (BFS traversal)"
    status: planned
    verification: "Callers(id, depth=1) returns direct callers via reverse adjacency. Callers(id, depth=2) returns transitive callers up to 2 hops. Callees works symmetrically on forward adjacency. Both return empty for unknown symbols. Depth=0 is treated as depth=1. Cycle in call graph does not cause infinite loop (visited set). Tested with: linear chain A->B->C, diamond A->B, A->C, B->D, C->D, cycle A->B->A."
    depends_on: ["4.2"]
  - id: "4.6"
    title: "FileDependencies"
    status: planned
    verification: "FileDependencies(path) returns the list of files included by that path from the includes map. Returns empty slice for unknown files or files with no includes. Tested."
    depends_on: ["4.2"]
  - id: "4.7"
    title: "DetectCycles (Tarjan's SCC)"
    status: planned
    verification: "DetectCycles returns all strongly connected components of size > 1 in the file-level include graph. Returns empty for acyclic graphs. Correctly identifies: single cycle (A->B->A), larger cycle (A->B->C->A), self-loop, independent cycles. Tested with: acyclic graph, 2-node cycle, 3-node cycle, graph with both cyclic and acyclic parts."
    depends_on: ["4.2"]
  - id: "4.8"
    title: "Orphans"
    status: planned
    verification: "Orphans('') returns all symbols with zero incoming 'calls' edges. Orphans('function') filters to only functions. main() functions are included (they typically have no callers). Class definitions are excluded from orphan detection (they are types, not callables) unless kind filter explicitly requests them. Tested with: a graph where some functions are called and some aren't."
    depends_on: ["4.2"]
  - id: "4.9"
    title: "ClassHierarchy"
    status: planned
    verification: "ClassHierarchy(className) returns a tree rooted at the given class showing base classes (upward) and derived classes (downward). Handles: single inheritance, multiple inheritance, deep chains (A extends B extends C). Returns nil for unknown classes. Tested."
    depends_on: ["4.2"]
  - id: "4.10"
    title: "Coupling"
    status: planned
    verification: "Coupling(path) returns a map of other file paths to the number of cross-file edges (calls + includes) between them. Files with no cross-file edges don't appear in the map. Tested with a multi-file graph."
    depends_on: ["4.2"]
  - id: "4.11"
    title: "Concurrent access safety"
    status: planned
    verification: "Test spawns 10 goroutines calling Callers/SearchSymbols/FileSymbols in a tight loop while another goroutine calls MergeFileGraph and RemoveFile repeatedly. Test passes under -race with no data races detected. RWMutex correctly allows concurrent reads but exclusive writes."
    depends_on: ["4.5", "4.4", "4.3", "4.2"]
  - id: "4.12"
    title: "Structural verification"
    status: planned
    verification: "`go vet ./...` passes; `go test -race ./internal/graph/` passes with all tests including concurrent access test"
    depends_on: ["4.11"]
---

# Phase 4: Graph Engine

## Overview

Build the in-memory directed graph in `internal/graph/`. This is a standalone data structure with no dependency on the parser implementation — it only depends on the parser types (Symbol, Edge, etc.) from Phase 1. It can be developed in parallel with Phase 2-3.

The graph provides all the query methods that MCP tool handlers will call in Phase 5.

## 4.1: Graph struct and core types

### Subtasks
- [ ] `internal/graph/graph.go` — Graph struct with `sync.RWMutex`, `nodes`, `adj`, `radj`, `files`, `includes` maps
- [ ] `New() *Graph` constructor — initialize all maps
- [ ] `Node` struct wrapping `parser.Symbol`
- [ ] `EdgeEntry` struct with Target, Kind, File, Line
- [ ] `CallChain` type for BFS results (symbol ID, file, line, depth)
- [ ] `HierarchyNode` type for class hierarchy tree (class name, bases, derived)

## 4.2: MergeFileGraph and RemoveFile

### Subtasks
- [ ] `MergeFileGraph(fg *parser.FileGraph)` — acquires write lock
  - [ ] Generate symbol ID for each symbol: `path:name` for free functions, `path:Parent::name` for methods, `path:name` for types
  - [ ] Add/replace nodes in `nodes` map
  - [ ] Update `files[fg.Path]` with new symbol IDs
  - [ ] For call edges: add to `adj` and `radj`
  - [ ] For include edges: add to `includes` map
  - [ ] For inherit edges: add to `adj` and `radj` with EdgeInherits kind
  - [ ] If file was previously indexed, call `removeFileUnsafe()` first to clear stale data
- [ ] `RemoveFile(path string)` — acquires write lock
  - [ ] Remove all nodes listed in `files[path]`
  - [ ] Remove all adj/radj entries sourced from this file
  - [ ] Remove includes entry for this file
  - [ ] Delete `files[path]`
- [ ] Internal `removeFileUnsafe()` — same logic without lock (called from MergeFileGraph which already holds the lock)

### Notes
The "replace on re-merge" behavior is critical for incremental re-indexing: when a file changes, `MergeFileGraph` removes old data then adds new data in one atomic operation.

Edge entries in `adj`/`radj` that come from OTHER files pointing TO symbols in this file are NOT removed by `RemoveFile` — they are owned by their source file. This matches the design's edge ownership model.

## 4.3: FileSymbols and SymbolDetail

### Subtasks
- [ ] `FileSymbols(path string) []parser.Symbol` — acquires read lock, looks up `files[path]`, returns symbols
- [ ] `SymbolDetail(symbolID string) *parser.Symbol` — acquires read lock, looks up `nodes[symbolID]`
- [ ] Both return zero-value (empty slice / nil) for unknown keys
- [ ] Tests: add a file, query symbols, query by ID, query unknown file, query unknown ID

## 4.4: SearchSymbols

### Subtasks
- [ ] `SearchSymbols(pattern string, kind parser.SymbolKind) []parser.Symbol` — acquires read lock
- [ ] Try compiling pattern as regex; if it fails, treat as case-insensitive substring
- [ ] Iterate all nodes, match name against pattern
- [ ] If kind is non-empty, filter by kind
- [ ] Return matching symbols (capped at a reasonable limit, e.g., 100, to prevent huge responses)
- [ ] Tests: exact match, substring, regex, kind filter, cap limit, no matches

## 4.5: Callers and Callees (BFS traversal)

### Subtasks
- [ ] `Callers(symbolID string, depth int) []CallChain` — acquires read lock
  - [ ] BFS on `radj` starting from symbolID
  - [ ] Track visited set to prevent cycles
  - [ ] Stop at requested depth (depth <= 0 treated as 1)
  - [ ] Return results with (symbol ID, file, line, depth level)
- [ ] `Callees(symbolID string, depth int) []CallChain` — same but on `adj`
- [ ] Tests: linear chain, diamond, cycle, unknown symbol, depth=1 vs depth=2

### Notes
BFS (not DFS) gives us results ordered by distance, which is more useful for agents — nearest callers first.

## 4.6: FileDependencies

### Subtasks
- [ ] `FileDependencies(path string) []string` — acquires read lock, returns `includes[path]`
- [ ] Returns empty slice for unknown files
- [ ] Tests: file with includes, file without includes, unknown file

## 4.7: DetectCycles (Tarjan's SCC)

### Subtasks
- [ ] `internal/graph/algorithms.go` — Tarjan's strongly connected components algorithm
- [ ] Operates on the `includes` map (file-level directed graph)
- [ ] Returns `[][]string` — each inner slice is a cycle (set of file paths in the SCC)
- [ ] Only returns SCCs of size > 1 (excludes self-loops unless a file includes itself)
- [ ] Tests: acyclic graph (empty result), 2-node cycle, 3-node cycle, mixed cyclic/acyclic, self-include

### Notes
Tarjan's SCC is O(V + E) and well-suited for this. The algorithm is textbook — implement it directly rather than pulling in a graph library.

## 4.8: Orphans

### Subtasks
- [ ] `Orphans(kind parser.SymbolKind) []parser.Symbol` — acquires read lock
- [ ] Iterate all nodes; for each, check if `radj[symbolID]` has any `EdgeCalls` entries
- [ ] If zero incoming calls and kind matches (or kind is empty), include in result
- [ ] Default: only report callables (functions and methods), not types — unless kind filter explicitly requests a type kind
- [ ] Tests: graph with called and uncalled functions, kind filter, all-called graph (empty result)

## 4.9: ClassHierarchy

### Subtasks
- [ ] `ClassHierarchy(className string) *HierarchyNode` — acquires read lock
- [ ] Find the class node by name (search across all files)
- [ ] Walk `EdgeInherits` edges upward (radj) to find base classes
- [ ] Walk `EdgeInherits` edges downward (adj) to find derived classes
- [ ] Build a tree structure
- [ ] Tests: single inheritance (A -> B), multiple inheritance (A -> B, A -> C), deep chain (A -> B -> C), unknown class

## 4.10: Coupling

### Subtasks
- [ ] `Coupling(path string) map[string]int` — acquires read lock
- [ ] For each symbol in `files[path]`, count outgoing edges to symbols in other files
- [ ] Also count include edges from this file
- [ ] Return map of other-file-path → count
- [ ] Tests: file with cross-file calls, file with only local calls, unknown file

## 4.11: Concurrent access safety

### Subtasks
- [ ] `internal/graph/graph_test.go` — `TestConcurrentAccess`
- [ ] Pre-populate graph with a few files/symbols
- [ ] Spawn 10 reader goroutines: each calls Callers, SearchSymbols, FileSymbols in a tight loop (1000 iterations)
- [ ] Spawn 2 writer goroutines: each calls MergeFileGraph and RemoveFile in a loop (100 iterations)
- [ ] Use `sync.WaitGroup` to wait for all goroutines
- [ ] Test must pass under `go test -race`
- [ ] No assertions on specific results — the goal is to verify no data races, not correctness under contention

## 4.12: Structural verification

### Subtasks
- [ ] `go vet ./internal/graph/` passes
- [ ] `go test -race ./internal/graph/` passes with all tests
- [ ] `go vet ./...` passes (full project)

## Acceptance Criteria
- [ ] All graph methods from the design are implemented
- [ ] MergeFileGraph correctly adds/replaces file data
- [ ] RemoveFile cleanly removes all data for a file
- [ ] BFS traversal handles cycles without infinite loops
- [ ] Tarjan's SCC correctly detects file-level cycles
- [ ] Concurrent access test passes under -race
- [ ] `go test -race ./internal/graph/` — all pass
- [ ] `go vet ./...` clean
