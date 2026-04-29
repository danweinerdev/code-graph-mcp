---
title: "Rust Language Parser"
type: phase
plan: RustRewrite
phase: 5
status: planned
created: 2026-04-28
updated: 2026-04-28
deliverable: "codegraph-lang-rust crate parsing .rs files with full impl/trait/use-tree/macro support; registered in the main binary; testdata/rust/ + dogfood-validation against this Rust workspace itself"
tasks:
  - id: "5.1"
    title: "codegraph-lang-rust crate scaffold + queries.rs"
    status: planned
    verification: "RustParser::new() compiles all query strings against tree-sitter-rust 0.24 without error; Extensions() returns [.rs]; query categories: definitions (function_item, struct_item, enum_item, trait_item, type_item, mod_item, impl_item methods), calls (direct identifier, method via field_expression, scoped via scoped_identifier, macro_invocation), use declarations, trait impls (impl_item with both type and trait fields); compile-time interface check `var _: &dyn LanguagePlugin = &RustParser::new()?`; helpers split_use_path, find_enclosing_impl, resolve_mod_namespace unit-tested"
  - id: "5.2"
    title: "Definition extraction with impl context and trait-impl disambiguation"
    status: planned
    depends_on: ["5.1"]
    verification: "Free function_item produces Kind=Function with no parent; function_item inside impl_item produces Kind=Method with parent=impl_item.type field; for `impl Trait for Type { fn m() }` the method's parent is Type (NOT Trait) — verified by a dedicated test fixture that inverts the parent and asserts the method-by-ID lookup `path:Type::m` resolves; struct_item → Struct, enum_item → Enum, trait_item → Trait (Kind=Trait), type_item → Typedef; mod_item populates Symbol.namespace recursively (a::b for nested mods, joined with ::); generic params (function_item with type_parameters), lifetime params, async fn, const fn, unsafe fn — all extracted without crash; signature truncated at `{` or `;` via shared truncate_signature"
  - id: "5.3"
    title: "Use-tree expansion (recursive walk for grouped/wildcard/aliased imports)"
    status: planned
    depends_on: ["5.1"]
    verification: "use foo → 1 edge to 'foo'; use foo::bar → 1 edge to 'foo::bar'; use foo::{a, b} → 2 edges (foo::a, foo::b); use foo::{a, b::c} → 2 edges (foo::a, foo::b::c); use foo::* → 1 edge to 'foo::*'; use foo as bar → 1 edge to 'foo' (path, not alias); use std::{io::{self, Read}, collections::HashMap} → 3 edges (std::io, std::io::Read, std::collections::HashMap); each edge has Kind=Includes; recursive walk handles nested use_list, scoped_use_list, use_wildcard, use_as_clause; tests for each form"
  - id: "5.4"
    title: "Call extraction (direct, method, scoped, macro) and inheritance (impl Trait for Type)"
    status: planned
    depends_on: ["5.1"]
    verification: "call_expression with function: identifier → direct call edge; with field_expression → method call edge; with scoped_identifier → scoped call edge (full path preserved as To); macro_invocation with identifier → macro call edge; turbofish (call_expression with generic args) → captured; chained calls a.b().c() → 2 edges; closure calls captured; impl_item with `trait` field present produces EdgeKind=Inherits from impl.type to impl.trait — verified with single trait impl, generic trait impl (impl<T> Trait for Vec<T>), multiple trait impls per type; impl_item without trait field (inherent impl) produces NO inheritance edge"
  - id: "5.5"
    title: "testdata/rust + corpus tests + dogfood validation"
    status: planned
    depends_on: ["5.2", "5.3", "5.4"]
    verification: "testdata/rust/ project covers structs/enums/traits/impl-blocks/trait-impls/generics/modules/use-declarations(all forms)/closures/macros/async/error-handling/lifetimes/derive-macros/visibility; MANIFEST.md documents expected symbols and edges; corpus tests cover all definition forms, all call patterns, all use forms, trait impl edges, and edge cases (empty file, mod-only file, unsafe block, extern crate, cfg attributes, nested mods, impl with no methods, trait with default methods); CLI parse-test on testdata/rust matches MANIFEST counts; **dogfood: parse-test on this very Rust workspace's crates/ directory** produces sensible output — every public type defined in `codegraph-core::types` appears as a symbol with correct kind; the `LanguagePlugin` trait shows up as Kind=Trait; `impl LanguagePlugin for CppParser` produces an inherits edge; spot-check 20+ symbols including methods inside impl blocks; 0 crashes, 0 warnings"
  - id: "5.6"
    title: "Register parser, integration tests, documentation"
    status: planned
    depends_on: ["5.5"]
    verification: "main.rs registers RustParser alongside CppParser via reg.register(RustParser::new()?); analyze_codebase on a directory containing both .cpp and .rs indexes both; mixed-language search test: search for a name that exists in both languages without a language filter returns both; with language='cpp' returns only C++; with language='rust' returns only Rust; get_class_hierarchy on a Rust trait works (regression test for the widened {Class, Struct, Interface, Trait} root filter from Phase 2); generate_diagram for a Rust trait inheritance produces edges; wire-format snapshot tests extended with Rust-specific responses (cargo insta accept on the new fixtures); README and CLAUDE.md updated to list Rust as a supported language with .rs extension and any limitations (e.g. macro_rules! definitions not extracted as symbols, only invocations as call edges)"
  - id: "5.7"
    title: "Structural verification"
    status: planned
    depends_on: ["5.6"]
    verification: "`cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean across all crates including the new codegraph-lang-rust; `cargo test --workspace` green — every Phase 1-5 test passes; release build succeeds; no new `unsafe` introduced; no `#[allow(clippy::...)]` attributes added to suppress findings"
---

# Phase 5: Rust Language Parser

## Overview

Add Rust language support — the priority-2 deliverable per the user's explicit ordering. Rust is the most complex of the three new parsers due to `impl` blocks, trait impls, `use`-tree traversal, and macro invocations, so we tackle it first while the architecture is freshest. Validation includes dogfooding the parser against this very Rust workspace's source code, which doubles as a confidence check that the parser handles a real production codebase.

This phase replaces the original `Plans/RustParser/` (status: superseded as of Phase 4 cutover). Every node-type and query pattern from that plan is carried forward, expressed against the Rust crate.

## 5.1: codegraph-lang-rust crate scaffold + queries.rs

### Subtasks
- [ ] Crate `crates/codegraph-lang-rust` with `tree-sitter-rust = "0.24"` dependency
- [ ] `RustParser` struct with cached Query objects (definitions, calls, uses, inheritance/trait-impl)
- [ ] `Extensions()` returns `[".rs"]`
- [ ] `queries.rs` constants:
  - `DEFINITION_QUERIES`: function_item, struct_item, enum_item, trait_item, type_item, mod_item, impl_item methods
  - `CALL_QUERIES`: identifier, field_expression, scoped_identifier, macro_invocation
  - `USE_QUERIES`: use_declaration with use_tree variants
  - `INHERITANCE_QUERIES`: impl_item with both type and trait fields
- [ ] Helpers in `helpers.rs`:
  - `split_use_path(use_tree, content) -> Vec<String>` — recursive walker
  - `find_enclosing_impl(node) -> Option<&Node>` — walks up to impl_item
  - `resolve_mod_namespace(node, content) -> String` — joins enclosing mod_item names with `::`
- [ ] Compile-time interface check

## 5.2: Definition extraction with impl context and trait-impl disambiguation

### Subtasks
- [ ] `extract_definitions` iterates definition matches
- [ ] `function_item` enclosed in `impl_item` → Kind=Method, parent = impl_item.type field text
- [ ] **Trait-impl disambiguation:** for `impl Trait for Type { fn m() }`, parent is `Type`, never `Trait`. Method's symbol ID becomes `path:Type::m`. The Trait relationship lives only in the inheritance edge.
- [ ] `function_item` at module level → Kind=Function, no parent
- [ ] `struct_item` → Struct; `enum_item` → Enum; `trait_item` → Trait; `type_item` → Typedef
- [ ] `mod_item` populates namespace via recursive walk; nested mods joined `a::b::c`
- [ ] Signature uses shared `truncate_signature`
- [ ] Tests:
  - Free function vs impl method
  - `impl Type` inherent method (parent=Type)
  - `impl Trait for Type` method (parent=Type, not Trait — explicit anti-regression test)
  - Generic function (`fn foo<T: Display>(x: T)`)
  - Lifetime parameters (`fn longest<'a>(x: &'a str)`)
  - async fn, const fn, unsafe fn
  - Nested mods (`mod a { mod b { fn x() {} } }`) — namespace `a::b`

## 5.3: Use-tree expansion

### Subtasks
- [ ] `extract_uses` iterates use_query matches
- [ ] `split_use_path` recursively walks `use_tree` children:
  - `identifier` / `scoped_identifier` → terminal path
  - `use_list` → recurse into each child, prepend parent scope
  - `scoped_use_list` → same with scope prefix
  - `use_wildcard` → terminal `*` appended to parent scope
  - `use_as_clause` → ignore alias, use the path
  - `self` keyword in a use_list (e.g. `std::io::{self, Read}`) → emit the parent scope itself as a leaf
- [ ] Each terminal path becomes one Edge { from: file, to: full_path, kind: Includes }
- [ ] Tests for every form listed in the verification field, including the deeply nested `use std::{io::{self, Read}, collections::HashMap}`

## 5.4: Call extraction and inheritance

### Subtasks
- [ ] `extract_calls`:
  - `call_expression > function: identifier` → direct call
  - `call_expression > function: field_expression > field: field_identifier` → method call
  - `call_expression > function: scoped_identifier` → scoped call (full path preserved as To)
  - `macro_invocation > macro: identifier` → macro call edge (matches LegacyGraph-MCP behavior)
  - Turbofish (`call_expression > function: generic_function`) → captured via the same identifier path
  - Chained calls (`a.b().c()`) produce two edges
- [ ] `extract_inheritance`:
  - `impl_item` with both `type` and `trait` fields → Edge { from: type_text, to: trait_text, kind: Inherits, file, line }
  - `impl_item` with only `type` (inherent impl) → no inheritance edge
  - Generic impls (`impl<T> Trait for Vec<T>`) handled
- [ ] Tests for every call pattern and every impl shape

## 5.5: testdata/rust + corpus tests + dogfood validation

### Subtasks
- [ ] `testdata/rust/` Cargo project with files:
  - `main.rs` — main fn, use declarations, function calls
  - `lib.rs` — pub module declarations, type aliases
  - `models.rs` — structs with `#[derive(...)]`, enums with all variant kinds
  - `traits.rs` — trait definitions, trait impls (incl. generic), default methods
  - `utils.rs` — free functions, type aliases, closures, macro_rules! definitions and invocations
  - `errors.rs` — custom error types, `From` impls (trait impl edges), `Result` usage, `?` operator
  - `MANIFEST.md` — expected symbols, edges, namespace map
- [ ] `parse-test testdata/rust` matches MANIFEST counts
- [ ] **Dogfooding gate:** `parse-test crates/` against this very workspace
  - `LanguagePlugin` trait in `codegraph-lang` shows up as Kind=Trait
  - `impl LanguagePlugin for CppParser` produces an inherits edge from CppParser to LanguagePlugin
  - Methods inside impl blocks have correct parents (e.g., `crates/codegraph-graph/src/graph.rs:Graph::merge_file_graph`)
  - 0 crashes, 0 warnings
  - Spot-check 20+ symbols across multiple crates

## 5.6: Register parser, integration tests, documentation

### Subtasks
- [ ] `main.rs`: `let rust_parser = codegraph_lang_rust::RustParser::new()?; reg.register(rust_parser);`
- [ ] Mixed-language integration test: directory with .cpp and .rs files; `analyze_codebase` indexes both; `search_symbols` without language filter returns from both; with `language="cpp"` returns only C++; with `language="rust"` returns only Rust
- [ ] `get_class_hierarchy` for a Rust trait — regression for Phase 2's widened root filter
- [ ] `generate_diagram` for a Rust trait produces inheritance edges
- [ ] Wire-format snapshot tests extended with Rust-specific responses
- [ ] README + CLAUDE.md updated:
  - Add Rust to supported languages table
  - List Rust-specific patterns and limitations: `macro_rules!` definitions not extracted as symbols (only invocations as call edges); call resolution still heuristic; complex use trees expanded but lifetime/generic constraints not represented

## 5.7: Structural verification

### Subtasks
- [ ] `cargo fmt --check` clean across the entire workspace
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean — no new `#[allow]` suppressions
- [ ] `cargo test --workspace` green — Phase 1-5 tests all pass
- [ ] `cargo build --release` succeeds for the host platform
- [ ] No new `unsafe` blocks introduced

## Acceptance Criteria
- [ ] RustParser implements LanguagePlugin
- [ ] All extraction patterns working including impl context, use-tree expansion, trait impl edges, macro calls
- [ ] Trait-impl method parent disambiguation explicitly tested
- [ ] testdata/rust passes; workspace dogfooding passes
- [ ] Mixed C++ + Rust indexing works
- [ ] Rust trait `get_class_hierarchy` works
- [ ] All Phase 1-5 tests pass
