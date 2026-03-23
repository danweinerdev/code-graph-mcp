---
title: "Real-World Validation"
type: phase
plan: CodeGraphMCP
phase: 3
status: complete
created: 2026-03-22
updated: 2026-03-22
deliverable: "Parser validated against real C++ code; CLI test harness; known limitations documented"
tasks:
  - id: "3.1"
    title: "Create testdata C++ project"
    status: complete
    verification: "testdata/cpp/ contains 8 files with known structure and MANIFEST.md documenting expected results"
  - id: "3.2"
    title: "Build CLI test harness"
    status: complete
    verification: "go run ./cmd/parse-test testdata/cpp/ prints structured report with files, symbols, edges, warnings"
    depends_on: ["3.1"]
  - id: "3.3"
    title: "Validate against testdata project"
    status: complete
    verification: "17 symbols, 21 edges extracted matching MANIFEST expectations. Forward declarations excluded. Orphan functions present as symbols."
    depends_on: ["3.2"]
  - id: "3.4"
    title: "Validate against a real open-source C++ project"
    status: complete
    verification: "fmtlib/fmt: 32 symbols, 244 edges, 0 crashes/warnings. Spot-check confirmed correct method names, parent classes, line numbers."
    depends_on: ["3.2"]
  - id: "3.5"
    title: "Fix query patterns from findings"
    status: complete
    verification: "C++ cast expressions (static_cast, dynamic_cast, const_cast, reinterpret_cast) filtered from call edges. Regression test added. Function pointer typedefs documented as known limitation."
    depends_on: ["3.3", "3.4"]
  - id: "3.6"
    title: "Document known limitations"
    status: complete
    verification: "CLAUDE.md documents 7 known limitations: function pointer typedefs, macro-generated definitions, template metaprogramming, syntactic call resolution, C++ cast filtering, forward declaration exclusion, template method calls."
    depends_on: ["3.5"]
  - id: "3.7"
    title: "Structural verification"
    status: complete
    verification: "go vet ./... passes; go test -race ./... passes (24 tests)"
    depends_on: ["3.5"]
---

# Phase 3: Real-World Validation

## Overview

Validation gate for the C++ parser. Exercised against a custom testdata project and fmtlib/fmt (real open-source C++).

## 3.1: Create testdata C++ project

### Subtasks
- [x] `testdata/cpp/main.cpp` — includes engine.h and utils.h; main() calls methods and utility functions
- [x] `testdata/cpp/engine.h` — Engine class, DebugEngine inheriting Engine, Vec2 struct
- [x] `testdata/cpp/engine.cpp` — Engine method implementations calling utility functions
- [x] `testdata/cpp/utils.h` — free function declarations in namespace utils
- [x] `testdata/cpp/utils.cpp` — free function definitions
- [x] `testdata/cpp/circular_a.h` — includes circular_b.h, declares ClassA
- [x] `testdata/cpp/circular_b.h` — includes circular_a.h, declares ClassB
- [x] `testdata/cpp/orphan.cpp` — neverCalled() and alsoOrphaned() functions
- [x] `testdata/cpp/MANIFEST.md` — expected symbols, edges, relationships documented

## 3.2: Build CLI test harness

### Subtasks
- [x] `cmd/parse-test/main.go` — standalone CLI tool
- [x] Accepts directory path, walks and filters by CppParser extensions
- [x] Parses each file, prints structured report (files, symbols, edges, warnings)
- [x] Exits 0 on success

## 3.3: Validate against testdata project

### Subtasks
- [x] 8 files parsed, 17 symbols, 21 edges
- [x] All expected symbols present with correct kinds and lines
- [x] All expected call edges present (after cast filtering)
- [x] Include edges match: engine.h, utils.h, string, iostream, circular includes
- [x] DebugEngine -> Engine inheritance detected
- [x] orphan.cpp functions present as symbols with no incoming calls
- [x] Forward declarations in utils.h correctly excluded

## 3.4: Validate against fmtlib/fmt

### Subtasks
- [x] fmtlib/fmt src/ directory: 4 source files
- [x] 0 crashes, 0 warnings
- [x] 32 symbols extracted — methods like `buffered_file::close`, `file::read` correct
- [x] 244 edges — calls to POSIX functions, fmt utilities, macro invocations
- [x] Inheritance: `utf8_system_category -> std::error_category`
- [x] False positive: `static_cast`/`reinterpret_cast` as calls (fixed in 3.5)
- [x] Macro calls (FMT_THROW, FMT_RETRY) correctly captured as edges

## 3.5: Fix query patterns from findings

### Subtasks
- [x] C++ cast filtering: added `isCppCast()` helper to skip static_cast/dynamic_cast/const_cast/reinterpret_cast
- [x] Regression test `TestCppCastsFiltered` added (24th test)
- [x] All 24 existing tests still pass
- [x] Function pointer typedef: documented as known limitation (not fixed — requires different query pattern)

## 3.6: Document known limitations

### Subtasks
- [x] CLAUDE.md created with build/test/architecture docs and 7 known limitations

## 3.7: Structural verification

### Subtasks
- [x] `go vet ./...` passes
- [x] `go test -race ./...` passes (24 tests)

## Acceptance Criteria
- [x] CLI harness runs and produces readable output
- [x] testdata project: all expected symbols and edges found
- [x] Real-world project (fmtlib/fmt): no crashes, symbols spot-check accurate
- [x] C++ cast false positives fixed with regression test
- [x] Known limitations documented in CLAUDE.md
- [x] `go test -race ./...` passes (24 tests)
- [x] `go vet ./...` clean
