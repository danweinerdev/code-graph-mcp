---
title: "Integration & Registration"
type: phase
plan: GoParser
phase: 3
status: planned
created: 2026-03-22
updated: 2026-03-22
deliverable: "GoParser registered in MCP server, integration tests via tools, docs updated"
tasks:
  - id: "3.1"
    title: "Register GoParser in main.go"
    status: planned
    verification: "GoParser created and registered alongside CppParser in main.go. Binary compiles and starts. Tool list shows all tools. analyze_codebase with a Go directory indexes .go files."
  - id: "3.2"
    title: "MCP tool integration tests for Go"
    status: planned
    verification: "Tests call analyze_codebase on testdata/go/ then verify: get_file_symbols returns Go symbols, search_symbols finds structs/interfaces, get_callers/get_callees return resolved Go call chains, get_dependencies returns Go import paths, get_orphans finds uncalled Go functions, generate_mermaid produces diagrams for Go symbols."
    depends_on: ["3.1"]
  - id: "3.3"
    title: "Mixed-language indexing test"
    status: planned
    verification: "analyze_codebase on a directory containing both .cpp and .go files indexes both. search_symbols finds symbols from both languages. get_dependencies shows C++ includes and Go imports."
    depends_on: ["3.2"]
  - id: "3.4"
    title: "Update README and CLAUDE.md"
    status: planned
    verification: "README lists Go in supported languages with .go extension. CLAUDE.md lists Go-specific patterns and limitations. Tool count updated."
    depends_on: ["3.2"]
  - id: "3.5"
    title: "Structural verification"
    status: planned
    verification: "`go vet ./...` passes. `go test -race ./...` passes. `make build` produces working binary."
    depends_on: ["3.2", "3.3", "3.4"]
---

# Phase 3: Integration & Registration

## Overview

Register the Go parser in the MCP server, test via MCP tool handlers, verify mixed-language support, update docs.

## 3.1: Register GoParser in main.go

### Subtasks
- [ ] Import `goparser` package in main.go
- [ ] `goParser, err := goparser.NewGoParser()` after CppParser
- [ ] `defer goParser.Close()`
- [ ] `reg.Register(goParser)`
- [ ] Verify `make build` and binary starts

## 3.2: MCP tool integration tests for Go

### Subtasks
- [ ] `internal/tools/go_projects_test.go`
- [ ] Test analyze_codebase on testdata/go/
- [ ] Test get_file_symbols for a Go file
- [ ] Test search_symbols for Go structs, interfaces, methods
- [ ] Test get_callers / get_callees for Go functions
- [ ] Test get_dependencies returns Go import paths
- [ ] Test get_orphans finds uncalled Go functions
- [ ] Test generate_mermaid for a Go symbol

## 3.3: Mixed-language indexing test

### Subtasks
- [ ] Create a temp directory with both .cpp and .go files
- [ ] analyze_codebase on it
- [ ] Verify symbols from both languages are in the graph
- [ ] Verify file counts include both types

## 3.4: Update docs

### Subtasks
- [ ] README: add Go to supported languages table
- [ ] CLAUDE.md: add Go parser patterns and any limitations

## Acceptance Criteria
- [ ] GoParser registered and working in the MCP server
- [ ] All MCP tool integration tests pass for Go
- [ ] Mixed C++/Go indexing works
- [ ] Docs updated
- [ ] `go test -race ./...` passes
