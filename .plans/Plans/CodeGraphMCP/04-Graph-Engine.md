---
title: "Graph Engine"
type: phase
plan: CodeGraphMCP
phase: 4
status: complete
created: 2026-03-22
updated: 2026-03-22
deliverable: "In-memory directed graph with add/remove/query methods, BFS traversal, cycle detection, orphan detection, and concurrent access safety"
tasks:
  - id: "4.1"
    title: "Graph struct and core types"
    status: complete
    verification: "Graph struct compiles with all fields. New() initializes all maps. Node, EdgeEntry, CallChain, HierarchyNode types defined."
  - id: "4.2"
    title: "MergeFileGraph and RemoveFile"
    status: complete
    verification: "Merge adds nodes/edges, idempotent on re-merge, RemoveFile cleans up. Tested with merge one/two files, re-merge, remove."
    depends_on: ["4.1"]
  - id: "4.3"
    title: "FileSymbols and SymbolDetail"
    status: complete
    verification: "FileSymbols returns symbols for known files, empty for unknown. SymbolDetail returns symbol by ID, nil for unknown."
    depends_on: ["4.2"]
  - id: "4.4"
    title: "SearchSymbols"
    status: complete
    verification: "Case-insensitive regex matching, substring fallback, kind filter, 100 result cap. Tested with exact, substring, regex, kind filter, no match."
    depends_on: ["4.2"]
  - id: "4.5"
    title: "Callers and Callees (BFS traversal)"
    status: complete
    verification: "BFS with visited set. Tested: linear chain, diamond, cycle (no infinite loop), unknown symbol, depth=0."
    depends_on: ["4.2"]
  - id: "4.6"
    title: "FileDependencies"
    status: complete
    verification: "Returns included files, empty for unknown. Tested."
    depends_on: ["4.2"]
  - id: "4.7"
    title: "DetectCycles (Tarjan's SCC)"
    status: complete
    verification: "Tarjan's SCC on include graph. Tested: acyclic, 2-node, 3-node, mixed cyclic/acyclic."
    depends_on: ["4.2"]
  - id: "4.8"
    title: "Orphans"
    status: complete
    verification: "Returns callables with no incoming call edges. Classes excluded by default. Kind filter works. Tested."
    depends_on: ["4.2"]
  - id: "4.9"
    title: "ClassHierarchy"
    status: complete
    verification: "Returns hierarchy tree with bases and derived. Tested: single, multiple, unknown class."
    depends_on: ["4.2"]
  - id: "4.10"
    title: "Coupling"
    status: complete
    verification: "Returns cross-file edge counts. Include edges counted. Tested with multi-file graph."
    depends_on: ["4.2"]
  - id: "4.11"
    title: "Concurrent access safety"
    status: complete
    verification: "10 reader + 2 writer goroutines under -race. No data races detected."
    depends_on: ["4.5", "4.4", "4.3", "4.2"]
  - id: "4.12"
    title: "Structural verification"
    status: complete
    verification: "go vet ./... passes; go test -race ./internal/graph/ passes (28 tests)"
    depends_on: ["4.11"]
---

# Phase 4: Graph Engine

## Overview

In-memory directed graph in `internal/graph/` with all query methods needed by MCP tool handlers.

## Results

- 28 tests, all passing under `-race`
- Tarjan's SCC for cycle detection
- BFS with visited set for callers/callees (handles cycles)
- Concurrent access: 10 readers + 2 writers, no data races
- `go vet` clean

## Acceptance Criteria
- [x] All graph methods from the design are implemented
- [x] MergeFileGraph correctly adds/replaces file data
- [x] RemoveFile cleanly removes all data for a file
- [x] BFS traversal handles cycles without infinite loops
- [x] Tarjan's SCC correctly detects file-level cycles
- [x] Concurrent access test passes under -race
- [x] `go test -race ./internal/graph/` — 28 tests pass
- [x] `go vet ./...` clean
