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

Phases 6/7 of the rewrite plan add `codegraph-lang-go` and `codegraph-lang-python` (scaffolded, not wired). As of the Phase 5 cutover, **C++ and Rust are live**.

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
              [C++ Plugin] [Rust Plugin] [Go*] [Python*]
              (codegraph-lang-cpp, codegraph-lang-rust + future plugins)
                     |
              [tree-sitter + tree-sitter-cpp + tree-sitter-rust]
```
*Go/Python plugins scaffolded but not wired as of Phase 5.*

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
