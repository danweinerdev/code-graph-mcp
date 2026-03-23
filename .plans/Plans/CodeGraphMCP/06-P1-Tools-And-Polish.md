---
title: "P1 Tools & Polish"
type: phase
plan: CodeGraphMCP
phase: 6
status: planned
created: 2026-03-22
updated: 2026-03-22
deliverable: "P1 structural analysis tools, error polish, README"
tasks:
  - id: "6.1"
    title: "detect_cycles tool"
    status: planned
    verification: "Returns JSON array of cycle chains from graph's DetectCycles(). Each cycle is an array of absolute file paths. Returns empty array if no cycles. Tested with testdata circular includes."
  - id: "6.2"
    title: "get_orphans tool"
    status: planned
    verification: "Returns JSON array of symbols with no incoming call edges. Optional `kind` parameter filters by symbol kind (function, method, class, etc.). Default returns only callables. Tested with testdata — neverCalled and alsoOrphaned appear."
    depends_on: ["6.1"]
  - id: "6.3"
    title: "get_class_hierarchy tool"
    status: planned
    verification: "Accepts `class` parameter. Returns JSON tree with name, bases (upward), derived (downward). Returns error for unknown class with did-you-mean suggestion. Tested with DebugEngine -> Engine."
    depends_on: ["6.1"]
  - id: "6.4"
    title: "get_coupling tool"
    status: planned
    verification: "Accepts `file` parameter. Returns JSON map of other file paths to cross-file edge counts (calls + includes). Returns error for unknown file. Tested with multi-file testdata."
    depends_on: ["6.1"]
  - id: "6.5"
    title: "Did-you-mean suggestions on symbol not found"
    status: planned
    verification: "When get_callers, get_callees, get_symbol_detail, or get_class_hierarchy receive an unknown symbol/class, the error message includes up to 5 closest matches from SearchSymbols. Tested with a misspelled symbol name."
    depends_on: ["6.1"]
  - id: "6.6"
    title: "Register P1 tools"
    status: planned
    verification: "All 4 P1 tools appear in the MCP tool list with correct parameter schemas and descriptions. Tested by inspecting Register() output."
    depends_on: ["6.1", "6.2", "6.3", "6.4"]
  - id: "6.7"
    title: "README and final CLAUDE.md update"
    status: planned
    verification: "README.md covers: project description, installation, configuration (MCP client setup), available tools with parameter docs, build instructions, limitations. CLAUDE.md updated with final tool list and conventions."
    depends_on: ["6.6"]
  - id: "6.8"
    title: "Structural verification"
    status: planned
    verification: "`go vet ./...` passes; `go test -race ./...` passes; `make build` produces working binary; binary serves all P0 + P1 tools over stdio"
    depends_on: ["6.5", "6.6", "6.7"]
---

# Phase 6: P1 Tools & Polish

## Overview

Add the P1 structural analysis tools (cycles, orphans, class hierarchy, coupling), polish error handling with did-you-mean suggestions, and write user-facing documentation.

## 6.1: detect_cycles tool

### Subtasks
- [ ] `internal/tools/structure.go` — `handleDetectCycles`
- [ ] Guard: `requireIndexed()`
- [ ] Call `g.DetectCycles()`
- [ ] Marshal cycle arrays to JSON, return
- [ ] No parameters needed

## 6.2: get_orphans tool

### Subtasks
- [ ] `internal/tools/structure.go` — `handleGetOrphans`
- [ ] Guard: `requireIndexed()`
- [ ] Extract `kind` (optional string) param
- [ ] Call `g.Orphans(parser.SymbolKind(kind))`
- [ ] Include symbol IDs in response
- [ ] Marshal to JSON array, return

## 6.3: get_class_hierarchy tool

### Subtasks
- [ ] `internal/tools/structure.go` — `handleGetClassHierarchy`
- [ ] Guard: `requireIndexed()`
- [ ] Extract `class` (required string) param
- [ ] Call `g.ClassHierarchy(class)`
- [ ] If nil, return error with did-you-mean from class search
- [ ] Marshal HierarchyNode tree to JSON, return

## 6.4: get_coupling tool

### Subtasks
- [ ] `internal/tools/structure.go` — `handleGetCoupling`
- [ ] Guard: `requireIndexed()`
- [ ] Extract `file` (required string) param
- [ ] Call `g.Coupling(file)`
- [ ] Marshal map to JSON, return

## 6.5: Did-you-mean suggestions

### Subtasks
- [ ] Helper function `suggestSymbols(g *graph.Graph, name string, limit int) []string`
- [ ] Uses `g.SearchSymbols(name, "")` to find partial matches
- [ ] Returns up to `limit` symbol IDs as suggestions
- [ ] Integrate into error paths: get_callers, get_callees, get_symbol_detail, get_class_hierarchy
- [ ] Error format: `"symbol not found: 'foo'. Did you mean: /a.cpp:fooBar, /b.cpp:foo_init?"`
- [ ] Test with a misspelled name

## 6.6: Register P1 tools

### Subtasks
- [ ] Add `detect_cycles`, `get_orphans`, `get_class_hierarchy`, `get_coupling` to `Register()`
- [ ] Each with `mcp.WithDescription(...)` and parameter schemas
- [ ] Verify all tools appear in server tool list

## 6.7: README and final CLAUDE.md update

### Subtasks
- [ ] `README.md` — project overview, installation, MCP client configuration example (Claude Desktop, Cursor), tool reference table, build instructions, limitations link
- [ ] Update `CLAUDE.md` — add full tool list, update architecture section with Phase 5 additions

## 6.8: Structural verification

### Subtasks
- [ ] `go vet ./...` passes
- [ ] `go test -race ./...` passes
- [ ] `make build` produces working binary
- [ ] Manual smoke test: start binary, connect MCP client, analyze a directory, query tools

## Acceptance Criteria
- [ ] All 4 P1 tools implemented and return correct JSON
- [ ] Error messages include helpful did-you-mean suggestions
- [ ] README documents all tools with parameters
- [ ] CLAUDE.md is complete and accurate
- [ ] `go test -race ./...` passes
- [ ] `go vet ./...` clean
- [ ] Binary serves all 11 tools (7 P0 + 4 P1) over stdio
