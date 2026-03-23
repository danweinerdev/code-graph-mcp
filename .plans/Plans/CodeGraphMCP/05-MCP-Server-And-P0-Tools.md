---
title: "MCP Server & P0 Tools"
type: phase
plan: CodeGraphMCP
phase: 5
status: complete
created: 2026-03-22
updated: 2026-03-22
deliverable: "Working MCP server with analyze_codebase and all P0 query tools"
tasks:
  - id: "5.1"
    title: "Entry point and server setup"
    status: complete
    verification: "main.go follows lldb-debug-mcp pattern. Binary builds and serves stdio MCP."
  - id: "5.2"
    title: "Tools struct, Register, and state guards"
    status: complete
    verification: "11 tools registered (7 P0 + 4 P1). requireIndexed guard works. indexMu prevents concurrent analyze."
    depends_on: ["5.1"]
  - id: "5.3"
    title: "analyze_codebase handler with worker pool and name resolution"
    status: complete
    verification: "Worker pool parses files concurrently. Include paths resolved via basename matching. Call edges resolved via scope-aware heuristics. Returns JSON summary."
    depends_on: ["5.2"]
  - id: "5.4"
    title: "get_file_symbols handler"
    status: complete
    verification: "Returns symbols with IDs. Error for unknown file."
    depends_on: ["5.2"]
  - id: "5.5"
    title: "get_callers and get_callees handlers"
    status: complete
    verification: "Returns resolved call chains. Did-you-mean for unknown symbols. main() -> Engine::update, Engine::render, clamp, Engine::status verified."
    depends_on: ["5.2"]
  - id: "5.6"
    title: "get_dependencies handler"
    status: complete
    verification: "Returns absolute resolved include paths. engine.cpp -> [engine.h, utils.h] verified."
    depends_on: ["5.2"]
  - id: "5.7"
    title: "search_symbols handler"
    status: complete
    verification: "Supports query + kind filter. Empty query with kind lists all of that type."
    depends_on: ["5.2"]
  - id: "5.8"
    title: "get_symbol_detail handler"
    status: complete
    verification: "Returns full symbol info. Did-you-mean for unknown symbols."
    depends_on: ["5.2"]
  - id: "5.9"
    title: "Integration tests"
    status: complete
    verification: "10 tests covering all P0 handlers + guards + error cases. All pass under -race."
    depends_on: ["5.3", "5.4", "5.5", "5.6", "5.7", "5.8"]
  - id: "5.10"
    title: "Structural verification"
    status: complete
    verification: "go vet clean, go test -race passes, make build produces binary."
    depends_on: ["5.9"]
---

# Phase 5: MCP Server & P0 Tools

## Overview

Wired parser + graph engine into a working MCP server with name resolution layer.

## Key Results

- **Name resolution works:** bare callee names (`update`) resolve to symbol IDs (`/path/engine.cpp:Engine::update`)
- **Include resolution works:** raw include paths (`engine.h`) resolve to absolute paths
- **11 tools registered:** 7 P0 + 4 P1 (structural analysis handlers implemented ahead of schedule)
- **10 integration tests** covering analyze, all query tools, guards, error handling

## Acceptance Criteria
- [x] MCP server starts and serves tools over stdio
- [x] analyze_codebase indexes a directory with concurrent parsing
- [x] Include paths resolved to absolute where possible
- [x] Call edges resolved to symbol IDs via scope-aware heuristics
- [x] All P0 query tools return correct JSON responses
- [x] Guards prevent queries before indexing and concurrent indexing
- [x] Integration tests pass
- [x] `go test -race ./...` passes
- [x] `go vet ./...` clean
