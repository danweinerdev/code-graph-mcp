---
title: "Shared Foundation & Go Parser Core"
type: phase
plan: GoParser
phase: 1
status: planned
created: 2026-03-22
updated: 2026-03-22
deliverable: "KindInterface in types.go, updated ClassHierarchy, GoParser with all extraction methods"
tasks:
  - id: "1.1"
    title: "Add KindInterface to shared types"
    status: planned
    verification: "KindInterface = 'interface' added to types.go. ClassHierarchy in graph.go accepts KindInterface alongside KindClass/KindStruct. Existing tests still pass."
  - id: "1.2"
    title: "Add tree-sitter-go dependency"
    status: planned
    verification: "`go get github.com/tree-sitter/tree-sitter-go/bindings/go@latest` succeeds. go mod tidy clean. go build ./... compiles."
    depends_on: ["1.1"]
  - id: "1.3"
    title: "GoParser struct and query compilation"
    status: planned
    verification: "NewGoParser() succeeds. Extensions() returns [.go]. Close() releases queries without panic. Satisfies parser.Parser interface (compile-time check)."
    depends_on: ["1.2"]
  - id: "1.4"
    title: "Go query string definitions"
    status: planned
    verification: "All query categories compile against the pinned tree-sitter-go grammar: definitions (function, method, struct, interface), calls (direct, method/selector, package-qualified), imports."
    depends_on: ["1.3"]
  - id: "1.5"
    title: "Symbol definition extraction"
    status: planned
    verification: "Extracts: free functions, methods (with receiver type as Parent), structs, interfaces. Package name populates Namespace. Line/Column/EndLine correct. Signature populated and truncated."
    depends_on: ["1.4"]
  - id: "1.6"
    title: "Call site extraction"
    status: planned
    verification: "Extracts: direct calls (foo()), method calls (obj.Method()), package-qualified calls (pkg.Func()), goroutine calls (go foo()). Each edge has correct From (enclosing function) and To (callee name)."
    depends_on: ["1.4"]
  - id: "1.7"
    title: "Import extraction"
    status: planned
    verification: "Extracts: single imports (import \"fmt\"), grouped imports (import ( \"fmt\" ; \"os\" )), aliased imports (import f \"fmt\"). Edge kind is 'includes'. Import paths have quotes stripped."
    depends_on: ["1.4"]
  - id: "1.8"
    title: "Structural verification"
    status: planned
    verification: "`go vet ./...` passes. `go test -race ./internal/lang/goparser/` passes. All existing C++ tests still pass."
    depends_on: ["1.5", "1.6", "1.7"]
---

# Phase 1: Shared Foundation & Go Parser Core

## Overview

Add `KindInterface` to shared types, update the graph engine, and implement the Go parser with all extraction methods.

## 1.1: Add KindInterface to shared types

### Subtasks
- [ ] Add `KindInterface SymbolKind = "interface"` to `internal/parser/types.go`
- [ ] Update `ClassHierarchy` in `internal/graph/graph.go` to accept `KindInterface`
- [ ] Verify all existing tests still pass

## 1.2: Add tree-sitter-go dependency

### Subtasks
- [ ] `go get github.com/tree-sitter/tree-sitter-go/bindings/go@latest`
- [ ] `go mod tidy`
- [ ] Verify `go build ./...`

## 1.3: GoParser struct and query compilation

### Subtasks
- [ ] `internal/lang/goparser/goparser.go` — GoParser struct with language, defQuery, callQuery, importQuery
- [ ] `NewGoParser() (*GoParser, error)` — compile all queries
- [ ] `Extensions() []string` — return `[".go"]`
- [ ] `Close()` — release all query objects
- [ ] `var _ parser.Parser = (*GoParser)(nil)` compile-time check

### Notes
Package name is `goparser` (not `go`) to avoid conflict with the Go keyword.

## 1.4: Go query string definitions

### Subtasks
- [ ] `internal/lang/goparser/queries.go` — query string constants
- [ ] `definitionQueries` — function_declaration, method_declaration, struct type_spec, interface type_spec
- [ ] `callQueries` — call_expression with identifier, selector_expression
- [ ] `importQueries` — import_spec with interpreted_string_literal

## 1.5: Symbol definition extraction

### Subtasks
- [ ] `extractDefinitions` — iterate defQuery matches
- [ ] Free functions: `function_declaration > name: (identifier)`
- [ ] Methods: `method_declaration > name: (field_identifier)` — extract receiver type from `receiver` parameter_list to set Parent
- [ ] Structs: `type_spec > struct_type` with `name: (type_identifier)`
- [ ] Interfaces: `type_spec > interface_type` with `name: (type_identifier)` → Kind=interface
- [ ] Package name from `package_clause > (package_identifier)` → Namespace
- [ ] Line/Column/EndLine from node positions (0-based → 1-based)

### Notes
Receiver extraction: `method_declaration` has `receiver: (parameter_list (parameter_declaration type: ...))`. The type may be `(pointer_type (type_identifier))` for pointer receivers or just `(type_identifier)` for value receivers.

## 1.6: Call site extraction

### Subtasks
- [ ] `extractCalls` — iterate callQuery matches
- [ ] Direct calls: `call_expression > function: (identifier)`
- [ ] Method/selector calls: `call_expression > function: (selector_expression field: (field_identifier))`
- [ ] Enclosing function resolved via `findEnclosingKind(node, "function_declaration")` or `"method_declaration"`
- [ ] `go` statements: the child of `go_statement` is a `call_expression` — already captured by call query

## 1.7: Import extraction

### Subtasks
- [ ] `extractImports` — iterate importQuery matches
- [ ] Single: `import "fmt"` → Edge(From=file, To="fmt", Kind=includes)
- [ ] Grouped: `import ( "fmt" ; "os" )` → two edges
- [ ] Strip quotes from import path string
- [ ] Aliased imports: `import f "fmt"` — capture the path, not the alias

## 1.8: Structural verification

### Subtasks
- [ ] `go vet ./...` passes
- [ ] `go test -race ./internal/lang/goparser/` passes
- [ ] `go test -race ./...` passes (all existing tests)

## Acceptance Criteria
- [ ] KindInterface added and ClassHierarchy updated
- [ ] GoParser implements Parser interface
- [ ] All Go extraction patterns working
- [ ] All existing C++ tests unaffected
