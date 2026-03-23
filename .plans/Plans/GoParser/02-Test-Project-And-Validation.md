---
title: "Go Test Project & Validation"
type: phase
plan: GoParser
phase: 2
status: planned
created: 2026-03-22
updated: 2026-03-22
deliverable: "Comprehensive Go test project, unit test corpus, CLI validation"
tasks:
  - id: "2.1"
    title: "Create testdata/go/ project"
    status: planned
    verification: "Multi-file Go project with known structure covering: packages, structs, interfaces, methods with pointer/value receivers, goroutines, channels, imports, init functions, closures, type aliases, generics (Go 1.18+), embedded structs. MANIFEST.md documents expected symbols and edges."
  - id: "2.2"
    title: "Unit test corpus for parser"
    status: planned
    verification: "Every query pattern has at least one test. Tests cover all of the following individually:"
    depends_on: ["2.1"]
  - id: "2.3"
    title: "Definition tests"
    status: planned
    verification: "Tests for: free function, method with value receiver, method with pointer receiver, struct definition, interface definition, type alias, generic function (Go 1.18+), init() function, main() function, unexported function. Each verifies Name, Kind, Parent (for methods), Namespace (package)."
    depends_on: ["2.2"]
  - id: "2.4"
    title: "Call site tests"
    status: planned
    verification: "Tests for: direct call (foo()), method call (obj.Method()), package-qualified call (fmt.Println()), goroutine (go foo()), defer call (defer f.Close()), chained call (a.B().C()), call inside closure. Each verifies From (enclosing function) and To (callee name)."
    depends_on: ["2.2"]
  - id: "2.5"
    title: "Import tests"
    status: planned
    verification: "Tests for: single import, grouped import (multiple paths), aliased import (import f \"fmt\"), dot import (import . \"pkg\"), blank import (import _ \"pkg\"). Each verifies edge Kind=includes and path string is correctly stripped of quotes."
    depends_on: ["2.2"]
  - id: "2.6"
    title: "Edge case tests"
    status: planned
    verification: "Tests for: empty file, file with only package clause, multiple functions same name different files, method on embedded struct, interface with embedded interface, anonymous struct field, blank identifier function. Parser does not crash on any input."
    depends_on: ["2.2"]
  - id: "2.7"
    title: "CLI validation against testdata"
    status: planned
    verification: "parse-test testdata/go/ runs without crashes. Output matches MANIFEST.md expected symbols and edges. Spot-check 20+ symbols for correctness."
    depends_on: ["2.3", "2.4", "2.5", "2.6"]
  - id: "2.8"
    title: "CLI validation against real Go project"
    status: planned
    verification: "parse-test against a real open-source Go project (e.g., this project itself or a small Go library). No crashes, 0 warnings. Spot-check 20+ symbols for accuracy."
    depends_on: ["2.7"]
  - id: "2.9"
    title: "Structural verification"
    status: planned
    verification: "`go vet ./...` passes. `go test -race ./...` passes including all new Go parser tests."
    depends_on: ["2.8"]
---

# Phase 2: Go Test Project & Validation

## Overview

Comprehensive testing of the Go parser against both synthetic test cases and real-world Go code.

## 2.1: Create testdata/go/ project

### Subtasks
- [ ] `testdata/go/main.go` — main package with main(), calls to other packages
- [ ] `testdata/go/server/server.go` — Server struct with methods, interface implementation
- [ ] `testdata/go/server/handler.go` — HTTP handler functions, closures
- [ ] `testdata/go/models/user.go` — Structs, interfaces, embedded types
- [ ] `testdata/go/models/repo.go` — Repository interface, generic functions (if grammar supports)
- [ ] `testdata/go/utils/helpers.go` — Free functions, type aliases, init()
- [ ] `testdata/go/MANIFEST.md` — Expected symbols, edges, relationships

### Notes
Structure as a realistic multi-package Go project. Include:
- Struct with exported/unexported methods
- Interface definition and structural implementation
- Pointer and value receivers
- Goroutines and defer
- Multiple import styles
- init() function
- Closures/anonymous functions assigned to variables

## 2.2–2.6: Unit test corpus

Test file: `internal/lang/goparser/goparser_test.go`

### 2.3: Definition tests
- [ ] `func foo() {}` → KindFunction, Name="foo"
- [ ] `func (s *Server) Start() {}` → KindMethod, Name="Start", Parent="Server"
- [ ] `func (s Server) Name() string {}` → KindMethod, Parent="Server" (value receiver)
- [ ] `type Server struct { ... }` → KindStruct, Name="Server"
- [ ] `type Handler interface { ... }` → KindInterface, Name="Handler"
- [ ] `type ID = string` → KindTypedef, Name="ID"
- [ ] `func Map[T any, U any](s []T, f func(T) U) []U {}` → KindFunction (generic)
- [ ] `func init() {}` → KindFunction, Name="init"
- [ ] `func main() {}` → KindFunction, Name="main"
- [ ] `func helper() {}` (unexported) → still extracted

### 2.4: Call site tests
- [ ] `func f() { foo() }` → edge to "foo"
- [ ] `func f() { s.Start() }` → edge to "Start"
- [ ] `func f() { fmt.Println() }` → edge to "Println"
- [ ] `func f() { go handler() }` → edge to "handler"
- [ ] `func f() { defer conn.Close() }` → edge to "Close"
- [ ] `func f() { a.B().C() }` → edges to "B" and "C"
- [ ] `func f() { fn := func() { bar() }; fn() }` → edges to "bar" and "fn"

### 2.5: Import tests
- [ ] `import "fmt"` → edge To="fmt"
- [ ] `import ( "fmt" ; "os" )` → two edges
- [ ] `import f "fmt"` → edge To="fmt" (path, not alias)
- [ ] `import . "testing"` → edge To="testing"
- [ ] `import _ "image/png"` → edge To="image/png"

### 2.6: Edge case tests
- [ ] Empty file with only `package main` → no symbols, no crash
- [ ] Method on embedded struct type
- [ ] Interface embedding another interface
- [ ] File with syntax errors → parser skips error nodes gracefully

## 2.7: CLI validation against testdata

### Subtasks
- [ ] `go run ./cmd/parse-test testdata/go/`
- [ ] Compare output against MANIFEST.md
- [ ] All expected symbols present with correct kinds

## 2.8: CLI validation against real Go project

### Subtasks
- [ ] Run against this project itself: `go run ./cmd/parse-test ./internal/`
- [ ] No crashes, 0 warnings
- [ ] Spot-check: verify parser.Parser interface is found, graph.Graph struct, tool handlers

## Acceptance Criteria
- [ ] testdata/go/ project with MANIFEST
- [ ] All definition, call, import, and edge case tests pass
- [ ] CLI validation clean on testdata and real project
- [ ] `go test -race ./...` passes
