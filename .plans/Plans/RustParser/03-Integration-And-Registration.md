---
title: "Integration & Registration"
type: phase
plan: RustParser
phase: 3
status: planned
created: 2026-03-22
updated: 2026-03-22
deliverable: "RustParser registered in MCP server, integration tests, docs updated"
tasks:
  - id: "3.1"
    title: "Register RustParser in main.go"
    status: planned
    verification: "RustParser created and registered. Binary compiles. analyze_codebase indexes .rs files."
  - id: "3.2"
    title: "MCP tool integration tests for Rust"
    status: planned
    verification: "Tests: analyze testdata/rust/, get_file_symbols returns Rust symbols, search_symbols finds structs/traits/enums, get_callers/callees for Rust functions (including macro calls), get_dependencies returns use paths, get_class_hierarchy for Rust types with trait impls, get_orphans finds uncalled Rust functions, generate_mermaid for Rust inheritance (trait hierarchy)."
    depends_on: ["3.1"]
  - id: "3.3"
    title: "Full mixed-language indexing test"
    status: planned
    verification: "analyze_codebase on a directory with .cpp, .go, .py, .rs files indexes all four. search_symbols returns symbols from all languages. Stats show correct totals."
    depends_on: ["3.2"]
  - id: "3.4"
    title: "Update docs"
    status: planned
    verification: "README lists all 4 languages. CLAUDE.md lists Rust-specific patterns and limitations (use tree expansion, macro handling). Supported languages table complete."
    depends_on: ["3.2"]
  - id: "3.5"
    title: "Structural verification"
    status: planned
    verification: "`go vet ./...` passes. `go test -race ./...` passes. `make build` works. Full test suite green."
    depends_on: ["3.2", "3.3", "3.4"]
---

# Phase 3: Integration & Registration

## Acceptance Criteria
- [ ] RustParser registered and working
- [ ] All MCP tool integration tests pass for Rust
- [ ] Mixed C++/Go/Python/Rust indexing works
- [ ] Docs complete for all 4 languages
- [ ] `go test -race ./...` passes
