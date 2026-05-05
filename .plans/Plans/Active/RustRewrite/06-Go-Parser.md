---
title: "Go Language Parser"
type: phase
plan: RustRewrite
phase: 6
status: in-progress
created: 2026-04-28
updated: 2026-05-05
deliverable: "codegraph-lang-go crate parsing .go files with method-receiver extraction, structs, interfaces, all import forms, and direct + selector-expression call patterns; registered in the main binary; testdata/go/ + real-world validation"
tasks:
  - id: "6.1"
    title: "codegraph-lang-go crate scaffold + queries.rs + Cargo.toml dependencies"
    status: complete
    verification: "`tree-sitter-go = \"=0.25.0\"` added to workspace `[workspace.dependencies]` (strict `=` pin matching the Phase 1 C++ convention); `crates/codegraph-lang-go/Cargo.toml` `[dependencies]` populated (tree-sitter, tree-sitter-go, streaming-iterator, codegraph-core, codegraph-lang, thiserror, anyhow) and `[dev-dependencies]` populated (rstest, pretty_assertions, insta); `cargo build -p codegraph-lang-go` green before any parser code lands; `GoParser::new() -> anyhow::Result<GoParser>` compiles all queries against tree-sitter-go 0.25 without error; `extensions()` returns `[\".go\"]`; `id()` returns `Language::Go` (already defined in `crates/codegraph-core/src/lib.rs:29` from Phase 1 — no codegraph-core change needed); **object-safety + id() verified by a single `#[test] fn go_parser_is_object_safe_via_box_dyn() { let p: Box<dyn LanguagePlugin> = Box::new(GoParser::new().unwrap()); assert_eq!(p.id(), Language::Go); }` matching the Phase 1 C++ test at `crates/codegraph-lang-cpp/src/lib.rs:542-545` exactly**; `GoParser` does NOT override `resolve_call` or `resolve_include` — `resolve_call` accepts the default scope-aware heuristic; `resolve_include` accepts the default basename match against the FileIndex, which is a no-op for Go import paths because they are module paths (e.g. `\"github.com/sirupsen/logrus\"`), not filesystem paths — leaving them unresolved is the intended behavior; query categories: definitions (function_declaration, method_declaration with receiver, type_spec→struct_type, type_spec→interface_type, type_spec→type alias), calls (call_expression with identifier, call_expression with selector_expression), imports (import_spec with interpreted_string_literal), package_clause for namespace; helpers extract_receiver_type (handles pointer_type and value type_identifier), extract_package_name unit-tested"
  - id: "6.2"
    title: "Definition extraction with method receiver as parent"
    status: complete
    depends_on: ["6.1"]
    verification: "function_declaration → Kind=Function with no parent; method_declaration → Kind=Method with parent=receiver type name; receiver type extracted whether pointer (`func (s *Server) M()` → parent=Server) or value (`func (s Server) M()` → parent=Server); struct via type_spec+struct_type → Kind=Struct; interface via type_spec+interface_type → Kind=Interface; type alias (type ID = string) → Kind=Typedef; package name from package_clause populates Symbol.namespace; init() and main() functions extracted as ordinary functions; generic functions (Go 1.18+ `func Map[T any](...)` ) extracted without crash; signature truncated by shared truncate_signature; tests cover each case"
  - id: "6.3"
    title: "Call site extraction (direct + selector_expression)"
    status: complete
    depends_on: ["6.1"]
    verification: "Direct calls (foo()) via call_expression > function: identifier produce edge with To=callee name; method/package-qualified calls (obj.Method(), fmt.Println()) via call_expression > function: selector_expression > field: field_identifier produce edges with To=field name; chained calls (a.B().C()) produce 2 edges (To=B, To=C); go statements (go foo()) produce call edges naturally because the child of go_statement is a call_expression already matched by the query; defer statements likewise (defer conn.Close() → edge To=Close); call inside closure literal still produces edges with the enclosing function as From; tests for each pattern"
  - id: "6.4"
    title: "Import extraction"
    status: complete
    depends_on: ["6.1"]
    verification: "Single import (import \"fmt\") → 1 edge with To='fmt' (quotes stripped); grouped import (import ( \"fmt\"; \"os\" )) → 2 edges; aliased import (import f \"fmt\") → 1 edge with To='fmt' (path preserved, alias dropped); dot import (import . \"testing\") → 1 edge with To='testing'; blank import (import _ \"image/png\") → 1 edge with To='image/png'; relative imports not applicable in Go (modules system handles this); each edge has Kind=Includes; tests cover every form"
  - id: "6.5"
    title: "testdata/go + corpus tests + real-world validation + watch-mode reindex regression"
    status: in-progress
    depends_on: ["6.2", "6.3", "6.4"]
    verification: "testdata/go/ multi-package project covers: structs with exported/unexported methods, interface definition, structural implementation (interface satisfied by concrete type, no edge), pointer and value receivers, goroutines (go fn()), defer, multiple import styles, init() function, closures, embedded structs, generic functions; MANIFEST.md documents expected symbols and edges; corpus tests cover all definition forms, all call patterns, all import forms, and edge cases (empty file with only package clause, interface embedding interface, anonymous struct field, blank identifier function); parse-test testdata/go matches MANIFEST counts; **watch-mode reindex regression: start a watch on a temp Go project, modify a `.go` file (add a function, remove a method), confirm `get_file_symbols` reflects the change after the debounce window, confirm `Graph::prune_dangling_edges` invariant holds (no adj/radj entries point at removed symbols) — mirrors the Phase 4 watch-test structure**; **real-world dogfood**: parse-test against `github.com/sirupsen/logrus` (small, stable Go library) cloned to /tmp at a pinned tag (v1.9.3) — 0 crashes, 0 warnings, approximate symbol count between 200 and 500 recorded as a regression baseline in a committed fixture file"
  - id: "6.6"
    title: "Register parser, integration tests, documentation"
    status: planned
    depends_on: ["6.5"]
    verification: "main.rs registers GoParser using the shipped Box+context pattern: `.register(Box::new(GoParser::new().context(\"initialize Go language plugin\")?,)).context(\"register Go language plugin\")?;` (mirroring `crates/code-graph-mcp/src/main.rs:20-23`); analyze_codebase on a directory with .cpp + .rs + .go indexes all three; mixed-language search and language-filter queries verified for the new combination; cross-language symbol-collision regression: a function named 'init' in Go and 'init' in C++ both exist after analyze, neither resolves to the other's calls (verified by checking the (Language, name) keying of SymbolIndex from Phase 3 — `crates/codegraph-lang/src/lib.rs:116`); Go interface get_class_hierarchy returns the interface as root with no bases or derived (interfaces are structural in Go, no inheritance edges); wire-format snapshot tests extended with Go-specific responses; README and CLAUDE.md updated to list Go and any Go-specific limitations (structural interface implementation not represented; method dispatch is heuristic; go.mod/vendor handling is not in scope — discovery walks files and respects .gitignore, no module-path resolution)"
  - id: "6.7"
    title: "Structural verification"
    status: planned
    depends_on: ["6.6"]
    verification: "`make release` (host-target only; cross-compile was removed in Phase 4) succeeds and produces a binary that includes the Go plugin; `cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean across all crates including the new codegraph-lang-go; `cargo test --workspace` green — every Phase 1-6 test passes; `cargo audit` clean; no new unsafe (workspace `unsafe_code = \"forbid\"`); no allow attributes suppressing findings"
---

# Phase 6: Go Language Parser

## Overview

Add Go language support — priority 3 per the user's ordering. Go's grammar is the simplest of the three new parsers (no preprocessor, no templates, no overloading); the only tricky part is method receiver extraction. This phase replaces the original `Plans/GoParser/` (status: superseded as of Phase 4 cutover).

This phase also serves as the cross-language collision regression check: a function named `init` exists in both C++ and Go codebases, and the `(Language, name)`-keyed SymbolIndex from Phase 3 must keep them distinct during call resolution. The test fixture explicitly covers this.

This doc was reviewed against the as-shipped state of phases 1-4 on 2026-04-30 (see `notes/04-Watch-Cross-Compile-Cutover.md`). Updates incorporate: the actual `LanguageRegistry::register(Box<dyn LanguagePlugin>)` signature with anyhow `.context(...)` wrappers, the `Graph::prune_dangling_edges` invariant established in Phase 4.2, the no-cross-compile build path from Phase 4.3, and the explicit `id() → Language::Go` registration requirement.

## 6.1: codegraph-lang-go crate scaffold + queries.rs + Cargo.toml dependencies

### Subtasks
- [x] Add `tree-sitter-go = "=0.25.0"` to workspace `Cargo.toml` `[workspace.dependencies]` (strict `=` pin matching the Phase 1 C++ convention)
- [x] `crates/codegraph-lang-go/Cargo.toml`:
  - `[dependencies]`: tree-sitter, tree-sitter-go (workspace = true), streaming-iterator, codegraph-core, codegraph-lang, thiserror, anyhow
  - `[dev-dependencies]`: rstest, pretty_assertions, insta
- [x] **Compile gate:** `cargo build -p codegraph-lang-go` succeeds (empty crate, deps resolve) before any parser code is written
- [x] `GoParser` with cached Query objects (definitions, calls, imports)
- [x] `GoParser::new() -> anyhow::Result<GoParser>`
- [x] `extensions()` returns `[".go"]`
- [x] `id()` returns `Language::Go` — already defined in `crates/codegraph-core/src/lib.rs:29` (scaffolded in Phase 1); no `codegraph-core` change needed
- [x] **Default trait methods:** `GoParser` does NOT override `resolve_call` or `resolve_include`. Rationale: `resolve_call` accepts the default scope-aware heuristic matching the C++ pattern; `resolve_include` accepts the default basename match against the FileIndex, which is a no-op for Go import paths because they are module paths (e.g. `"github.com/sirupsen/logrus"`), not filesystem paths — leaving them unresolved is the intended behavior.
- [x] `queries.rs`:
  - `DEFINITION_QUERIES`: function_declaration, method_declaration, type_spec with struct_type / interface_type, type_alias
  - `CALL_QUERIES`: identifier (direct), selector_expression (method/package-qualified)
  - `IMPORT_QUERIES`: import_spec with interpreted_string_literal
- [x] Helpers in `helpers.rs`:
  - `extract_receiver_type(receiver_node, content)` — handles `(parameter_list (parameter_declaration type: (pointer_type (type_identifier))))` and `(parameter_list (parameter_declaration type: (type_identifier)))`
  - `extract_package_name(root, content)` — finds `package_clause`
- [x] **Object-safety + id() test** in `#[cfg(test)] mod tests` (mirrors `crates/codegraph-lang-cpp/src/lib.rs:542-545` exactly):
  ```rust
  #[test]
  fn go_parser_is_object_safe_via_box_dyn() {
      let p: Box<dyn LanguagePlugin> = Box::new(GoParser::new().unwrap());
      assert_eq!(p.id(), Language::Go);
  }
  ```

## 6.2: Definition extraction with method receiver as parent

### Subtasks
- [x] `function_declaration` → Function, no parent
- [x] `method_declaration`:
  - Extract `receiver: parameter_list` field
  - Walk the parameter_declaration's `type` field
  - If `pointer_type`, descend into its child `type_identifier`
  - Otherwise read `type_identifier` directly
  - Set `Symbol.parent` to the type name
- [x] `type_spec` containing `struct_type` → Kind=Struct, name from `type_identifier`
- [x] `type_spec` containing `interface_type` → Kind=Interface
- [x] `type_spec` with non-struct/non-interface body → Kind=Typedef (e.g., `type ID = string`, `type Handler func(...)`)
- [x] `package_clause > (package_identifier)` → set Symbol.namespace (single-level; Go packages are flat)
- [x] Generic functions (Go 1.18+) — `function_declaration` carries a `type_parameters` field as a sibling of `name: (identifier)`. Confirm via a unit fixture: `func Map[T any](s []T) []T {}` → name=`Map`, Kind=Function, parent empty, signature truncates correctly via `truncate_signature` (which already handles bracketed type params before `{`)
- [x] init() and main() are ordinary functions
- [x] **Embedded struct fields produce NO Inherits edge.** `type Foo struct { Bar }` is structural composition (method-set promotion at runtime), not inheritance — no edge is emitted. Anti-regression test asserts a fixture with an embedded field yields zero `Inherits` edges.
- [x] Tests for each form, including value vs pointer receiver, exported vs unexported names, generic functions, embedded struct fields

## 6.3: Call site extraction

### Subtasks
- [x] `extract_calls`:
  - `call_expression > function: identifier` → direct call (To = identifier text)
  - `call_expression > function: selector_expression > field: field_identifier` → method or package-qualified call (To = field text)
- [x] Enclosing function: walk up from the call node to `function_declaration` or `method_declaration`; extract function name; build From = `path:funcName` or `path:Parent::Name` for methods. **Fallback: if the walk reaches the source file root without finding a function/method declaration (e.g., a call inside a package-level closure assigned to a global: `var H = func() { foo() }`), set From = the file path.** Matches the C++ lambda-at-global-scope behavior.
- [x] `go` and `defer` statements naturally captured because they wrap a `call_expression` that the query already matches
- [x] Closures (function literals) — calls inside them have the enclosing top-level function as From
- [x] Tests:
  - `func f() { foo() }` → edge To=foo
  - `func f() { s.Start() }` → edge To=Start
  - `func f() { fmt.Println("x") }` → edge To=Println
  - `func f() { go handler() }` → edge To=handler
  - `func f() { defer conn.Close() }` → edge To=Close
  - `func f() { a.B().C() }` → 2 edges (To=B, To=C)
  - Call inside closure assigned to var
  - **Package-level closure: `var H = func() { foo() }` → edge has From=file path (no enclosing function declaration)**

## 6.4: Import extraction

### Subtasks
- [x] `extract_imports` iterates import_spec matches
- [x] Strip surrounding quotes from `interpreted_string_literal`
- [x] Aliased imports: `import f "fmt"` — the import_spec has both a `name: package_identifier` (the alias) and a `path: interpreted_string_literal`; capture the path, not the alias
- [x] Dot imports (`import . "testing"`) and blank imports (`import _ "image/png"`) — same treatment, capture the path
- [x] Grouped imports — each import_spec inside `import_declaration > import_spec_list` produces its own edge
- [x] Tests for each form

## 6.5: testdata/go + corpus tests + real-world validation + watch-mode regression

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
- [ ] **Watch-mode reindex regression** — new test in `crates/codegraph-tools/tests/watch_go_reindex.rs`:
  - Spawn watch on a temp directory containing `srv.go` with `func (s *Server) Alpha()` and `func (s *Server) Beta()`
  - Modify `srv.go`: remove `Beta`, add `func (s *Server) Gamma()`
  - After debounce, assert `get_file_symbols` shows `Alpha` + `Gamma`, no `Beta`
  - Assert no dangling edges remain (any prior caller of `Server.Beta` is pruned per `Graph::prune_dangling_edges`)
- [ ] Real-world dogfood: clone `github.com/sirupsen/logrus` (a stable, well-known, mid-sized Go library) to `/tmp/logrus` at a pinned tag (e.g. v1.9.3), run `parse-test /tmp/logrus`, expect 0 crashes, 0 warnings, and an approximate symbol count between 200 and 500 — record the actual count in `testdata/go/logrus-baseline.txt` (one line: `symbols: N`); a follow-up test asserts the recorded count stays within ±10% as a regression gate

## 6.6: Register parser, integration tests, documentation

### Subtasks
- [ ] `crates/code-graph-mcp/src/main.rs` registers GoParser using the shipped Box+context pattern (mirrors the C++ block at `main.rs:20-23`):
  ```rust
  .register(Box::new(
      codegraph_lang_go::GoParser::new()
          .context("initialize Go language plugin")?,
  ))
  .context("register Go language plugin")?;
  ```
- [ ] **Mixed-language test (extends Phase 5 fixture):** add `foo.go` defining `func helper() {}` to the existing `testdata/mixed/` directory created in Phase 5.6 (which already contains `foo.cpp` and `foo.rs` defining `helper`); run `analyze_codebase` on the extended fixture and confirm all three are indexed; `search_symbols` for `helper` without language filter returns three entries; with each `language=` filter returns only that language's match
- [ ] **Cross-language collision regression test:** a fixture with `func init()` in Go and `void init()` in C++; analyze; assert `search_symbols` without language filter returns both; assert `get_callers` against the Go init does NOT return the C++ init's callers (and vice versa) — verifying the `(Language, name)`-keyed SymbolIndex isolation (Phase 3 invariant at `crates/codegraph-lang/src/lib.rs:116`)
- [ ] `get_class_hierarchy` for a Go interface returns the interface as root; bases and derived are empty (no structural inheritance edges in Go); the lookup itself succeeds (Phase 2 widened root filter)
- [ ] Wire-format snapshot tests extended with Go-specific responses
- [ ] README + CLAUDE.md updated:
  - Add Go to supported languages (extension `.go`)
  - Update `crates/code-graph-mcp/src/main.rs` module-level doc comment (currently "C++ only — Phases 5/6/7 add Rust, Go, Python") to reflect Go is now live alongside C++ and Rust
  - Limitations: structural interface implementation not represented as edges (`type T struct { Embedded }` produces no `Inherits` edge); method dispatch resolved heuristically; go.mod / vendor handling is not in scope (discovery walks files and respects .gitignore; module-path resolution is not performed)

## 6.7: Structural verification

### Subtasks
- [ ] `make release` (host-target only) succeeds and produces a binary that includes the Go plugin
- [ ] `cargo fmt --check` clean across the workspace
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` green — Phase 1-6 tests all pass
- [ ] `cargo audit` clean (no new advisories)
- [ ] No new `unsafe` or `#[allow]` suppressions

## Acceptance Criteria
- [ ] GoParser implements LanguagePlugin (object-safety check passes)
- [ ] All extraction patterns working including method receiver extraction, all import forms, direct + selector_expression calls
- [ ] testdata/go passes; real-world Go project (logrus@v1.9.3) parses cleanly within recorded baseline
- [ ] Mixed C++ + Rust + Go indexing works
- [ ] Cross-language collision regression passes (`init` in C++ vs Go stays isolated)
- [ ] Watch-mode reindex regression passes (incremental reindex + dangling-edge prune)
- [ ] All Phase 1-6 tests pass; lint, format, audit gates clean
