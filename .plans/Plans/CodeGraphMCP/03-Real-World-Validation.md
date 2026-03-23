---
title: "Real-World Validation"
type: phase
plan: CodeGraphMCP
phase: 3
status: planned
created: 2026-03-22
updated: 2026-03-22
deliverable: "Parser validated against real C++ code; CLI test harness; known limitations documented"
tasks:
  - id: "3.1"
    title: "Create testdata C++ project"
    status: planned
    verification: "testdata/cpp/ contains a multi-file C++ project with known structure: main.cpp calling functions from engine.cpp and utils.cpp; engine.h/cpp with a class; utils.h/cpp with free functions; circular_a.h/circular_b.h with circular includes; orphan.cpp with an uncalled function. A MANIFEST.md in testdata/cpp/ documents the expected symbols, edges, and relationships."
  - id: "3.2"
    title: "Build CLI test harness"
    status: planned
    verification: "Running `go run ./cmd/parse-test <directory>` walks the directory, parses all C++ files, and prints a structured report: file list, symbols per file (name, kind, line), edges (from, to, kind), warnings/errors. Output is human-readable and suitable for manual inspection. Exits 0 on success."
    depends_on: ["3.1"]
  - id: "3.3"
    title: "Validate against testdata project"
    status: planned
    verification: "CLI harness output for testdata/cpp/ matches the expected MANIFEST.md. All expected symbols found, all expected edges present, no false positives in a manually audited sample."
    depends_on: ["3.2"]
  - id: "3.4"
    title: "Validate against a real open-source C++ project"
    status: planned
    verification: "Parser runs against a non-trivial open-source C++ codebase (e.g., a small-to-medium project like json.hpp, fmt, or a user-chosen project) without crashing. Report is generated. Manual spot-check of 20+ symbols confirms accuracy (correct names, kinds, lines). False positive/negative rate for call edges is documented."
    depends_on: ["3.2"]
  - id: "3.5"
    title: "Fix query patterns from findings"
    status: planned
    verification: "Any query pattern failures discovered in 3.3 or 3.4 are fixed. Corresponding unit tests added to cpp_test.go for each fix. All existing tests still pass."
    depends_on: ["3.3", "3.4"]
  - id: "3.6"
    title: "Document known limitations"
    status: planned
    verification: "A LIMITATIONS.md or section in CLAUDE.md documents: (1) C++ patterns the parser does not handle (e.g., macro-generated function definitions, complex template metaprogramming); (2) call resolution is best-effort name matching, not semantic; (3) any grammar-version-specific caveats discovered during validation."
    depends_on: ["3.5"]
  - id: "3.7"
    title: "Structural verification"
    status: planned
    verification: "`go vet ./...` passes; `go test -race ./...` passes including all new and fixed tests"
    depends_on: ["3.5"]
---

# Phase 3: Real-World Validation

## Overview

This is the **validation gate**. The parser from Phase 2 is exercised against real C++ code to confirm accuracy before the graph engine or MCP server depend on it. A CLI test harness provides human-readable output for manual inspection.

No MCP work starts until this phase confirms parser accuracy.

## 3.1: Create testdata C++ project

### Subtasks
- [ ] `testdata/cpp/main.cpp` — includes engine.h and utils.h; main() calls Engine::update() and utility functions
- [ ] `testdata/cpp/engine.h` — Engine class declaration with update(), render() methods
- [ ] `testdata/cpp/engine.cpp` — Engine method implementations; calls utility functions from utils.h
- [ ] `testdata/cpp/utils.h` — free function declarations: clamp(), lerp(), formatString()
- [ ] `testdata/cpp/utils.cpp` — free function definitions
- [ ] `testdata/cpp/circular_a.h` — includes circular_b.h, declares ClassA
- [ ] `testdata/cpp/circular_b.h` — includes circular_a.h, declares ClassB
- [ ] `testdata/cpp/orphan.cpp` — contains neverCalled() function that nothing references
- [ ] `testdata/cpp/MANIFEST.md` — expected symbols (name, kind, file, line), expected edges (from, to, kind), expected relationships (inheritance, includes)

### Notes
The testdata project should exercise every query pattern from Phase 2: free functions, methods, classes, structs, includes (quoted and system), inheritance, all call patterns. Keep it small (< 200 lines total) but comprehensive.

## 3.2: Build CLI test harness

### Subtasks
- [ ] `cmd/parse-test/main.go` — standalone CLI tool (not part of the MCP server)
- [ ] Accept a directory path as argument
- [ ] Walk directory, filter by CppParser extensions
- [ ] Parse each file, collect FileGraph results
- [ ] Print structured report:
  ```
  === Files (N) ===
  - path/to/file.cpp

  === Symbols (N) ===
  [function] main (main.cpp:5)
  [method]   Engine::update (engine.cpp:12)
  [class]    Engine (engine.h:3)
  ...

  === Edges (N) ===
  [calls]    main.cpp:main -> Engine::update (line 8)
  [includes] main.cpp -> engine.h (line 1)
  [inherits] DerivedClass -> BaseClass (line 15)
  ...

  === Warnings (N) ===
  - engine.cpp:42: skipped error node
  ```
- [ ] Exit 0 on success, non-zero on fatal errors

### Notes
This tool is a development aid — it doesn't need to be polished, just functional for manual inspection. It can live in `cmd/parse-test/` and be excluded from the main build.

## 3.3: Validate against testdata project

### Subtasks
- [ ] Run `go run ./cmd/parse-test testdata/cpp/`
- [ ] Compare output against MANIFEST.md
- [ ] Verify: every expected symbol is present with correct kind and line
- [ ] Verify: every expected call edge is present
- [ ] Verify: include edges match expected includes
- [ ] Verify: inheritance edges match expected inheritance
- [ ] Verify: orphan.cpp's neverCalled() appears as a symbol but has no incoming call edges
- [ ] Verify: no false positive symbols from forward declarations

## 3.4: Validate against a real open-source C++ project

### Subtasks
- [ ] Choose a small-to-medium open-source C++ project (suggest: nlohmann/json single-header, fmtlib/fmt, or similar)
- [ ] Run `go run ./cmd/parse-test <project-dir>`
- [ ] Verify: no crashes or panics on any file
- [ ] Spot-check 20+ symbols: correct names, kinds, line numbers
- [ ] Spot-check 10+ call edges: plausible caller→callee relationships
- [ ] Document false positive rate (edges to wrong targets) and false negative rate (missed symbols/edges)
- [ ] Note any C++ patterns that break the parser

### Notes
The goal is not 100% accuracy — it's to confirm the parser is "good enough for navigation" and to discover query patterns that need fixing. A 90%+ symbol detection rate and 80%+ call edge accuracy is a reasonable target for tree-sitter-based syntactic analysis.

## 3.5: Fix query patterns from findings

### Subtasks
- [ ] For each failure discovered in 3.3 or 3.4, create a minimal C++ snippet that reproduces it
- [ ] Add the snippet as a test case in `cpp_test.go`
- [ ] Fix the query pattern or Go extraction logic
- [ ] Confirm the fix doesn't break existing tests

## 3.6: Document known limitations

### Subtasks
- [ ] Create a limitations section (in CLAUDE.md or a separate doc)
- [ ] Document: macro-generated definitions (e.g., `DEFINE_HANDLER(name)` that expands to a function)
- [ ] Document: complex template metaprogramming patterns that produce incomplete ASTs
- [ ] Document: call resolution is syntactic name matching, not semantic
- [ ] Document: anonymous namespace handling
- [ ] Document: any grammar-version-specific issues discovered
- [ ] Document: forward declarations are intentionally excluded

## 3.7: Structural verification

### Subtasks
- [ ] `go vet ./...` passes
- [ ] `go test -race ./...` — all tests pass including new regression tests from 3.5

## Acceptance Criteria
- [ ] CLI harness runs and produces readable output
- [ ] testdata project: all expected symbols and edges found
- [ ] Real-world project: no crashes, 90%+ symbol accuracy on spot-check
- [ ] All discovered issues either fixed or documented as known limitations
- [ ] `go test -race ./...` passes
- [ ] `go vet ./...` clean
