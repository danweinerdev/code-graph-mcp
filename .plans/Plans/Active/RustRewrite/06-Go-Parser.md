---
title: "Go Language Parser"
type: phase
plan: RustRewrite
phase: 6
status: planned
created: 2026-04-28
updated: 2026-04-28
deliverable: "codegraph-lang-go crate parsing .go files with method-receiver extraction, structs, interfaces, all import forms, and direct + selector-expression call patterns; registered in the main binary; testdata/go/ + real-world validation"
tasks:
  - id: "6.1"
    title: "codegraph-lang-go crate scaffold + queries.rs"
    status: planned
    verification: "GoParser::new() compiles all queries against tree-sitter-go 0.25 without error; Extensions() returns [.go]; query categories: definitions (function_declaration, method_declaration with receiver, type_spec→struct_type, type_spec→interface_type, type_spec→type alias), calls (call_expression with identifier, call_expression with selector_expression), imports (import_spec with interpreted_string_literal), package_clause for namespace; helpers extract_receiver_type (handles pointer_type and value type_identifier), find_enclosing_func unit-tested; compile-time interface check"
  - id: "6.2"
    title: "Definition extraction with method receiver as parent"
    status: planned
    depends_on: ["6.1"]
    verification: "function_declaration → Kind=Function with no parent; method_declaration → Kind=Method with parent=receiver type name; receiver type extracted whether pointer (`func (s *Server) M()` → parent=Server) or value (`func (s Server) M()` → parent=Server); struct via type_spec+struct_type → Kind=Struct; interface via type_spec+interface_type → Kind=Interface (the new SymbolKind variant added in Phase 1.2); type alias (type ID = string) → Kind=Typedef; package name from package_clause populates Symbol.namespace; init() and main() functions extracted as ordinary functions; generic functions (Go 1.18+ `func Map[T any](...)` ) extracted without crash; signature truncated by shared truncate_signature; tests cover each case"
  - id: "6.3"
    title: "Call site extraction (direct + selector_expression)"
    status: planned
    depends_on: ["6.1"]
    verification: "Direct calls (foo()) via call_expression > function: identifier produce edge with To=callee name; method/package-qualified calls (obj.Method(), fmt.Println()) via call_expression > function: selector_expression > field: field_identifier produce edges with To=field name; chained calls (a.B().C()) produce 2 edges (To=B, To=C); go statements (go foo()) produce call edges naturally because the child of go_statement is a call_expression already matched by the query; defer statements likewise (defer conn.Close() → edge To=Close); call inside closure literal still produces edges with the enclosing function as From; tests for each pattern"
  - id: "6.4"
    title: "Import extraction"
    status: planned
    depends_on: ["6.1"]
    verification: "Single import (import \"fmt\") → 1 edge with To='fmt' (quotes stripped); grouped import (import ( \"fmt\"; \"os\" )) → 2 edges; aliased import (import f \"fmt\") → 1 edge with To='fmt' (path preserved, alias dropped); dot import (import . \"testing\") → 1 edge with To='testing'; blank import (import _ \"image/png\") → 1 edge with To='image/png'; relative imports not applicable in Go (modules system handles this); each edge has Kind=Includes; tests cover every form"
  - id: "6.5"
    title: "testdata/go + corpus tests + real-world validation"
    status: planned
    depends_on: ["6.2", "6.3", "6.4"]
    verification: "testdata/go/ multi-package project covers: structs with exported/unexported methods, interface definition, structural implementation (interface satisfied by concrete type, no edge), pointer and value receivers, goroutines (go fn()), defer, multiple import styles, init() function, closures, embedded structs, generic functions; MANIFEST.md documents expected symbols and edges; corpus tests cover all definition forms, all call patterns, all import forms, and edge cases (empty file with only package clause, interface embedding interface, anonymous struct field, blank identifier function); parse-test testdata/go matches MANIFEST counts; **real-world dogfood**: parse-test against a small open-source Go project (e.g., `github.com/sirupsen/logrus` or similar small lib cloned to /tmp) — 0 crashes, 0 warnings, spot-check 20+ symbols for accuracy (correct method receivers, package paths, import edges)"
  - id: "6.6"
    title: "Register parser, integration tests, documentation"
    status: planned
    depends_on: ["6.5"]
    verification: "main.rs registers GoParser alongside CppParser and RustParser; analyze_codebase on a directory with .cpp + .rs + .go indexes all three; mixed-language search and language-filter queries verified for the new combination; cross-language symbol-collision regression: a function named 'init' in Go and 'init' in C++ both exist after analyze, neither resolves to the other's calls (verified by checking the (Language, name) keying of SymbolIndex); Go interface get_class_hierarchy returns the interface as root with no bases or derived (interfaces are structural in Go, no inheritance edges); wire-format snapshot tests extended with Go-specific responses; README and CLAUDE.md updated to list Go and any Go-specific limitations (structural interface implementation not represented; method dispatch is heuristic)"
  - id: "6.7"
    title: "Structural verification"
    status: planned
    depends_on: ["6.6"]
    verification: "`cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean across all crates including the new codegraph-lang-go; `cargo test --workspace` green — every Phase 1-6 test passes; release build succeeds; no new unsafe; no allow attributes suppressing findings"
---

# Phase 6: Go Language Parser

## Overview

Add Go language support — priority 3 per the user's ordering. Go's grammar is the simplest of the three new parsers (no preprocessor, no templates, no overloading); the only tricky part is method receiver extraction. This phase replaces the original `Plans/GoParser/` (status: superseded as of Phase 4 cutover).

This phase also serves as the cross-language collision regression check: a function named `init` exists in both C++ and Go codebases, and the `(Language, name)`-keyed SymbolIndex from Phase 3 must keep them distinct during call resolution. The test fixture explicitly covers this.

## 6.1: codegraph-lang-go crate scaffold + queries.rs

### Subtasks
- [ ] Crate `crates/codegraph-lang-go` with `tree-sitter-go = "0.25"`
- [ ] `GoParser` with cached Query objects (definitions, calls, imports)
- [ ] `Extensions()` returns `[".go"]`
- [ ] `queries.rs`:
  - `DEFINITION_QUERIES`: function_declaration, method_declaration, type_spec with struct_type / interface_type, type_alias
  - `CALL_QUERIES`: identifier (direct), selector_expression (method/package-qualified)
  - `IMPORT_QUERIES`: import_spec with interpreted_string_literal
- [ ] Helpers in `helpers.rs`:
  - `extract_receiver_type(receiver_node, content)` — handles `(parameter_list (parameter_declaration type: (pointer_type (type_identifier))))` and `(parameter_list (parameter_declaration type: (type_identifier)))`
  - `extract_package_name(root, content)` — finds `package_clause`
- [ ] Compile-time interface check

## 6.2: Definition extraction with method receiver as parent

### Subtasks
- [ ] `function_declaration` → Function, no parent
- [ ] `method_declaration`:
  - Extract `receiver: parameter_list` field
  - Walk the parameter_declaration's `type` field
  - If `pointer_type`, descend into its child `type_identifier`
  - Otherwise read `type_identifier` directly
  - Set `Symbol.parent` to the type name
- [ ] `type_spec` containing `struct_type` → Kind=Struct, name from `type_identifier`
- [ ] `type_spec` containing `interface_type` → Kind=Interface
- [ ] `type_spec` with non-struct/non-interface body → Kind=Typedef (e.g., `type ID = string`, `type Handler func(...)`)
- [ ] `package_clause > (package_identifier)` → set Symbol.namespace (single-level; Go packages are flat)
- [ ] Generic functions (Go 1.18+) — `function_declaration` with `type_parameters` field; the Go grammar handles this; verify no crash on a generic function fixture
- [ ] init() and main() are ordinary functions
- [ ] Tests for each form, including value vs pointer receiver, exported vs unexported names, generic functions

## 6.3: Call site extraction

### Subtasks
- [ ] `extract_calls`:
  - `call_expression > function: identifier` → direct call (To = identifier text)
  - `call_expression > function: selector_expression > field: field_identifier` → method or package-qualified call (To = field text)
- [ ] Enclosing function: walk up from the call node to `function_declaration` or `method_declaration`; extract function name; build From = `path:funcName` or `path:Parent::Name` for methods
- [ ] `go` and `defer` statements naturally captured because they wrap a `call_expression` that the query already matches
- [ ] Closures (function literals) — calls inside them have the enclosing top-level function as From
- [ ] Tests:
  - `func f() { foo() }` → edge To=foo
  - `func f() { s.Start() }` → edge To=Start
  - `func f() { fmt.Println("x") }` → edge To=Println
  - `func f() { go handler() }` → edge To=handler
  - `func f() { defer conn.Close() }` → edge To=Close
  - `func f() { a.B().C() }` → 2 edges (To=B, To=C)
  - Call inside closure assigned to var

## 6.4: Import extraction

### Subtasks
- [ ] `extract_imports` iterates import_spec matches
- [ ] Strip surrounding quotes from `interpreted_string_literal`
- [ ] Aliased imports: `import f "fmt"` — the import_spec has both a `name: package_identifier` (the alias) and a `path: interpreted_string_literal`; capture the path, not the alias
- [ ] Dot imports (`import . "testing"`) and blank imports (`import _ "image/png"`) — same treatment, capture the path
- [ ] Grouped imports — each import_spec inside `import_declaration > import_spec_list` produces its own edge
- [ ] Tests for each form

## 6.5: testdata/go + corpus tests + real-world validation

### Subtasks
- [ ] `testdata/go/` multi-package project:
  - `main.go` — package main, imports, calls into other packages
  - `server/server.go` — Server struct with methods (pointer + value receivers), interface implementation
  - `server/handler.go` — HTTP handler functions, closures
  - `models/user.go` — struct, interface, embedded fields
  - `models/repo.go` — interface, generic function
  - `utils/helpers.go` — free functions, type alias, init()
  - `MANIFEST.md` — expected symbols and edges
- [ ] Corpus tests in `tests.rs` covering every definition, call, import form + edge cases (empty file, mod-only, interface-embedding-interface, anonymous struct field)
- [ ] `parse-test testdata/go` matches MANIFEST
- [ ] Real-world dogfood: clone `github.com/sirupsen/logrus` (a stable, well-known, mid-sized Go library) to `/tmp/logrus` at a pinned tag (e.g. v1.9.3), run `parse-test /tmp/logrus`, expect 0 crashes, 0 warnings, and an approximate symbol count between 200 and 500 — record the actual numbers in the test as the regression baseline (commit the expected count to a fixture file so future runs detect drift)

## 6.6: Register parser, integration tests, documentation

### Subtasks
- [ ] `main.rs` registers GoParser
- [ ] Mixed-language test: directory with .cpp + .rs + .go all indexed
- [ ] **Cross-language collision regression test:** a fixture with `func init()` in Go and `void init()` in C++; analyze; assert `search_symbols` without language filter returns both; assert `get_callers` against the Go init does NOT return the C++ init's callers (and vice versa) — verifying the `(Language, name)`-keyed SymbolIndex isolation
- [ ] `get_class_hierarchy` for a Go interface returns the interface as root; bases and derived are empty (no structural inheritance edges in Go); the lookup itself succeeds (Phase 2 widened root filter)
- [ ] Wire-format snapshot tests extended with Go-specific responses
- [ ] README + CLAUDE.md updated:
  - Add Go to supported languages
  - Limitations: structural interface implementation not represented as edges; method dispatch resolved heuristically

## 6.7: Structural verification

### Subtasks
- [ ] `cargo fmt --check` clean across the workspace
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` green — Phase 1-6 tests all pass
- [ ] `cargo build --release` succeeds
- [ ] No new `unsafe` or `#[allow]` suppressions

## Acceptance Criteria
- [ ] GoParser implements LanguagePlugin
- [ ] All extraction patterns working including method receiver extraction, all import forms, direct + selector_expression calls
- [ ] testdata/go passes; real-world Go project parses cleanly
- [ ] Mixed C++ + Rust + Go indexing works
- [ ] Cross-language collision regression passes
- [ ] All Phase 1-6 tests pass
