---
title: "Rust Language Parser"
type: phase
plan: RustRewrite
phase: 5
status: complete
created: 2026-04-28
updated: 2026-04-30
deliverable: "codegraph-lang-rust crate parsing .rs files with full impl/trait/use-tree/macro support; registered in the main binary; testdata/rust/ + dogfood-validation against this Rust workspace itself"
tasks:
  - id: "5.1"
    title: "codegraph-lang-rust crate scaffold + queries.rs + Cargo.toml dependencies"
    status: planned
    verification: "`tree-sitter-rust = \"=0.24.0\"` added to workspace `[workspace.dependencies]` (strict `=` pin matching the Phase 1 C++ convention `tree-sitter-cpp = \"=0.23.4\"` — guards against a patch release silently changing query node types); `crates/codegraph-lang-rust/Cargo.toml` `[dependencies]` populated (tree-sitter, tree-sitter-rust, streaming-iterator, codegraph-core, codegraph-lang, thiserror, anyhow) and `[dev-dependencies]` populated (rstest, pretty_assertions, insta); `cargo build -p codegraph-lang-rust` green before any parser code lands; `RustParser::new() -> anyhow::Result<RustParser>` compiles all query strings against tree-sitter-rust 0.24 without error (errors surface via `anyhow::bail!` or `?`-conversion); `extensions()` returns `[\".rs\"]`; `id()` returns `Language::Rust`; **object-safety + id() verified by a single `#[test] fn rust_parser_is_object_safe_via_box_dyn() { let p: Box<dyn LanguagePlugin> = Box::new(RustParser::new().unwrap()); assert_eq!(p.id(), Language::Rust); }` matching the Phase 1 C++ test at `crates/codegraph-lang-cpp/src/lib.rs:542-545` exactly** (the dead-`fn _object_safety_check` form would fail the `-D warnings` clippy gate); `RustParser` does NOT override `resolve_call` or `resolve_include` — accepts the default implementations from the `LanguagePlugin` trait (default `resolve_call` is the scope-aware heuristic; default `resolve_include` is a basename match against the FileIndex which is a no-op for Rust `use` paths since they are dotted module paths, not filesystem paths — correct for Rust); query categories: definitions (function_item, struct_item, enum_item, trait_item, type_item, mod_item, impl_item methods — explicitly NOT macro_rules_definition; that is documented in the queries module as a deliberate exclusion), calls (direct identifier, method via field_expression, scoped via scoped_identifier, macro_invocation), use declarations, trait impls (impl_item with both type and trait fields); helpers split_use_path, find_enclosing_impl, resolve_mod_namespace unit-tested"
  - id: "5.2"
    title: "Definition extraction with impl context and trait-impl disambiguation"
    status: planned
    depends_on: ["5.1"]
    verification: "Free function_item produces Kind=Function with no parent; function_item inside impl_item produces Kind=Method with parent=impl_item.type field; for `impl Trait for Type { fn m() }` the method's parent is Type (NOT Trait) — verified by a dedicated test fixture that inverts the parent and asserts the method-by-ID lookup `path:Type::m` resolves; struct_item → Struct, enum_item → Enum, trait_item → Trait (Kind=Trait), type_item → Typedef; mod_item populates Symbol.namespace recursively (a::b for nested mods, joined with ::); generic params (function_item with type_parameters), lifetime params, async fn, const fn, unsafe fn — all extracted without crash; signature truncated at `{` or `;` via shared truncate_signature; macro_rules! definitions produce ZERO symbols (anti-regression against the queries accidentally matching `macro_rules_definition`)"
  - id: "5.3"
    title: "Use-tree expansion (recursive walk for grouped/wildcard/aliased imports) + extern crate"
    status: planned
    depends_on: ["5.1"]
    verification: "use foo → 1 edge to 'foo'; use foo::bar → 1 edge to 'foo::bar'; use foo::{a, b} → 2 edges (foo::a, foo::b); use foo::{a, b::c} → 2 edges (foo::a, foo::b::c); use foo::* → 1 edge to 'foo::*'; use foo as bar → 1 edge to 'foo' (path, not alias); use std::{io::{self, Read}, collections::HashMap} → 3 edges (std::io, std::io::Read, std::collections::HashMap); `extern crate alloc;` → 1 edge to 'alloc' (Includes); each edge has Kind=Includes; recursive walk handles nested use_list, scoped_use_list, use_wildcard, use_as_clause; tests for each form including extern_crate_declaration"
  - id: "5.4"
    title: "Call extraction (direct, method, scoped, macro) and inheritance (impl Trait for Type)"
    status: planned
    depends_on: ["5.1"]
    verification: "call_expression with function: identifier → direct call edge; with field_expression → method call edge; with scoped_identifier → scoped call edge (full path preserved as To); macro_invocation with identifier → macro call edge; turbofish (call_expression with generic args) → captured; chained calls a.b().c() → 2 edges; closure calls captured; impl_item with `trait` field present produces EdgeKind=Inherits from impl.type to impl.trait — verified with single trait impl, generic trait impl (impl<T> Trait for Vec<T>), generic impl with where clause (impl<T> Trait for Foo<T> where T: Display), multiple trait impls per type; impl_item without trait field (inherent impl) produces NO inheritance edge"
  - id: "5.5"
    title: "testdata/rust + corpus tests + dogfood validation + watch-mode reindex regression"
    status: planned
    depends_on: ["5.2", "5.3", "5.4"]
    verification: "testdata/rust/ project covers structs/enums/traits/impl-blocks/trait-impls/generics(both type-bound and where-clause forms)/modules/use-declarations(all forms)/extern-crate/closures/macros/async/error-handling/lifetimes/derive-macros/visibility; MANIFEST.md documents expected symbols and edges; corpus tests cover all definition forms, all call patterns, all use forms, trait impl edges, and edge cases (empty file, mod-only file, unsafe block, extern crate, cfg attributes, nested mods, impl with no methods, trait with default methods); CLI parse-test on testdata/rust matches MANIFEST counts; **watch-mode reindex regression: start a watch on a temp Rust project, modify a `.rs` file (add a function, remove a struct), confirm `get_file_symbols` reflects the change after the debounce window, confirm `Graph::prune_dangling_edges` invariant holds (no adj/radj entries point at removed symbols) — exercises the same `try_reindex_file` path Phase 4 Task 4.2 established; mirrors the structure of `crates/codegraph-tools/tests/watch_dangling_edges.rs`**; **dogfood: parse-test on this very Rust workspace's crates/ directory** produces sensible output — every public type defined in `codegraph-core::types` appears as a symbol with correct kind; the `LanguagePlugin` trait shows up as Kind=Trait; `impl LanguagePlugin for CppParser` produces an inherits edge; spot-check 20+ symbols including methods inside impl blocks; 0 crashes, 0 warnings"
  - id: "5.6"
    title: "Register parser, integration tests, documentation"
    status: planned
    depends_on: ["5.5"]
    verification: "main.rs registers RustParser alongside CppParser via the shipped pattern: `.register(Box::new(RustParser::new().context(\"initialize Rust language plugin\")?,)).context(\"register Rust language plugin\")?;` (mirroring the C++ block at `crates/code-graph-mcp/src/main.rs:20-23`); analyze_codebase on a directory containing both .cpp and .rs indexes both; mixed-language search test backed by `testdata/mixed/` (containing `foo.cpp` defining `int helper()` and `foo.rs` defining `fn helper()`): search for `helper` without language filter returns both; with language='cpp' returns only C++; with language='rust' returns only Rust; get_class_hierarchy on a Rust trait works (regression test for the widened {Class, Struct, Interface, Trait} root filter from Phase 2); generate_diagram for a Rust trait inheritance produces edges; wire-format snapshot tests extended with Rust-specific responses (cargo insta accept on the new fixtures); README and CLAUDE.md updated to list Rust as a supported language with .rs extension and document Rust-specific limitations: `macro_rules!` definitions not extracted as symbols (only invocations as call edges); `#[derive(...)]` and proc-macro attributes appear as `attribute_item` not `macro_invocation` and are NOT captured as call edges; complex use trees expanded but lifetime/generic constraints not represented; call resolution still heuristic"
  - id: "5.7"
    title: "Structural verification"
    status: planned
    depends_on: ["5.6"]
    verification: "`make release` (host-target only — cross-compile was removed in Phase 4) produces a binary that includes the Rust plugin; `cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean across all crates including the new codegraph-lang-rust; `cargo test --workspace` green — every Phase 1-5 test passes (398 tests from Phase 4 baseline plus the new Rust-plugin tests); no new `unsafe` introduced (workspace `unsafe_code = \"forbid\"`); no `#[allow(clippy::...)]` attributes added to suppress findings; `cargo audit` clean"
tags: [rewrite, rust, mcp, code-graph, tree-sitter, cpp, multi-language]
---

# Phase 5: Rust Language Parser

## Overview

Add Rust language support — the priority-2 deliverable per the user's explicit ordering. Rust is the most complex of the three new parsers due to `impl` blocks, trait impls, `use`-tree traversal, and macro invocations, so we tackle it first while the architecture is freshest. Validation includes dogfooding the parser against this very Rust workspace's source code, which doubles as a confidence check that the parser handles a real production codebase.

This phase replaces the original `Plans/RustParser/` (status: superseded as of Phase 4 cutover). Every node-type and query pattern from that plan is carried forward, expressed against the Rust crate.

This doc was reviewed against the as-shipped state of phases 1-4 on 2026-04-30 (see `notes/04-Watch-Cross-Compile-Cutover.md`). Updates incorporate: the actual `LanguageRegistry::register(Box<dyn LanguagePlugin>)` signature with anyhow `.context(...)` wrappers, the `Graph::prune_dangling_edges` invariant established in Phase 4.2, the no-cross-compile build path from Phase 4.3, and the `(Language, name)` SymbolIndex keying from Phase 3.

## 5.1: codegraph-lang-rust crate scaffold + queries.rs + Cargo.toml dependencies

### Subtasks
- [ ] Add `tree-sitter-rust = "=0.24.0"` to workspace `Cargo.toml` `[workspace.dependencies]` (strict `=` pin matching the Phase 1 C++ convention `tree-sitter-cpp = "=0.23.4"`)
- [ ] `crates/codegraph-lang-rust/Cargo.toml`:
  - `[dependencies]`: tree-sitter, tree-sitter-rust (workspace = true), streaming-iterator, codegraph-core, codegraph-lang, thiserror, anyhow
  - `[dev-dependencies]`: rstest, pretty_assertions, insta
- [ ] **Compile gate:** `cargo build -p codegraph-lang-rust` succeeds (empty crate, deps resolve) before any parser code is written
- [ ] `RustParser` struct with cached Query objects (definitions, calls, uses, inheritance/trait-impl)
- [ ] `RustParser::new() -> anyhow::Result<RustParser>` — query compilation errors surface via `anyhow::bail!` or `?`-conversion (matches the C++ pattern)
- [ ] `extensions()` returns `[".rs"]` (lowercase trait method, matching `LanguagePlugin` signature)
- [ ] `id()` returns `Language::Rust`
- [ ] **Default trait methods:** `RustParser` does NOT override `resolve_call` or `resolve_include`. Rationale: the default `resolve_call` is the scope-aware heuristic and matches the C++ plugin pattern; the default `resolve_include` is a basename match against the `FileIndex` which is a no-op for Rust `use` paths because they are dotted module paths, not filesystem paths — leaving them unresolved is the intended behavior (the wire format records the full `use` path as the edge's `to` field).
- [ ] `queries.rs` constants:
  - `DEFINITION_QUERIES`: function_item, struct_item, enum_item, trait_item, type_item, mod_item, impl_item methods. **Explicitly does NOT match `macro_rules_definition`** — this exclusion is a documented decision (macro definitions are not extracted as symbols; only invocations produce call edges).
  - `CALL_QUERIES`: identifier, field_expression, scoped_identifier, macro_invocation
  - `USE_QUERIES`: use_declaration with use_tree variants; `extern_crate_declaration` for the legacy `extern crate <name>;` form
  - `INHERITANCE_QUERIES`: impl_item with both type and trait fields
- [ ] Helpers in `helpers.rs`:
  - `split_use_path(use_tree, content) -> Vec<String>` — recursive walker
  - `find_enclosing_impl(node) -> Option<&Node>` — walks up to impl_item
  - `resolve_mod_namespace(node, content) -> String` — joins enclosing mod_item names with `::`
- [ ] **Object-safety + id() test** in `#[cfg(test)] mod tests` (mirrors `crates/codegraph-lang-cpp/src/lib.rs:542-545` exactly):
  ```rust
  #[test]
  fn rust_parser_is_object_safe_via_box_dyn() {
      let p: Box<dyn LanguagePlugin> = Box::new(RustParser::new().unwrap());
      assert_eq!(p.id(), Language::Rust);
  }
  ```
  This single `#[test]` covers both object-safety (Box-dyn coercion) and the `id()` contract. The dead-`fn _object_safety_check()` form would emit a `dead_code` warning that fails the `-D warnings` clippy gate; the `#[test]` form avoids that and matches the shipped C++ test exactly.

## 5.2: Definition extraction with impl context and trait-impl disambiguation

### Subtasks
- [ ] `extract_definitions` iterates definition matches
- [ ] `function_item` enclosed in `impl_item` → Kind=Method, parent = impl_item.type field text
- [ ] **Trait-impl disambiguation:** for `impl Trait for Type { fn m() }`, parent is `Type`, never `Trait`. Method's symbol ID becomes `path:Type::m`. The Trait relationship lives only in the inheritance edge.
- [ ] `function_item` at module level → Kind=Function, no parent
- [ ] `struct_item` → Struct; `enum_item` → Enum; `trait_item` → Trait; `type_item` → Typedef
- [ ] `mod_item` populates namespace via recursive walk; nested mods joined `a::b::c`
- [ ] Signature uses shared `truncate_signature`
- [ ] `macro_rules!` definitions produce zero symbols (anti-regression test asserts a fixture with `macro_rules! my_macro { ... }` yields no Symbol)
- [ ] Tests:
  - Free function vs impl method
  - `impl Type` inherent method (parent=Type)
  - `impl Trait for Type` method (parent=Type, not Trait — explicit anti-regression test)
  - Generic function with type bound (`fn foo<T: Display>(x: T)`)
  - Generic function with where clause (`fn foo<T>(x: T) where T: Display`)
  - Lifetime parameters (`fn longest<'a>(x: &'a str)`)
  - async fn, const fn, unsafe fn
  - Nested mods (`mod a { mod b { fn x() {} } }`) — namespace `a::b`
  - macro_rules! definition produces zero symbols

## 5.3: Use-tree expansion + extern crate

### Subtasks
- [ ] `extract_uses` iterates use_query matches
- [ ] `split_use_path` recursively walks `use_tree` children:
  - `identifier` / `scoped_identifier` → terminal path
  - `use_list` → recurse into each child, prepend parent scope
  - `scoped_use_list` → same with scope prefix
  - `use_wildcard` → terminal `*` appended to parent scope
  - `use_as_clause` → ignore alias, use the path
  - `self` keyword in a use_list (e.g. `std::io::{self, Read}`) → emit the parent scope itself as a leaf
- [ ] `extract_extern_crate` handles `extern_crate_declaration` nodes — emit Edge { from: file, to: crate_name, kind: Includes }
- [ ] Each terminal path becomes one Edge { from: file, to: full_path, kind: Includes }
- [ ] Tests for every use form listed in the verification field, including the deeply nested `use std::{io::{self, Read}, collections::HashMap}`, plus `extern crate alloc;`

## 5.4: Call extraction and inheritance

### Subtasks
- [ ] `extract_calls`:
  - `call_expression > function: identifier` → direct call
  - `call_expression > function: field_expression > field: field_identifier` → method call
  - `call_expression > function: scoped_identifier` → scoped call (full path preserved as To)
  - `macro_invocation > macro: identifier` → macro call edge
  - Turbofish (`call_expression > function: generic_function`) → captured via the same identifier path
  - Chained calls (`a.b().c()`) produce two edges
- [ ] `extract_inheritance`:
  - `impl_item` with both `type` and `trait` fields → Edge { from: type_text, to: trait_text, kind: Inherits, file, line }
  - `impl_item` with only `type` (inherent impl) → no inheritance edge
  - Generic impls (`impl<T> Trait for Vec<T>`) handled
  - Generic impls with where clause (`impl<T> Trait for Foo<T> where T: Display`) handled
- [ ] Tests for every call pattern and every impl shape

## 5.5: testdata/rust + corpus tests + dogfood + watch-mode regression

### Subtasks
- [ ] `testdata/rust/` Cargo project with files:
  - `main.rs` — main fn, use declarations, function calls
  - `lib.rs` — pub module declarations, type aliases
  - `models.rs` — structs with `#[derive(...)]`, enums with all variant kinds
  - `traits.rs` — trait definitions, trait impls (incl. generic with type bounds AND where clauses), default methods, async methods
  - `utils.rs` — free functions, type aliases, closures, macro_rules! definitions and invocations, extern crate declaration
  - `errors.rs` — custom error types, `From` impls (trait impl edges), `Result` usage, `?` operator
  - `MANIFEST.md` — expected symbols, edges, namespace map (asserts macro_rules! definitions produce 0 symbols)
- [ ] `parse-test testdata/rust` matches MANIFEST counts
- [ ] **Watch-mode reindex regression** — new test in `crates/codegraph-tools/tests/watch_rust_reindex.rs`:
  - Spawn watch on a temp directory containing `lib.rs` with `pub fn alpha()` and `pub fn beta()`
  - Modify `lib.rs`: remove `beta`, add `pub fn gamma()`
  - After debounce, assert `get_file_symbols` shows `alpha` + `gamma`, no `beta`
  - Assert no dangling edges remain (any prior caller of `beta` is pruned per `Graph::prune_dangling_edges`)
  - Mirrors the structure of `crates/codegraph-tools/tests/watch_dangling_edges.rs`
- [ ] **Dogfooding gate:** `parse-test crates/` against this very workspace
  - `LanguagePlugin` trait in `codegraph-lang` shows up as Kind=Trait
  - `impl LanguagePlugin for CppParser` produces an inherits edge from CppParser to LanguagePlugin
  - Methods inside impl blocks have correct parents (e.g., `crates/codegraph-graph/src/graph.rs:Graph::merge_file_graph`)
  - 0 crashes, 0 warnings
  - Spot-check 20+ symbols across multiple crates

## 5.6: Register parser, integration tests, documentation

### Subtasks
- [ ] `crates/code-graph-mcp/src/main.rs` registers RustParser using the shipped Box+context pattern (mirrors the C++ block at `main.rs:20-23`):
  ```rust
  .register(Box::new(
      codegraph_lang_rust::RustParser::new()
          .context("initialize Rust language plugin")?,
  ))
  .context("register Rust language plugin")?;
  ```
- [ ] **Mixed-language fixture:** create `testdata/mixed/` with `foo.cpp` defining `int helper()` and `foo.rs` defining `fn helper()` — `helper` is the shared anchor for the cross-language search test
- [ ] Mixed-language integration test: `analyze_codebase` on `testdata/mixed/` indexes both; `search_symbols` for `helper` without language filter returns both; with `language="cpp"` returns only C++; with `language="rust"` returns only Rust
- [ ] `get_class_hierarchy` for a Rust trait — regression for Phase 2's widened root filter
- [ ] `generate_diagram` for a Rust trait produces inheritance edges
- [ ] Wire-format snapshot tests extended with Rust-specific responses
- [ ] README + CLAUDE.md updated:
  - Add Rust to supported languages table (extension `.rs`)
  - List Rust-specific patterns and limitations: `macro_rules!` definitions not extracted as symbols (only invocations as call edges); `#[derive(...)]` and proc-macro attributes appear as `attribute_item` (not `macro_invocation`) so they are NOT captured as call edges; call resolution still heuristic; complex use trees expanded but lifetime/generic constraints not represented

## 5.7: Structural verification

### Subtasks
- [ ] `make release` (host-target only; cross-compile was removed in Phase 4) succeeds and produces a binary that includes the Rust plugin
- [ ] `cargo fmt --check` clean across the entire workspace
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean — no new `#[allow]` suppressions
- [ ] `cargo test --workspace` green — Phase 1-5 tests all pass (398 from Phase 4 baseline + new Rust-plugin tests)
- [ ] `cargo audit` clean (no new advisories)
- [ ] No new `unsafe` blocks introduced (workspace `unsafe_code = "forbid"`)

## Acceptance Criteria
- [ ] RustParser implements LanguagePlugin (object-safety check passes)
- [ ] All extraction patterns working including impl context, use-tree expansion, extern crate, trait impl edges, macro calls
- [ ] Trait-impl method parent disambiguation explicitly tested
- [ ] macro_rules! definitions explicitly excluded from symbol extraction
- [ ] testdata/rust passes; workspace dogfooding passes
- [ ] Mixed C++ + Rust indexing works with cross-language search isolation
- [ ] Rust trait `get_class_hierarchy` works
- [ ] Watch-mode reindex regression passes (incremental reindex + dangling-edge prune)
- [ ] All Phase 1-5 tests pass; lint, format, audit gates clean
