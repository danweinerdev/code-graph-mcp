---
title: "Integration & Registration"
type: phase
plan: PythonParser
phase: 3
status: planned
created: 2026-03-22
updated: 2026-03-22
deliverable: "PythonParser registered in MCP server, integration tests, docs updated"
tasks:
  - id: "3.1"
    title: "Register PythonParser in main.go"
    status: planned
    verification: "PythonParser created and registered. Binary compiles. analyze_codebase indexes .py files."
  - id: "3.2"
    title: "MCP tool integration tests for Python"
    status: planned
    verification: "Tests: analyze testdata/python/, get_file_symbols returns Python symbols, search_symbols finds classes, get_callers/callees for Python functions, get_dependencies returns Python imports, get_class_hierarchy for Python classes with inheritance, get_orphans finds uncalled Python functions, generate_mermaid for Python symbols and class inheritance."
    depends_on: ["3.1"]
  - id: "3.3"
    title: "Mixed-language indexing test (C++ + Go + Python)"
    status: planned
    verification: "analyze_codebase on a directory with .cpp, .go, .py files indexes all three. search_symbols returns symbols from all languages."
    depends_on: ["3.2"]
  - id: "3.4"
    title: "Update docs"
    status: planned
    verification: "README lists Python. CLAUDE.md lists Python-specific patterns and limitations (dynamic typing, decorator handling)."
    depends_on: ["3.2"]
  - id: "3.5"
    title: "Structural verification"
    status: planned
    verification: "`go vet ./...` passes. `go test -race ./...` passes. `make build` works."
    depends_on: ["3.2", "3.3", "3.4"]
---

# Phase 3: Integration & Registration

## Acceptance Criteria
- [ ] PythonParser registered and working
- [ ] All MCP tool integration tests pass for Python
- [ ] Mixed C++/Go/Python indexing works
- [ ] Docs updated
