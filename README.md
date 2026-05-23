# code-graph-mcp

An MCP server that builds an in-memory semantic code graph from C++, Rust, Go, Python, C#, and Java source files using [tree-sitter](https://tree-sitter.github.io/), exposing 15 query tools to AI agents over stdio. Instead of an agent burning tokens grepping files to understand how code is connected, it calls `analyze_codebase` once and then issues targeted queries like `get_callers`, `get_class_hierarchy`, or `generate_diagram` to navigate the codebase instantly.

## Supported languages

| Language | Extensions | Plugin crate |
|----------|------------|--------------|
| C++      | `.cpp`, `.cc`, `.cxx`, `.c`, `.h`, `.hpp`, `.hxx` | `code-graph-lang-cpp` |
| Rust     | `.rs`      | `code-graph-lang-rust` |
| Go       | `.go`      | `code-graph-lang-go` |
| Python   | `.py`, `.pyi` | `code-graph-lang-python` |
| C#       | `.cs`      | `code-graph-lang-csharp` |
| Java     | `.java`    | `code-graph-lang-java` |

## Installation

Build from source on whichever platform you need the binary for — there is no cross-compile pipeline and no prebuilt binaries are published.

### Via `cargo install`

```bash
git clone https://github.com/danweinerdev/code-graph-mcp.git
cd code-graph-mcp
cargo install --path crates/code-graph-mcp
```

This installs `code-graph-mcp` to `~/.cargo/bin/` (which should already be on your PATH if you have a working Rust toolchain).

### Via `make release`

```bash
git clone https://github.com/danweinerdev/code-graph-mcp.git
cd code-graph-mcp
make release              # cargo build --release -p code-graph-mcp
```

The binary lands at `target/release/code-graph-mcp`. Symlink or copy it into your PATH:

```bash
ln -s "$(pwd)/target/release/code-graph-mcp" ~/.local/bin/code-graph-mcp
```

No CGo or C toolchain required — the tree-sitter grammars link via their pure-Rust `cc`-built crates.

## MCP client configuration

Register the binary as an MCP server in your client. For Claude Desktop, edit `~/Library/Application Support/Claude/claude_desktop_config.json` (macOS) or the platform equivalent:

```json
{
  "mcpServers": {
    "code-graph": {
      "command": "/path/to/code-graph-mcp"
    }
  }
}
```

For Claude Code, add the equivalent block to your project's `.claude/settings.json`. Any MCP-compatible client should work — the server speaks [Model Context Protocol](https://modelcontextprotocol.io/) over stdio with no CLI flags.

## Configuration

Place a `.code-graph.toml` at your project root. `analyze_codebase` walks upward from the path you invoke it with, looking for the nearest `.code-graph.toml` — the same convention cargo, git, rustfmt, and editorconfig use. Whichever directory contains that file becomes the **project root**: the discovered config applies, the project-wide cache lives there, and scoped invocations (`analyze_codebase` against a subdirectory) accumulate into the same cache. See [`.code-graph.toml.example`](.code-graph.toml.example) at the repo root for the documented schema.

Schema (all keys optional; defaults shown):

```toml
[discovery]
max_threads = 0           # 0 = auto (num CPUs); over-cap values are clamped
respect_gitignore = true  # honor .gitignore / .ignore / global ignore files
follow_symlinks = false   # follow symlinks during discovery
extra_ignore = []         # additional gitignore-style globs to exclude

[parsing]
max_threads = 0           # 0 = auto; same clamping rule as discovery
```

The discovery walk stops at the first `.code-graph.toml` it finds (no merging across nested files — a `.code-graph.toml` inside a subdir of a configured project marks that subdir as its own project). If no toml exists between the invocation path and the filesystem root, built-in defaults apply and `analyze_codebase` surfaces a warning naming the consequence (engine-style classes prefixed with API-export macros will not extract — see the `[cpp].macro_strip` section of the example config). Malformed TOML → `analyze_codebase` fails with a parse error (no silent fallback).

## Tools

The server exposes 15 tools. Descriptions are copied verbatim from the `#[tool(description = "...")]` attributes in `crates/code-graph-tools/src/server.rs` (the source of truth).

### Indexing

| Tool | Description |
|------|-------------|
| `analyze_codebase` | Index a codebase (C/C++, Rust, Go, Python) and build the code graph. Must be called before any query tools. |

The index is cached to `.code-graph-cache.json` in the indexed directory. On subsequent calls, files unchanged by mtime are loaded from cache; only modified files are re-parsed. Use `force=true` to re-index from scratch.

### Symbol queries

| Tool | Description |
|------|-------------|
| `get_file_symbols` | List all symbols (functions, classes, etc.) defined in a file. Returns paginated results in the `{results, total, offset, limit}` envelope. Default `limit` 100 (max 1000); pass `limit`/`offset` to page through large files. |
| `search_symbols` | Search for symbols by name pattern across the indexed codebase. Returns paginated results. Default brief mode omits signatures for token efficiency. |
| `get_symbol_detail` | Get full details for a symbol by its ID |
| `get_symbol_summary` | Get symbol counts grouped by namespace and kind — useful for codebase orientation |

Symbol IDs use the format `file:name` for free functions and `file:Parent::name` for methods (e.g., `/path/engine.cpp:Engine::update`).

### Call graph

| Tool | Description |
|------|-------------|
| `get_callers` | Find functions that call the given symbol (upstream call chain). Returns paginated results in the `{results, total, offset, limit}` envelope, sorted by `(depth, symbol_id)` ascending so the closest callers appear first. Default `limit` 100 (max 1000). |
| `get_callees` | Find functions called by the given symbol (downstream call chain). Returns paginated results in the same envelope, sorted by `(depth, symbol_id)`. Default `limit` 100 (max 1000). |

### Dependencies

| Tool | Description |
|------|-------------|
| `get_dependencies` | List files included/imported by the given file |

### Structural analysis

| Tool | Description |
|------|-------------|
| `detect_cycles` | Detect circular include dependencies in the indexed codebase |
| `get_orphans` | Find symbols with no incoming call edges (uncalled functions/methods). Returns paginated results in the `{results, total, offset, limit}` envelope. Default `limit` 20 (max 1000); `brief` defaults to true. |
| `get_class_hierarchy` | Get the inheritance tree for a class. Returns `{hierarchy, truncated, max_nodes, total_nodes_seen}`: `hierarchy` is the tree, `truncated` flags whether the budget cut children, `total_nodes_seen` is the unique-name count actually walked. Default `max_nodes` 250 (max 1000). Diamond inheritance counts shared ancestors once. |
| `get_coupling` | Get cross-file dependency counts for a file |

### Visualization

| Tool | Description |
|------|-------------|
| `generate_diagram` | Generate a graph diagram: call graph (symbol), file dependencies (file), or inheritance tree (class). Returns edges as JSON by default, or Mermaid syntax when format=mermaid. |

### Watch mode

| Tool | Description |
|------|-------------|
| `watch_start` | Start watching the indexed directory for file changes and auto-reindex modified files |
| `watch_stop` | Stop watching for file changes |

Watch mode uses [notify-debouncer-full](https://docs.rs/notify-debouncer-full) with a 250ms debounce window. Re-indexing is index-lock-aware: if `analyze_codebase` is in flight, the event is dropped (the in-flight analyze will pick up the file's current state anyway).

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

- Free functions, methods inside `impl` blocks (`Type::method`), default methods inside `trait` blocks
- Structs, enums (all variant kinds), traits, type aliases (`type` items)
- Generics — both type-bound (`fn foo<T: Display>`) and where-clause (`fn foo<T> where T: Display`) forms
- Lifetime parameters (`fn longest<'a>(x: &'a str)`)
- `async fn`, `const fn`, `unsafe fn` — all extracted as `Function` (or `Method` inside an `impl`)
- Nested modules — `mod a { mod b { fn x() {} } }` populates `Symbol.namespace = "a::b"`
- All `use`-tree forms expanded to dotted paths: simple, scoped, grouped (`use foo::{a, b}`), nested grouped (`use std::{io::{self, Read}, collections::HashMap}`), wildcard (`use foo::*`), aliased (`use foo as bar` records `foo`), `self`-in-list, and `extern crate alloc`
- All call patterns: direct (`foo()`), method via `field_expression` (`obj.foo()`), scoped (`foo::bar::baz()`), turbofish (`foo::<u32>()`), macro invocation (`println!()`), chained calls
- Trait impls (`impl Trait for Type`) produce `Inherits` edges from the implementing type to the trait — including generic impls (`impl<T> Trait for Vec<T>`) and impls with `where` clauses
- Closure bodies — calls inside `|| foo()` report the enclosing function as `from`

### Known Limitations

1. **`macro_rules!` definitions are not extracted as symbols.** Only macro *invocations* produce `Calls` edges. The definition queries deliberately do not match `macro_definition` nodes (the tree-sitter-rust 0.24 wrapping node for `macro_rules!` blocks). An anti-regression test in `code-graph-lang-rust` asserts that `macro_rules! foo { ... }` yields zero Symbol records.

2. **`#[derive(...)]` and other proc-macro attributes are NOT captured as call edges.** They parse as `attribute_item` nodes, not `macro_invocation`, and the call queries only target `macro_invocation`. Multiple `#[derive(Debug, Clone, ...)]` attributes on a struct contribute zero `Calls` edges.

3. **Forward declarations excluded.** Trait method declarations without bodies (`fn bar();`) parse as `function_signature_item` and do NOT produce Symbol records — only `function_item` (which requires a body) is matched. Default methods inside trait bodies (with bodies) and methods inside `impl` blocks DO produce symbols.

4. **Call resolution is heuristic** — same as C++. Edges resolve via scope-aware heuristic matching (same file > same parent > same namespace > global). This is syntactic, not semantic.

5. **Complex use trees expanded but lifetime/generic constraints not represented.** Each terminal path in a `use` tree becomes one edge; lifetime parameters and generic bounds in the surrounding code are not part of the graph. Generic impls record the type-field text verbatim — the parent of methods in `impl<T> Trait for Vec<T>` is `Vec<T>` (with the generic in the parent string), not bare `Vec`.

## Go Parser Limitations

Validated against tree-sitter-go v0.25.0.

### Supported Go Patterns

- Free functions, methods (with receiver type as parent — both pointer `(s *T)` and value `(s T)` forms, including generic receivers `(s *T[U])`)
- Structs (`type T struct { ... }`), interfaces (`type T interface { ... }`), type aliases (`type ID = string`), defined types (`type Count int`, `type Handler func(...)`)
- Generic functions (Go 1.18+, `func Map[T any](...)`) — type parameters preserved in the captured signature
- `init()` and `main()` are extracted as ordinary functions (no special-casing)
- Package name from `package_clause` populates `Symbol.namespace` (Go packages are flat — single-level)
- All call patterns: direct (`foo()`), method/field selector (`obj.M()`), package-qualified (`fmt.Println()`), chained (`a.B().C()` → 2 edges), `go fn()`, `defer fn()`, calls inside closure literals (`func_literal`)
- All import forms: single (`import "fmt"`), grouped (`import (...)`), aliased (`import f "fmt"` — alias dropped, path captured), dot (`import . "testing"`), blank (`import _ "image/png"`)
- Package-level closure fallback: a call inside a `var H = func() { foo() }` reports the file path as `from` (no enclosing function declaration)

### Known Limitations

1. **Structural interface implementation produces no edges.** Go interfaces are satisfied structurally — a concrete type implements an interface by having the right method set, with no syntactic declaration. The parser emits zero `Inherits` edges for Go. `get_class_hierarchy` on a Go interface returns the interface as a leaf node with empty `bases` and `derived`.

2. **Embedded struct fields produce no `Inherits` edge.** `type T struct { Bar }` is structural composition (method-set promotion), not inheritance — no edge is emitted. An anti-regression test in `code-graph-lang-go` asserts a fixture with an embedded field yields zero `Inherits` edges.

3. **Method dispatch is heuristic.** Same as the C++ and Rust plugins — call edges resolve via scope-aware heuristic matching (same file > same parent > same namespace > global). This is syntactic, not semantic; methods on different receiver types that share a name may resolve to the wrong candidate.

4. **`go.mod` and vendor directories are not consulted.** Discovery walks files and respects `.gitignore`; module-path resolution is out of scope. Import paths (e.g. `"github.com/sirupsen/logrus"`) are recorded verbatim in the `Includes` edge's `to` field — the default `resolve_include` basename match against the FileIndex is correctly a no-op for module paths.

5. **Generic type parameters and constraints not represented in symbol records.** Generic types are recognized in receiver positions (`func (s *Server[T]) M()` → parent `Server`), but the type-parameter list `[T]` and any constraints (`[T any]`, `[T comparable]`) are not part of the symbol record. They survive in the captured signature text only.

6. **`raw_string_literal` (backtick) imports are intentionally not matched.** Backtick-delimited import paths are valid Go grammar but not idiomatic and not produced by `gofmt`; the import query only matches `interpreted_string_literal`. An anti-regression test in `code-graph-lang-go` asserts backtick imports produce zero `Includes` edges.

## Python Parser Limitations

Validated against tree-sitter-python v0.25.0.

### Supported Python Patterns

- Free functions, methods inside classes (`Class::method`), nested classes (inner class records the immediate-enclosing outer class as parent)
- `async def` — extracted as `Function` (or `Method` inside a class), no separate kind. `async def` parses as `function_definition` in tree-sitter-python 0.25.
- `class` definitions — single, multiple (`class D(A, B)`), and qualified (`class D(module.Base)`) inheritance all produce `Inherits` edges
- Decorators are transparent for definition extraction. `@property`, `@staticmethod`, `@classmethod`, `@abstractmethod`, custom decorators — all wrap `decorated_definition > function_definition` and the queries match the inner `function_definition` directly. The decoration metadata is not preserved as a separate flag.
- All call patterns: direct (`foo()`), attribute (`obj.method()`), chained (`a.b().c()` → 2 edges), constructor calls (`MyClass()` — recorded as a call to `MyClass`), `super()`, calls inside list/dict/set comprehensions, calls inside lambdas (lambda is transparent for the enclosing-function walk), calls inside default arguments
- All import forms: `import foo`, `import foo.bar`, `import foo as f` (alias dropped, dotted path captured), `from foo import bar` (records `foo`, NOT `bar` — the module is the dependency), `from foo.bar import baz`, `from . import utils` (records `.utils`), `from __future__ import annotations` (records `__future__`)
- `.pyi` stub files indexed identically to `.py` files. `def f() -> int: ...` parses as a `function_definition` and produces a Function symbol; class stubs with method stubs produce Class + Method symbols.

### Known Limitations

1. **Call resolution is especially noisy due to dynamic typing.** `PythonParser` does not override `resolve_call` — the default scope-aware heuristic (same file > same class > same namespace > global) is the documented contract. Python's runtime polymorphism means most call resolutions are best-effort: `obj.foo()` cannot be resolved to a concrete `foo` without type inference, which is out of scope for a tree-sitter-based static analyzer.

2. **Decorators are transparent for definition extraction.** `@property` / `@staticmethod` / `@classmethod` produce ordinary `Method` symbols with no separate flag. `@abstractmethod` is NOT flagged as a separate kind — it parses as a method like any other. The decorator type is not part of the symbol record.

3. **Type hints not extracted as edges.** `def f(x: SomeType) -> OtherType` does not produce `Includes`/`Calls` edges to `SomeType` or `OtherType`. Only call sites and explicit imports drive the dependency graph.

4. **Conditional imports NOT extracted.** Patterns like `if TYPE_CHECKING: import expensive_module` are wrapped in `if_statement > block` and the import queries do not enter conditional bodies — module-top-level guard in `extract_imports` filters them out. `try: import x except ImportError: ...` is filtered for the same reason. Anti-regression tests in the Python plugin's import tests cover both forms.

5. **`from __future__` records `__future__` as the module path.** `from __future__ import annotations` produces an `Includes` edge with `to = "__future__"`. The dunder module is handled via the dedicated `future_import_statement` node kind, NOT via `import_from_statement`.

6. **Forward declarations don't apply.** Python doesn't have C-style forward declarations. `.pyi` stubs are indexed identically to `.py` files (the grammar is the same; `...` body still parses as a function body).

7. **Method dispatch is heuristic.** Same as the C++/Rust/Go plugins — call edges resolve via scope-aware heuristic matching (same file > same parent > same namespace > global). This is syntactic, not semantic.

## Smoke test

Watch-mode and incremental-cache behavior have automated coverage in `crates/code-graph-tools/tests/watch_race.rs` and `crates/code-graph-tools/tests/watch_dangling_edges.rs`. For end-to-end validation against an MCP client, see [`docs/SMOKE_TEST.md`](docs/SMOKE_TEST.md).

## License

See [LICENSE](LICENSE).
