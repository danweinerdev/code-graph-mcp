---
title: "Rust Parser Core"
type: phase
plan: RustParser
phase: 1
status: planned
created: 2026-03-22
updated: 2026-03-22
deliverable: "KindTrait in types.go, RustParser with all extraction methods"
tasks:
  - id: "1.1"
    title: "Add KindTrait to shared types"
    status: planned
    verification: "KindTrait = 'trait' added to types.go. ClassHierarchy in graph.go accepts KindTrait. Existing tests still pass."
  - id: "1.2"
    title: "Add tree-sitter-rust dependency"
    status: planned
    verification: "`go get` succeeds. go mod tidy clean. go build ./... compiles."
    depends_on: ["1.1"]
  - id: "1.3"
    title: "RustParser struct and query compilation"
    status: planned
    verification: "NewRustParser() succeeds. Extensions() returns [.rs]. Close() releases queries. Satisfies parser.Parser interface."
    depends_on: ["1.2"]
  - id: "1.4"
    title: "Rust query string definitions"
    status: planned
    verification: "All query categories compile: definitions (function_item, struct_item, enum_item, trait_item, type_item, impl_item methods), calls (direct, method, scoped, macro_invocation), use declarations, trait impls."
    depends_on: ["1.3"]
  - id: "1.5"
    title: "Symbol definition extraction"
    status: planned
    verification: "Extracts: free functions, methods inside impl blocks (Parent set from impl_item type field), structs, enums, traits (Kind=trait), type aliases, module names. Correctly distinguishes free function_item vs method function_item via enclosing declaration_list > impl_item. Signature populated."
    depends_on: ["1.4"]
  - id: "1.6"
    title: "Call site extraction"
    status: planned
    verification: "Extracts: direct calls (foo()), method calls (obj.method()), scoped path calls (module::func(), Type::method()), macro invocations (println!(), vec![]). Each edge has correct From and To."
    depends_on: ["1.4"]
  - id: "1.7"
    title: "Use declaration extraction"
    status: planned
    verification: "Extracts: simple use (use foo), scoped use (use foo::bar), grouped use (use foo::{A, B}), wildcard use (use foo::*), use with alias (use foo as bar). Each produces an include edge. Grouped use expanded to individual edges."
    depends_on: ["1.4"]
  - id: "1.8"
    title: "Trait impl extraction"
    status: planned
    verification: "Extracts: `impl Trait for Type` produces EdgeInherits from Type to Trait. `impl Type` (inherent impl) does not produce inheritance edges. Tested with single trait impl, multiple trait impls for one type, generic impls."
    depends_on: ["1.4"]
  - id: "1.9"
    title: "Structural verification"
    status: planned
    verification: "`go vet ./...` passes. `go test -race ./internal/lang/rust/` passes. All existing tests still pass."
    depends_on: ["1.5", "1.6", "1.7", "1.8"]
---

# Phase 1: Rust Parser Core

## Overview

Add `KindTrait` to shared types and implement RustParser in `internal/lang/rust/`.

## 1.5: Symbol definition extraction

### Notes
The key challenge is `impl_item` context resolution:
- `impl Type { fn method() {} }` → method with Parent=Type
- `impl Trait for Type { fn method() {} }` → method with Parent=Type (not Trait)
- Free function at module level → no Parent

Walk up from `function_item` to find enclosing `impl_item`, read its `type:` field. This is the Rust equivalent of C++'s `resolveParentClass`.

Rust modules (`mod_item`) can be used as Namespace, analogous to C++ namespaces.

## 1.7: Use declaration extraction

### Notes
`use_declaration` has complex `use_tree` children:
- `use foo;` → `(use_declaration argument: (identifier))`
- `use foo::bar;` → `(use_declaration argument: (scoped_identifier))`
- `use foo::{A, B};` → `(use_declaration argument: (use_list ...))`
- `use foo::*;` → `(use_declaration argument: (use_wildcard))`

For grouped uses, the Go extractor needs to walk the `use_list` children and construct full paths by combining the parent scope with each child identifier. This is the most novel parsing logic across all three languages.

## Acceptance Criteria
- [ ] KindTrait added and ClassHierarchy updated
- [ ] RustParser implements Parser interface
- [ ] All Rust extraction patterns working including impl context and use tree expansion
- [ ] All existing tests unaffected
