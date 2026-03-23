---
title: "Python Parser Core"
type: phase
plan: PythonParser
phase: 1
status: planned
created: 2026-03-22
updated: 2026-03-22
deliverable: "PythonParser with all extraction methods"
tasks:
  - id: "1.1"
    title: "Add tree-sitter-python dependency"
    status: planned
    verification: "`go get` succeeds. go mod tidy clean. go build ./... compiles."
  - id: "1.2"
    title: "PythonParser struct and query compilation"
    status: planned
    verification: "NewPythonParser() succeeds. Extensions() returns [.py, .pyi]. Close() releases queries. Satisfies parser.Parser interface."
    depends_on: ["1.1"]
  - id: "1.3"
    title: "Python query string definitions"
    status: planned
    verification: "All query categories compile: definitions (function, class), calls (direct, method/attribute), imports (import, from-import), inheritance (superclasses)."
    depends_on: ["1.2"]
  - id: "1.4"
    title: "Symbol definition extraction"
    status: planned
    verification: "Extracts: free functions, class methods (Parent set to enclosing class), classes, decorated functions/classes (decorator doesn't prevent extraction). Line/Column/EndLine correct. Signature populated."
    depends_on: ["1.3"]
  - id: "1.5"
    title: "Call site extraction"
    status: planned
    verification: "Extracts: direct calls (foo()), method calls (obj.method()), chained calls (a.b().c()). Note: Python uses 'call' node kind, not 'call_expression'. Each edge has correct From and To."
    depends_on: ["1.3"]
  - id: "1.6"
    title: "Import extraction"
    status: planned
    verification: "Extracts: `import foo` → edge To='foo'; `from foo import bar` → edge To='foo'; `import foo.bar` → edge To='foo.bar'; `from . import utils` → edge To='.utils'. Quotes not present in Python imports (unlike Go)."
    depends_on: ["1.3"]
  - id: "1.7"
    title: "Inheritance extraction"
    status: planned
    verification: "Extracts: single inheritance (class D(B)), multiple inheritance (class D(A, B)), qualified base (class D(mod.Base)). Edge kind is 'inherits'."
    depends_on: ["1.3"]
  - id: "1.8"
    title: "Structural verification"
    status: planned
    verification: "`go vet ./...` passes. `go test -race ./internal/lang/python/` passes. All existing tests still pass."
    depends_on: ["1.4", "1.5", "1.6", "1.7"]
---

# Phase 1: Python Parser Core

## Overview

Implement PythonParser in `internal/lang/python/`.

## 1.4: Symbol definition extraction

### Notes
Methods vs free functions: use `findEnclosingKind(node, "class_definition")` to check if a `function_definition` is inside a class body. If so, Kind=method and Parent=class name. Otherwise Kind=function.

Decorated definitions: `@decorator` appears as `decorated_definition` wrapping the function/class. The tree-sitter query `(function_definition name: ...)` still matches the inner node directly because queries search the entire tree.

`__init__`, `__str__`, etc. are ordinary methods — no special handling needed.

## 1.6: Import extraction

### Notes
Python has two import forms:
- `import foo` → `import_statement` with `name: (dotted_name)`
- `from foo import bar` → `import_from_statement` with `module_name: (dotted_name)` and `name:`

For graph purposes, use the module path as the edge `To` value. `from foo import bar` produces an edge to `"foo"` (the module), not `"bar"` (the imported name).

## Acceptance Criteria
- [ ] PythonParser implements Parser interface
- [ ] All Python extraction patterns working
- [ ] All existing tests unaffected
