---
title: "Rust Test Project & Validation"
type: phase
plan: RustParser
phase: 2
status: planned
created: 2026-03-22
updated: 2026-03-22
deliverable: "Comprehensive Rust test project, unit test corpus, CLI validation"
tasks:
  - id: "2.1"
    title: "Create testdata/rust/ project"
    status: planned
    verification: "Multi-file Rust project covering: structs, enums, traits, impl blocks, trait impls, generics, modules (mod), use declarations (all forms), closures, macros (definition and invocation), async/await, error handling (Result, Option), lifetimes in signatures, derive macros, pub/pub(crate) visibility. MANIFEST.md documents expected symbols and edges."
  - id: "2.2"
    title: "Definition tests"
    status: planned
    verification: "Tests for: free function, method in inherent impl (fn in impl Type), method in trait impl (fn in impl Trait for Type), struct, enum, trait (Kind=trait), type alias, module (mod item), macro definition. Each verifies Name, Kind, Parent (for methods), Namespace (module). Also: function with generics, function with lifetime params — verify they don't break extraction."
    depends_on: ["2.1"]
  - id: "2.3"
    title: "Call site tests"
    status: planned
    verification: "Tests for: direct call (foo()), method call (obj.method()), scoped call (module::func()), associated function call (Type::new()), macro invocation (println!(), vec![]), turbofish call (func::<Type>()), chained method call (a.b().c()), closure call, call inside match arm. Each verifies From and To."
    depends_on: ["2.1"]
  - id: "2.4"
    title: "Use declaration tests"
    status: planned
    verification: "Tests for: use foo (simple), use foo::bar (scoped), use foo::{A, B} (grouped — produces 2 edges), use foo::* (wildcard), use foo as bar (alias), use std::io::{self, Read} (nested group — produces 2 edges: std::io and std::io::Read). Each verifies edge To value is the full path."
    depends_on: ["2.1"]
  - id: "2.5"
    title: "Trait impl tests"
    status: planned
    verification: "Tests for: impl Trait for Type (produces EdgeInherits Type→Trait), impl Type (no inheritance edge), impl generic trait (impl<T> Trait for Vec<T>), multiple trait impls for one type. Verified with ClassHierarchy tool."
    depends_on: ["2.1"]
  - id: "2.6"
    title: "Edge case tests"
    status: planned
    verification: "Tests for: empty file (no crash), file with only mod declaration, unsafe block, extern crate, extern fn, #[cfg] conditional compilation attributes, nested modules (mod a { mod b { ... } }), impl block with no methods, trait with default method implementations, async fn, const fn."
    depends_on: ["2.1"]
  - id: "2.7"
    title: "CLI validation against testdata and real project"
    status: planned
    verification: "parse-test on testdata/rust/ matches MANIFEST. parse-test on a real Rust project (e.g., clone a small crate to /tmp) — no crashes, spot-check 20+ symbols."
    depends_on: ["2.2", "2.3", "2.4", "2.5", "2.6"]
  - id: "2.8"
    title: "Structural verification"
    status: planned
    verification: "`go vet ./...` passes. `go test -race ./...` passes."
    depends_on: ["2.7"]
---

# Phase 2: Rust Test Project & Validation

## Overview

Comprehensive testing of the Rust parser — the most complex of the three due to `impl` blocks, traits, and `use` tree expansion.

## 2.1: Create testdata/rust/ project

### Subtasks
- [ ] `testdata/rust/main.rs` — main function, use declarations, function calls
- [ ] `testdata/rust/lib.rs` — pub module declarations
- [ ] `testdata/rust/models.rs` — structs, enums, impl blocks, derive macros
- [ ] `testdata/rust/traits.rs` — trait definitions, trait impls, default methods
- [ ] `testdata/rust/utils.rs` — free functions, type aliases, closures, macros
- [ ] `testdata/rust/errors.rs` — custom error types, Result usage, From impls
- [ ] `testdata/rust/MANIFEST.md`

### Notes
Rust test project should demonstrate:
- Struct with `#[derive(Debug, Clone)]`
- Enum with variants (unit, tuple, struct variants)
- Trait with required and default methods
- `impl Trait for Type` (explicit trait implementation)
- `impl Type` (inherent methods)
- Generic functions and types (`fn foo<T: Display>(x: T)`)
- Lifetime annotations (`fn longest<'a>(x: &'a str) -> &'a str`)
- `use` with all forms: simple, scoped, grouped, wildcard, alias
- Macro definition (`macro_rules!`) and invocation (`println!()`)
- Async functions (`async fn`)
- Closures (`|x| x + 1`) and higher-order functions
- Module hierarchy (`mod a { mod b { ... } }`)
- Error handling patterns (Result, Option, `?` operator)
- Pattern matching in match expressions

## Acceptance Criteria
- [ ] testdata/rust/ with MANIFEST
- [ ] All definition, call, use, trait impl, edge case tests pass
- [ ] CLI validation clean
- [ ] `go test -race ./...` passes
