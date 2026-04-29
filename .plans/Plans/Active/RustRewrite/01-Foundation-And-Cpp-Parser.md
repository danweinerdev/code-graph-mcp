---
title: "Foundation & C++ Parser"
type: phase
plan: RustRewrite
phase: 1
status: in-progress
created: 2026-04-28
updated: 2026-04-28
deliverable: "Cargo workspace with core types, language plugin trait, registry, RootConfig TOML loader, and a feature-complete C++ parser passing the ported 24-test corpus and the fmtlib/fmt real-world validation gate"
tasks:
  - id: "1.1"
    title: "Cargo workspace scaffold and toolchain pin"
    status: complete
    verification: "`cargo build --workspace` succeeds with empty crate skeletons; rust-toolchain.toml pins a specific stable channel; .gitignore covers target/ and bin/; `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings` both pass clean on the empty workspace"
  - id: "1.2"
    title: "codegraph-core: shared types"
    status: complete
    depends_on: ["1.1"]
    verification: "Language enum (Cpp/Rust/Go/Python) serializes to lowercase strings; SymbolKind covers Function/Method/Class/Struct/Enum/Typedef/Interface/Trait — Interface and Trait added now (not deferred); EdgeKind covers Calls/Includes/Inherits; Symbol carries `language` field plus all Go fields with `#[serde(skip_serializing_if = \"String::is_empty\")]` on namespace and parent so empty fields omit cleanly; SymbolId helper produces `path:Name` for free symbols and `path:Parent::Name` for methods identical to Go; round-trip JSON tests cover every kind"
  - id: "1.3"
    title: "LanguagePlugin trait, LanguageRegistry, RootConfig"
    status: complete
    depends_on: ["1.2"]
    verification: "LanguagePlugin trait is object-safe (Box<dyn LanguagePlugin> compiles); Registry::for_path returns the right plugin for known extensions and None for unknown; duplicate-extension registration returns an error; RootConfig::load(root) returns Default for missing file, parsed value for valid TOML, ConfigError for malformed TOML (no silent fallback); resolve_concurrency() clamps `max_threads` to available_parallelism() and emits a clamp warning, treats 0 as auto, returns warnings list; tests cover missing file, valid file with auto values, valid file with over-cap values, and malformed TOML"
  - id: "1.4"
    title: "codegraph-lang-cpp: parser struct, queries, helpers"
    status: complete
    depends_on: ["1.3"]
    verification: "CppParser::new() compiles all 4 query strings (definitions, calls, includes, inheritance) against tree-sitter-cpp 0.23.4 without error; Extensions() returns [.cpp .cc .cxx .c .h .hpp .hxx]; helpers split_qualified, strip_include_path, is_cpp_cast, find_enclosing_kind, resolve_namespace, resolve_parent_class, enclosing_function_id are unit-tested with the same fixtures as the Go test corpus; truncate_signature uses char_indices so UTF-8 boundary slicing is impossible by construction (test with multi-byte content past 200 bytes confirming no panic and a valid UTF-8 result)"
  - id: "1.5"
    title: "codegraph-lang-cpp: definition, call, include, inheritance extraction"
    status: complete
    depends_on: ["1.4"]
    verification: "Extracts free functions, qualified methods (Class::method, ns::func), inline methods (field_identifier path), classes/structs/enums (incl. enum class), simple typedefs, function-pointer typedefs, type-alias `using` declarations, operator overloads (free and in-class); each symbol has correct Name, Kind, File, Line, Column, EndLine, Signature, Namespace, Parent — Namespace populated from enclosing namespace_definition, joined `a::b` for nesting, empty for anonymous namespaces; call edges produced for all 4 patterns (free, method, qualified, template free) with correct From (enclosing function symbol ID) and To (callee text); cast expressions (static_cast/dynamic_cast/const_cast/reinterpret_cast) filtered; include edges produced for both quoted and system forms with brackets/quotes stripped; inheritance edges produced for class_specifier and struct_specifier with simple and qualified bases, multiple bases produce multiple edges; tree-sitter error nodes skipped via has_error()"
  - id: "1.6"
    title: "C++ test corpus port and real-world validation"
    status: planned
    depends_on: ["1.5"]
    verification: "All 24 tests from the Go corpus (cpp_test.go) ported to Rust with rstest parameterized cases or equivalent; every query pattern has at least one test exercising it against a real C++ snippet; codegraph-parse-test bin walks testdata/cpp/ via codegraph-tools::discovery (using default RootConfig) and produces 17 symbols and 21 edges matching the original MANIFEST.md; the parse-test bin run against fmtlib/fmt produces 0 crashes, 0 warnings, 32 symbols, 244 edges — same baseline numbers the Go binary delivered; spot-check confirms `buffered_file::close` and `file::read` are extracted with correct parent classes and line numbers; macro calls (FMT_THROW, FMT_RETRY) appear as call edges"
  - id: "1.7"
    title: "Structural verification"
    status: planned
    depends_on: ["1.6"]
    verification: "`cargo fmt --check` clean across workspace; `cargo clippy --workspace --all-targets -- -D warnings` clean (no allow attributes added to suppress findings); `cargo test --workspace` passes including all C++ corpus tests, RootConfig tests, registry tests, and core type tests; codegraph-parse-test bin compiles and runs end-to-end; no `unsafe` blocks introduced (FFI to tree-sitter is encapsulated in the upstream crate)"
---

# Phase 1: Foundation & C++ Parser

## Overview

Establish the Cargo workspace, the shared type system, the language-plugin trait that drives multi-language dispatch, the `RootConfig` TOML loader that controls discovery and parsing concurrency, and a feature-complete C++ parser that passes the same test corpus and real-world validation gate the Go implementation passed (24-test corpus + fmtlib/fmt). After this phase, `codegraph-parse-test` can index any C++ codebase that the Go binary could index, with identical symbol and edge output.

The phase deliberately bundles foundation and C++ work because the foundation is small (workspace, types, trait, registry, config) and the C++ parser is the validation gate for the foundation's design — getting C++ green is the only way to prove the architecture before Phase 2 layers the graph engine on top.

## 1.1: Cargo workspace scaffold and toolchain pin

### Subtasks
- [x] Create top-level `Cargo.toml` workspace manifest listing all 10 crates as members
- [x] Create `rust-toolchain.toml` pinning to stable (e.g., `channel = "1.84"`)
- [x] Create skeleton crates: `crates/code-graph-mcp` (bin), `crates/codegraph-core` (lib), `crates/codegraph-lang` (lib), `crates/codegraph-graph` (lib), `crates/codegraph-tools` (lib), `crates/codegraph-lang-cpp` (lib), `crates/codegraph-parse-test` (bin), plus placeholders for `codegraph-lang-{rust,go,python}` (lib) for later phases
- [x] Top-level `.gitignore` covers `target/`, `bin/`, `.code-graph-cache.json`, `.code-graph.toml` (the latter only at indexed-project root, not workspace root, but ignore-by-default is safer)
- [x] `Makefile` or `justfile` exposes `build`, `test`, `lint`, `fmt-check` recipes
- [x] Workspace builds cleanly with `cargo build --workspace`

## 1.2: codegraph-core: shared types

### Subtasks
- [x] `Language` enum derives `Copy, Clone, Eq, PartialEq, Hash, Debug, Serialize, Deserialize` with `#[serde(rename_all = "lowercase")]`
- [x] `SymbolKind` enum with all 8 variants (Function, Method, Class, Struct, Enum, Typedef, Interface, Trait); same derives; `#[serde(rename_all = "lowercase")]`
- [x] `EdgeKind` enum (Calls, Includes, Inherits) with same derives
- [x] `Symbol` struct with `language` field added; namespace and parent use `#[serde(skip_serializing_if = "String::is_empty", default)]`; line/column/end_line are `u32`
- [x] `Edge` struct with `from`, `to`, `kind`, `file`, `line`
- [x] `FileGraph` struct with `path`, `language`, `symbols`, `edges`
- [x] `SymbolId` type alias for `String`; `symbol_id(&Symbol) -> SymbolId` helper that matches Go's `SymbolID` exactly
- [x] Round-trip JSON serialization tests for every kind, edge variant, and FileGraph
- [x] Wire-format invariant: collections-returning fields (Vec) never serialize as null — verified by tests with empty inputs

## 1.3: LanguagePlugin trait, LanguageRegistry, RootConfig

### Subtasks
- [x] `LanguagePlugin` trait in `codegraph-lang` with `id`, `extensions`, `parse_file`, `resolve_call` (default impl), `resolve_include` (default impl), `close`
- [x] Trait is `Send + Sync` and object-safe (proven by `Box<dyn LanguagePlugin>` compiling)
- [x] `LanguageRegistry` with `register`, `for_path`, `language_for_path`, `plugin_for(Language)`; duplicate registration returns error
- [x] `RootConfig` (in `codegraph-core::config`) with `[discovery]` and `[parsing]` sections; both have `max_threads`, plus discovery gets `respect_gitignore`, `follow_symlinks`, `extra_ignore`
- [x] `RootConfig::load(root)` reads `<root>/.code-graph.toml`; returns `Ok(Default)` if missing; returns `Err(ConfigError::Toml)` on parse failure
- [x] `RootConfig::resolve_concurrency()` clamps `max_threads` to `std::thread::available_parallelism()`; treats 0 as auto; returns a `Vec<String>` of clamp warnings
- [x] Tests: missing file → default; valid auto config; valid pinned-value config; over-cap config (warning emitted, value clamped); malformed TOML (parse error returned, no fallback)

### Notes
The TOML loader fails the entire `analyze_codebase` call on parse error rather than silently falling back to defaults — this is a deliberate decision from the design (Decision 8 / Risks table). A typo in a thread-count is the kind of silent perf-degradation that wastes hours; the explicit error is worth the rare friction.

## 1.4: codegraph-lang-cpp: parser struct, queries, helpers

### Subtasks
- [x] `CppParser` struct with `language`, `def_query`, `call_query`, `incl_query`, `inh_query` fields holding `tree_sitter::Query` objects
- [x] `CppParser::new() -> Result<Self, ParseError>` compiles all 4 queries; returns error if any fails
- [x] `Extensions()` returns the same 7 extensions as Go: `.cpp .cc .cxx .c .h .hpp .hxx`
- [x] `Drop` impl closes queries (tree-sitter Query already drops cleanly; verify no leaks) — no explicit Drop needed; `Query` already drops cleanly
- [x] `queries.rs` ports all 4 query strings verbatim from `internal/lang/cpp/queries.go`: `DEFINITION_QUERIES`, `CALL_QUERIES`, `INCLUDE_QUERIES`, `INHERITANCE_QUERIES`
- [x] Helpers in `helpers.rs`: `find_enclosing_kind(node, kind)`, `resolve_namespace(node, content)`, `resolve_parent_class(node, content)`, `enclosing_function_id(node, content, path)`, `is_cpp_cast(name)`, `split_qualified(qualified)`, `strip_include_path(raw)`, `truncate_signature(s)`
- [x] `truncate_signature` uses `char_indices` to track byte boundaries; the 200-byte fallback returns `&s[..i]` where `i` is guaranteed on a UTF-8 boundary by construction
- [x] Unit tests for each helper with fixtures matching the Go test corpus (TestSplitQualified, TestStripIncludePath, TestTruncateSignature with multi-byte content)
- [x] `var _: &dyn LanguagePlugin = &CppParser::new()?` (or equivalent) compile-time interface check

## 1.5: codegraph-lang-cpp: definition, call, include, inheritance extraction

### Subtasks
- [x] `extract_definitions` iterates `def_query` matches; handles capture names: `func.name`, `inline.name`, `method.qname`, `operator.name`, `class.name`, `struct.name`, `enum.name`, `typedef.name`
- [x] Each symbol populated with Name, Kind, File, Line (1-based), Column (0-based), EndLine, Signature (truncated), Namespace (joined `a::b`), Parent (from class_specifier or struct_specifier or qualified split)
- [x] Method-vs-function distinction: `func.name` becomes Method when enclosed in a class/struct; `method.qname` always Method with parent split from `Scope::Name`
- [x] `extract_calls` iterates `call_query` matches; capture names `call.name` and `call.qname`
- [x] Cast filter: `is_cpp_cast(callee_name)` skips `static_cast`/`dynamic_cast`/`const_cast`/`reinterpret_cast`
- [x] `From` field set via `enclosing_function_id` (returns `path:funcName` or just `path` for top-level calls)
- [x] `extract_includes` iterates `incl_query` matches; quotes/angle brackets stripped
- [x] `extract_inheritance` iterates `inh_query` matches; emits one Edge per (derived, base) pair; both `type_identifier` and `qualified_identifier` base node forms handled
- [x] Error nodes: every extraction loop checks `node.has_error()` and skips gracefully (matches Go behavior, prevents crashes on macro-heavy or template-metaprogramming-heavy code)

### Notes
The 7 documented C++ limitations from `CLAUDE.md` are preserved verbatim — they are intentional, not bugs:
1. Macro-generated definitions invisible to tree-sitter
2. Complex template metaprogramming may produce error nodes (skipped)
3. Call resolution is heuristic (scope-aware, not semantic)
4. C++ cast expressions filtered (`is_cpp_cast`)
5. Forward declarations excluded (only `function_definition` with body produces symbols)
6. Template method calls fall through to method-call pattern
7. Function pointer typedefs handled via the alternation pattern

## 1.6: C++ test corpus port and real-world validation

### Subtasks
- [ ] Port all 24 tests from `internal/lang/cpp/cpp_test.go`: free function, methods (qualified and inline), classes, structs, enums (incl. enum class), typedefs (simple, function-pointer, using-alias), nested namespaces, multiple inheritance, qualified inheritance, all 4 call patterns, both include forms, forward decl exclusion, top-level call, anonymous namespace, signature truncation, helper functions, C++ cast filter regression
- [ ] Use `rstest` parameterized cases or table-driven tests where the Go corpus did the same
- [ ] `pretty_assertions::assert_eq` for diff-friendly failure output
- [ ] Add 1 new test for UTF-8 boundary in truncate_signature with multi-byte content past 200 bytes
- [ ] `codegraph-parse-test` bin: walks a directory via `codegraph-tools::discovery` (placeholder if Phase 3 isn't ready — for Phase 1 use a synchronous `walkdir` direct loop and migrate to the parallel walker in Phase 3); per-language plugin dispatch; prints structured report with files, symbols, edges, warnings; matches the original Go `cmd/parse-test` output format
- [ ] `parse-test testdata/cpp/` produces 17 symbols and 21 edges matching `testdata/cpp/MANIFEST.md`
- [ ] `parse-test <fmtlib/fmt clone>/src/` produces 0 crashes, 0 warnings, 32 symbols, 244 edges
- [ ] Spot-check fmtlib output: `buffered_file::close`, `file::read` correctly attributed; macro calls captured

### Notes
For Phase 1, the parse-test bin uses a simple synchronous walker since the parallel discovery walker doesn't ship until Phase 3. This keeps Phase 1 self-contained while still validating the parser. Phase 3 swaps in `codegraph-tools::discovery::discover` and re-runs the validation as a regression check.

## 1.7: Structural verification

### Subtasks
- [ ] `cargo fmt --check` passes across the entire workspace
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings` passes clean
- [ ] No `#[allow(clippy::...)]` attributes added to suppress findings — fix the underlying issue or document why the lint is wrong
- [ ] `cargo test --workspace` passes (all C++ corpus tests, RootConfig tests, registry tests, core type round-trip tests, helper tests)
- [ ] `cargo test --doc --workspace` passes (any doc examples compile and run)
- [ ] `codegraph-parse-test --version` prints something; `codegraph-parse-test testdata/cpp` runs end-to-end and exits 0
- [ ] No `unsafe` blocks introduced anywhere in this workspace's code (FFI to tree-sitter is fully encapsulated in the upstream `tree-sitter` crate)

## Acceptance Criteria
- [ ] Cargo workspace builds clean; toolchain pinned; lint and format gates green
- [ ] `Language`, `SymbolKind` (incl. Interface and Trait), `EdgeKind`, `Symbol`, `Edge`, `FileGraph` all defined with stable JSON serialization
- [ ] `LanguagePlugin` trait + `LanguageRegistry` + extension dispatch working; duplicate registration error case handled
- [ ] `RootConfig` TOML loader works for missing-file, valid, over-cap (clamped with warning), and malformed (error returned) cases
- [ ] `CppParser` extracts all symbol kinds, all 4 call patterns, both include forms, single/multiple/qualified inheritance; cast filter and error-node skip both verified
- [ ] 24-test corpus + UTF-8 boundary test all pass
- [ ] testdata/cpp/ produces expected 17 symbols / 21 edges
- [ ] fmtlib/fmt produces expected 32 symbols / 244 edges, 0 crashes, 0 warnings
- [ ] `cargo clippy -- -D warnings` clean across the workspace
