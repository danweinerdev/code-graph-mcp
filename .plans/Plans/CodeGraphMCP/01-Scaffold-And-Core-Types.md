---
title: "Scaffold & Core Types"
type: phase
plan: CodeGraphMCP
phase: 1
status: complete
created: 2026-03-22
updated: 2026-03-22
deliverable: "Buildable Go module with parser interface, core types, registry, and Makefile"
tasks:
  - id: "1.1"
    title: "Initialize Go module and dependencies"
    status: complete
    verification: "`go mod tidy` succeeds; `go build ./...` compiles with CGO_ENABLED=1; go-tree-sitter and tree-sitter-cpp imports resolve"
  - id: "1.2"
    title: "Create directory structure"
    status: complete
    verification: "All directories from the design exist: cmd/code-graph-mcp/, internal/tools/, internal/graph/, internal/parser/, internal/lang/cpp/"
    depends_on: ["1.1"]
  - id: "1.3"
    title: "Define parser types (Symbol, Edge, FileGraph, SymbolKind, EdgeKind)"
    status: complete
    verification: "Types compile; SymbolKind and EdgeKind are string constants that serialize to readable JSON; FileGraph contains Path, Symbols, Edges fields"
    depends_on: ["1.2"]
  - id: "1.4"
    title: "Define Parser interface"
    status: complete
    verification: "Interface has Extensions(), ParseFile(path, content), and Close() methods; a mock implementation can satisfy the interface in tests"
    depends_on: ["1.3"]
  - id: "1.5"
    title: "Implement parser Registry"
    status: complete
    verification: "Register() maps extensions to parsers; ForFile() returns the correct parser for a given file path; ForFile() returns nil for unregistered extensions; duplicate extension registration is rejected"
    depends_on: ["1.4"]
  - id: "1.6"
    title: "Create Makefile"
    status: complete
    verification: "`make build` produces bin/code-graph-mcp; `make test` runs `go test -race ./...` with CGO_ENABLED=1; `make vet` runs `go vet ./...`"
    depends_on: ["1.1"]
  - id: "1.7"
    title: "Structural verification"
    status: complete
    verification: "`go vet ./...` passes; `go test -race ./...` passes; no compilation warnings"
    depends_on: ["1.3", "1.4", "1.5", "1.6"]
---

# Phase 1: Scaffold & Core Types

## Overview

Set up the Go module, directory structure, dependencies, and core type definitions. This phase produces a buildable (but non-functional) project with the contracts that all subsequent phases implement against.

No business logic in this phase — just the skeleton and type system.

## 1.1: Initialize Go module and dependencies

### Subtasks
- [x] `go mod init github.com/danweinerdev/code-graph-mcp`
- [x] `go get github.com/mark3labs/mcp-go@latest`
- [x] `go get github.com/tree-sitter/go-tree-sitter@latest`
- [x] `go get github.com/tree-sitter/tree-sitter-cpp/bindings/go@latest`
- [x] Verify `go mod tidy` and `go build ./...` succeed with `CGO_ENABLED=1`

### Notes
Pinned versions: mcp-go v0.45.0, go-tree-sitter v0.25.0, tree-sitter-cpp v0.23.4.

## 1.2: Create directory structure

### Subtasks
- [x] `cmd/code-graph-mcp/main.go` — minimal `func main()` placeholder
- [x] `internal/parser/` — will hold interface and registry
- [x] `internal/graph/` — will hold graph engine
- [x] `internal/tools/` — will hold MCP tool handlers
- [x] `internal/lang/cpp/` — will hold C++ parser

## 1.3: Define parser types

### Subtasks
- [x] `internal/parser/types.go` — `Symbol`, `SymbolKind`, `Edge`, `EdgeKind`, `FileGraph`
- [x] `SymbolKind` as `string` type with constants: `function`, `method`, `class`, `struct`, `enum`, `typedef`
- [x] `EdgeKind` as `string` type with constants: `calls`, `includes`, `inherits`
- [x] `Symbol` fields: Name, Kind, File, Line, Column, EndLine, Signature, Namespace, Parent
- [x] `Edge` fields: From, To, Kind, File, Line
- [x] `FileGraph` fields: Path, Symbols, Edges

## 1.4: Define Parser interface

### Subtasks
- [x] `internal/parser/parser.go` — `Parser` interface with `Extensions()`, `ParseFile()`, `Close()`
- [x] Write a trivial mock parser in `internal/parser/parser_test.go` that returns hardcoded symbols to confirm the interface compiles and is usable

## 1.5: Implement parser Registry

### Subtasks
- [x] `internal/parser/registry.go` — `Registry` struct with `Register(Parser)` and `ForFile(path) Parser`
- [x] `Register` maps each extension from `parser.Extensions()` to the parser instance
- [x] `Register` returns an error if an extension is already registered
- [x] `ForFile` extracts the extension from the path and looks up the parser
- [x] `ForFile` returns `nil` for unregistered extensions (no error — just skip unknown files)
- [x] Unit tests: register a mock parser for `.cpp`, verify `ForFile("foo.cpp")` returns it, `ForFile("foo.py")` returns nil, duplicate registration returns error

## 1.6: Create Makefile

### Subtasks
- [x] `build` target: `CGO_ENABLED=1 go build -o bin/code-graph-mcp ./cmd/code-graph-mcp`
- [x] `test` target: `CGO_ENABLED=1 go test -race ./...`
- [x] `test-integration` target: `CGO_ENABLED=1 go test -tags integration -race ./internal/tools/ -v`
- [x] `vet` target: `go vet ./...`
- [x] `clean` target: `rm -rf bin/`

## 1.7: Structural verification

### Subtasks
- [x] Run `go vet ./...` — must pass clean
- [x] Run `go test -race ./...` — all tests pass
- [x] Run `go build ./...` — compiles without warnings

## Acceptance Criteria
- [x] `make build` produces a binary (even if it does nothing)
- [x] `make test` runs registry tests, all pass
- [x] `make vet` clean
- [x] All types from the design document are defined and compile
- [x] Parser interface is satisfiable by a mock
