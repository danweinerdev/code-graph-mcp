# CLAUDE.md

## Project: code-graph-mcp

Rust MCP server that builds an in-memory semantic code graph from source files using tree-sitter, exposing graph query tools for AI agents over stdio.

## Build

```bash
make build                                            # cargo build --release -p code-graph-mcp
make test                                             # cargo test --workspace
make lint                                             # cargo clippy --workspace --all-targets -- -D warnings
make fmt-check                                        # cargo fmt --all --check

# Or invoke cargo directly:
cargo build --release -p code-graph-mcp               # host-target release binary
cargo test --workspace                                # full workspace test suite
cargo clippy --workspace --all-targets -- -D warnings # lint
cargo fmt --check                                     # format check
```

No CGo or C toolchain required — `tree-sitter-cpp` (and the other tree-sitter grammars) link via their pure-Rust `cc`-built crates. Build natively on each platform you need the binary for; there is no cross-compile pipeline.

## Test

```bash
# Full workspace test suite (unit + integration tests)
cargo test --workspace

# Run a single crate's tests
cargo test -p codegraph-tools

# Parse-test harness (manual inspection: parse one file/dir and dump symbols+edges)
cargo run -p codegraph-parse-test -- <directory>
```

## Architecture

The workspace is split into language-agnostic core crates plus per-language plugin crates:

| Crate | Responsibility |
|-------|----------------|
| `crates/code-graph-mcp` | Binary — `rmcp`-based stdio MCP server entry point |
| `crates/codegraph-core` | Shared types (`Symbol`, `Edge`, `SymbolKind`, `EdgeKind`), `RootConfig` for `.code-graph.toml` |
| `crates/codegraph-lang` | `LanguagePlugin` trait + `LanguageRegistry` (extension → plugin dispatch) |
| `crates/codegraph-graph` | In-memory `Graph` (nodes, forward + reverse adjacency, file index), JSON cache persistence |
| `crates/codegraph-tools` | Tool handlers, parallel `analyze_codebase` discovery + indexer, watcher (notify-debouncer-full) |
| `crates/codegraph-lang-cpp` | C++ language plugin — tree-sitter-cpp queries + scope-aware call resolution |
| `crates/codegraph-lang-rust` | Rust language plugin — tree-sitter-rust queries; impl/trait extraction, use-tree expansion, macro invocation calls |
| `crates/codegraph-lang-go` | Go language plugin — tree-sitter-go queries; method-receiver extraction, all import forms (single/grouped/aliased/dot/blank), direct + selector_expression calls |

Phase 7 of the rewrite plan adds `codegraph-lang-python` (scaffolded, not wired). As of the Phase 6 cutover, **C++, Rust, and Go are live**.

```
AI Agent <-stdio/MCP-> [code-graph-mcp (rmcp server)]
                              |
                     +--------+--------+
                     |                 |
              [Tool Handlers]    [Graph]
              (codegraph-tools)  (codegraph-graph)
                     |                 |
              [LanguageRegistry] [In-memory graph + JSON cache]
              (codegraph-lang)
                     |
              [C++ Plugin] [Rust Plugin] [Go Plugin] [Python*]
              (codegraph-lang-cpp, codegraph-lang-rust, codegraph-lang-go + future Python plugin)
                     |
              [tree-sitter + tree-sitter-cpp + tree-sitter-rust + tree-sitter-go]
```
*Python plugin scaffolded but not wired as of Phase 6.*

## Configuration

Each indexed root may contain a `.code-graph.toml` controlling discovery and parsing knobs. The file is read once per `analyze_codebase` call from `<root>/.code-graph.toml` and cached on the server for subsequent watch events.

Schema (all keys optional; defaults shown):

```toml
[discovery]
# Glob patterns added to the default ignore set (.git, target, node_modules, etc.).
# Patterns follow the `ignore` crate's gitignore syntax.
extra_ignores = []

# Maximum parallel discovery threads (0 = num_cpus).
max_threads = 0

[parsing]
# Maximum parallel parse threads (0 = num_cpus). The two thread pools share
# the host concurrency budget — the indexer caps the sum at num_cpus.
max_threads = 0
```

A sample `.code-graph.toml` ships at the repo root (Task 4.5).

## MCP Tools (15 total)

The tool surface is unchanged from the Go implementation — only the implementation language changed. Tools are grouped by purpose:

**Indexing:** `analyze_codebase` (with JSON cache + mtime-based incremental re-index)
**Symbol queries:** `get_file_symbols` (with `top_level_only`/`brief`), `search_symbols` (with `namespace`, `limit`/`offset`, `brief` default true), `get_symbol_detail`, `get_symbol_summary` (counts by namespace/kind)
**Call graph:** `get_callers`, `get_callees`
**Dependencies:** `get_dependencies`
**Structural analysis:** `detect_cycles`, `get_orphans`, `get_class_hierarchy` (with `depth` for transitive walk), `get_coupling` (outgoing/incoming/both)
**Visualization:** `generate_mermaid` (call graph, file deps, or inheritance tree)
**Watch mode:** `watch_start`, `watch_stop` (auto-reindex on file changes via notify-debouncer-full)

## Code Conventions

- All tool handlers return `Result<CallToolResult, McpError>`; user-visible errors travel as `CallToolResult` with the error flag set, not `Err`
- State guards check indexed state via `ServerInner::require_indexed()` before executing query handlers
- `LanguagePlugin` trait: `extensions()`, `parse_file(path, content)`, `resolve_edges(symbols, file_graph, registry)`
- All stored file paths are absolute
- Symbol ID format: `file:name` for free functions, `file:Parent::name` for methods
- `SymbolKind` and `EdgeKind` derive `Serialize`/`Deserialize` and serialize as readable JSON strings (e.g. `"function"`, `"calls"`)
- Snapshot tests for tool wire format use `insta` (snapshots in `crates/codegraph-tools/tests/snapshots/`)

## C++ Parser Limitations

Validated against tree-sitter-cpp v0.23.4.

### Supported C++ Patterns

- Free functions, qualified methods (`Class::method`), inline methods in class bodies
- Classes, structs, enums (including `enum class`), typedefs, `using` aliases
- Function pointer typedefs (`typedef void (*Callback)(int)`)
- Operator overloads (`operator+`, `operator==`, etc.) — both in-class and free
- Auto return types (trailing `-> T` and deduced)
- Nested classes/structs (Parent field set correctly)
- Lambda call edges (calls inside and to lambdas)
- All call patterns: free, method, arrow, qualified, template

### Known Limitations

1. **Macro-generated definitions** — Macros like `DEFINE_HANDLER(name)` that expand to function definitions are not visible to tree-sitter (it sees the macro call, not the expansion). Macro invocations that look like function calls ARE captured as call edges.

2. **Complex template metaprogramming** — Deeply nested template specializations may produce incomplete or error-containing AST nodes. The parser skips error nodes gracefully.

3. **Call resolution is heuristic** — Call edges are resolved via scope-aware heuristic matching (same file > same class > same namespace > global). This is syntactic, not semantic — overloaded functions may resolve to the wrong candidate.

4. **C++ cast expressions** — `static_cast`, `dynamic_cast`, `const_cast`, `reinterpret_cast` are filtered out (tree-sitter parses them as call expressions).

5. **Forward declarations excluded** — Only `function_definition` (with body) produces symbols. Forward declarations (`void foo();`) are intentionally excluded to avoid duplicates.

6. **Template method calls** — `obj.foo<T>()` via `template_method` node type is not matched in tree-sitter-cpp v0.23.4. These calls fall through to the regular `field_expression` pattern when possible.

## Rust Parser Limitations

Validated against tree-sitter-rust v0.24.0.

### Supported Rust Patterns

- Free functions, methods inside `impl` blocks (`Type::method`), default methods inside `trait` blocks (extracted as `Function`, not `Method` — only `impl` ancestry promotes to `Method`)
- Structs, enums (all variant kinds), traits, type aliases (`type` items)
- Generics — both type-bound (`fn foo<T: Display>`) and where-clause (`fn foo<T> where T: Display`) forms
- Lifetime parameters (`fn longest<'a>(x: &'a str)`)
- `async fn`, `const fn`, `unsafe fn` — extracted as `Function` (or `Method` inside an `impl`)
- Nested modules — `mod a { mod b { fn x() {} } }` populates `Symbol.namespace = "a::b"`; `mod_item`s themselves are namespace anchors and do NOT produce Symbol records
- All `use`-tree forms expanded to dotted paths: simple, scoped, grouped, nested grouped (`use std::{io::{self, Read}, collections::HashMap}` → 3 edges), wildcard (`use foo::*`), aliased (`use foo as bar` records the path `foo`), `self`-in-list, and `extern crate alloc`
- All call patterns: direct, method via `field_expression`, scoped, turbofish, macro invocation, chained calls
- Trait impls (`impl Trait for Type`) produce `Inherits` edges from the implementing type to the trait — including generic impls (`impl<T> Trait for Vec<T>`) and impls with `where` clauses
- Trait-impl method parent disambiguation: in `impl Trait for Type { fn m() }` the method's parent is `Type`, never `Trait`. The trait identity lives only on the `Inherits` edge.

### Known Limitations

1. **`macro_rules!` definitions are not extracted as symbols.** Only macro *invocations* produce `Calls` edges. The definition queries deliberately do not match `macro_definition` nodes (the tree-sitter-rust 0.24 wrapping node for `macro_rules!` blocks). An anti-regression test in `codegraph-lang-rust` (`macro_rules_definition_produces_zero_symbols`) asserts that `macro_rules! foo { ... }` yields zero Symbol records.

2. **`#[derive(...)]` and proc-macro attributes are NOT captured as call edges.** They parse as `attribute_item` nodes, not `macro_invocation`, and the call queries only target `macro_invocation`. Multiple `#[derive(Debug, Clone, ...)]` attributes on a struct contribute zero `Calls` edges.

3. **Forward declarations excluded.** Trait method declarations without bodies (`fn bar();`) parse as `function_signature_item` and do NOT produce Symbol records — only `function_item` (which requires a body) is matched. Default methods inside trait bodies (with bodies) and methods inside `impl` blocks DO produce symbols.

4. **Call resolution is heuristic** — same as C++. Edges resolve via scope-aware heuristic matching (same file > same parent > same namespace > global). This is syntactic, not semantic — overloaded functions may resolve to the wrong candidate.

5. **Complex use trees expanded but lifetime/generic constraints not represented.** Each terminal path in a `use` tree becomes one edge; lifetime parameters and generic bounds in the surrounding code are not part of the graph. Generic impls record the type-field text verbatim — methods inside `impl<T> Trait for Vec<T>` carry parent `Vec<T>` (with the generic in the parent string), not bare `Vec`. The `Inherits` edge's `from` field follows the same rule.

## Go Parser Limitations

Validated against tree-sitter-go v0.25.0.

### Supported Go Patterns

- Free functions, methods (with receiver type as parent — both pointer `(s *T)` and value `(s T)` forms, including generic receivers `(s *T[U])` where the bare `T` is recorded as parent)
- Structs (`type T struct { ... }`), interfaces (`type T interface { ... }`), type aliases (`type ID = string`), defined types (`type Count int`, `type Handler func(...)` → `Typedef`)
- Generic functions (Go 1.18+, `func Map[T any](...)`) — type-parameter list survives in the captured signature; the bare name is recorded as the symbol name
- `init()` and `main()` are extracted as ordinary functions — no special-casing
- Package name from `package_clause` populates `Symbol.namespace` (Go packages are flat — single-level, no nested module path)
- All call patterns: direct (`foo()`), method/field selector (`obj.M()`), package-qualified (`fmt.Println()`), chained (`a.B().C()` → 2 edges, one per chain link), `go fn()`, `defer fn()`, calls inside closure literals (`func_literal`)
- All import forms via `import_spec`: single (`import "fmt"`), grouped (`import (...)`), aliased (`import f "fmt"` — alias dropped, path captured), dot (`import . "testing"`), blank (`import _ "image/png"`)
- Package-level closure fallback: a call inside a `var H = func() { foo() }` reports the file path as `from` (no enclosing function declaration), mirroring the C++ lambda-at-global-scope behavior

### Known Limitations

1. **Structural interface implementation produces no edges.** Go interfaces are satisfied structurally — a concrete type implements an interface by having the right method set, with no syntactic declaration. The parser emits zero `Inherits` edges for Go. `get_class_hierarchy` on a Go interface returns the interface as a leaf node with empty `bases` and `derived` (anti-regression test in `crates/codegraph-tools/tests/mixed_language.rs::get_class_hierarchy_for_go_interface`).

2. **Embedded struct fields produce no `Inherits` edge.** `type T struct { Bar }` is structural composition (method-set promotion at runtime), not inheritance — no edge is emitted. An anti-regression test in `codegraph-lang-go` (`embedded_struct_field_produces_no_inherits_edge`) asserts a fixture with an embedded field yields zero `Inherits` edges.

3. **Method dispatch is heuristic.** Same as the C++ and Rust plugins — call edges resolve via scope-aware heuristic matching (same file > same parent > same namespace > global). This is syntactic, not semantic; methods on different receiver types that share a name may resolve to the wrong candidate.

4. **`go.mod` and vendor directories are NOT consulted.** Discovery walks files and respects `.gitignore`; module-path resolution is out of scope. Import paths (e.g. `"github.com/sirupsen/logrus"`) are recorded verbatim in the `Includes` edge's `to` field — the default `resolve_include` basename match against the FileIndex is correctly a no-op for module paths.

5. **Generic type parameters and constraints not represented in symbol records.** Generic types are recognized in receiver positions (`func (s *Server[T]) M()` → parent recorded as `Server`, not `Server[T]`) so methods on a generic struct group with the bare type name. The type-parameter list `[T]` and any constraints (`[T any]`, `[T comparable]`) survive in the captured signature text only — they are not part of the symbol record's structured fields.

6. **`raw_string_literal` (backtick) imports are intentionally NOT matched.** Backtick-delimited import paths are valid Go grammar but not idiomatic and not produced by `gofmt`; the import query only matches `interpreted_string_literal`. An anti-regression test in `codegraph-lang-go` (`backtick_import_produces_no_includes_edge`) asserts backtick imports produce zero `Includes` edges.

7. **Forward declarations excluded.** Interface method elements (`type R interface { Read() }`) parse as `method_elem` nodes (no body) and are NOT matched by the definition query — only `method_declaration` (with a body and receiver) produces method symbols. The interface method set is implicit; only the interface type itself becomes a `Symbol`.
