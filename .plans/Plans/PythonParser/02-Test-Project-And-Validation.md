---
title: "Python Test Project & Validation"
type: phase
plan: PythonParser
phase: 2
status: planned
created: 2026-03-22
updated: 2026-03-22
deliverable: "Comprehensive Python test project, unit test corpus, CLI validation"
tasks:
  - id: "2.1"
    title: "Create testdata/python/ project"
    status: planned
    verification: "Multi-file Python project covering: classes with inheritance, decorators, __init__/__str__ methods, static/class methods, free functions, closures, lambda, import and from-import, type hints, dataclasses, abstract base classes, exceptions, generators, context managers. MANIFEST.md documents expected symbols and edges."
  - id: "2.2"
    title: "Definition tests"
    status: planned
    verification: "Tests for: free function, class method, static method (@staticmethod), class method (@classmethod), __init__ method, class definition, decorated function, decorated class, nested function, nested class, lambda (not extracted as symbol — verify no crash). Each verifies Name, Kind, Parent, Namespace."
    depends_on: ["2.1"]
  - id: "2.3"
    title: "Call site tests"
    status: planned
    verification: "Tests for: direct call (foo()), method call (obj.method()), chained call (a.b().c()), constructor call (MyClass()), super() call, built-in call (print(), len()), call inside list comprehension, call inside lambda, call as default argument. Each verifies From and To."
    depends_on: ["2.1"]
  - id: "2.4"
    title: "Import tests"
    status: planned
    verification: "Tests for: import os, from os import path, from os.path import join, import os.path, from . import utils (relative), from typing import List, import as alias. Each verifies correct edge To value."
    depends_on: ["2.1"]
  - id: "2.5"
    title: "Inheritance tests"
    status: planned
    verification: "Tests for: single inheritance (class D(B)), multiple inheritance (class D(A, B)), qualified base (class D(module.Base)), ABC abstract class, no base class (class C: — no edge). Verified with edge kind 'inherits'."
    depends_on: ["2.1"]
  - id: "2.6"
    title: "Edge case tests"
    status: planned
    verification: "Tests for: empty file, file with only comments, syntax error in file (parser skips gracefully), deeply nested classes (class inside class), method with same name as free function, *args/**kwargs in signature, async def function, generator function (yield), property decorator."
    depends_on: ["2.1"]
  - id: "2.7"
    title: "CLI validation against testdata and real project"
    status: planned
    verification: "parse-test on testdata/python/ matches MANIFEST. parse-test on a real Python project (e.g., pip install a small lib to /tmp) — no crashes, spot-check 20+ symbols."
    depends_on: ["2.2", "2.3", "2.4", "2.5", "2.6"]
  - id: "2.8"
    title: "Structural verification"
    status: planned
    verification: "`go vet ./...` passes. `go test -race ./...` passes."
    depends_on: ["2.7"]
---

# Phase 2: Python Test Project & Validation

## Overview

Comprehensive testing of the Python parser.

## 2.1: Create testdata/python/ project

### Subtasks
- [ ] `testdata/python/app.py` — main module, imports, function calls
- [ ] `testdata/python/models.py` — classes, inheritance, dataclasses, ABC
- [ ] `testdata/python/handlers.py` — decorated functions, closures, async
- [ ] `testdata/python/utils.py` — free functions, type aliases, generators
- [ ] `testdata/python/__init__.py` — package init
- [ ] `testdata/python/MANIFEST.md`

### Notes
Python test project should demonstrate:
- Class with `__init__`, `__str__`, `__repr__`
- @property, @staticmethod, @classmethod decorators
- Single and multiple inheritance
- ABC with @abstractmethod
- Async functions (`async def`)
- Generator functions (`yield`)
- Type hints (don't affect tree-sitter, just verify no crash)
- f-strings, walrus operator (syntax variety)

## Acceptance Criteria
- [ ] testdata/python/ with MANIFEST
- [ ] All definition, call, import, inheritance, edge case tests pass
- [ ] CLI validation clean
- [ ] `go test -race ./...` passes
