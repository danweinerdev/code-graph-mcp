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
| `crates/codegraph-lang-python` | Python language plugin — tree-sitter-python queries; class/method/decorator handling, both import forms (`import` + `from … import`), multi-base inheritance, `.py` and `.pyi` |

As of the Phase 7 cutover, **all four languages — C++, Rust, Go, and Python — are live in the binary**. Cross-language collisions (e.g. an `init` symbol that exists in C++, Go, and Python) stay isolated via the `(Language, name)`-keyed `SymbolIndex` at `crates/codegraph-lang/src/lib.rs:116`.

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
              [C++ Plugin] [Rust Plugin] [Go Plugin] [Python Plugin]
              (codegraph-lang-cpp, codegraph-lang-rust, codegraph-lang-go, codegraph-lang-python)
                     |
              [tree-sitter + tree-sitter-cpp + tree-sitter-rust + tree-sitter-go + tree-sitter-python]
```

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

[cpp]
# Identifier tokens to remove from C++ source bytes before tree-sitter parses
# them. Each occurrence is whole-word matched (bordered by non-identifier
# bytes on both sides) and overwritten with spaces of the same length, so
# byte offsets, line numbers, and column numbers reported in extracted
# symbols match the original source. Default: `[]` (no rewriting).
#
# Use this for API-export macros that confuse the tree-sitter-cpp grammar
# by occupying the position between `class` and the class name. UE example:
#
#   [cpp]
#   macro_strip = ["CORE_API", "ENGINE_API", "UMG_API", "MYGAME_API"]
#
# With the list above, `class CORE_API AActor : public UObject {};` extracts
# correctly as a Class symbol with a UObject `Inherits` edge.
macro_strip = []
```

A sample `.code-graph.toml.example` ships at the repo root; copy it to `.code-graph.toml` in any indexed root and customize as needed.

### Cache invalidation

Changes to `[cpp].macro_strip` between `analyze_codebase` calls do NOT retroactively re-parse files whose mtime is unchanged (the cache uses mtime-based stale checking). To apply a new `macro_strip` list to already-indexed files, re-run `analyze_codebase` with `force=true`.

## MCP Tools (15 total)

The tool surface mirrors the Go implementation; the PaginationOverhaul plan retrofitted defensive caps and pagination envelopes for UE-scale codebases. Tools are grouped by purpose:

**Indexing:** `analyze_codebase` (with JSON cache + mtime-based incremental re-index)
**Symbol queries:** `get_file_symbols` (with `top_level_only`/`brief` + `limit`/`offset`, default limit 100, max 1000), `search_symbols` (with `namespace`, `limit`/`offset`, `brief` default true), `get_symbol_detail`, `get_symbol_summary` (counts by namespace/kind)
**Call graph:** `get_callers`, `get_callees` (both with `limit`/`offset`, default limit 100, max 1000)
**Dependencies:** `get_dependencies`
**Structural analysis:** `detect_cycles`, `get_orphans` (with `kind` + `limit`/`offset`/`brief`, default limit 20, max 1000), `get_class_hierarchy` (with `depth` for transitive walk + `max_nodes` budget, default 250, max 1000), `get_coupling` (outgoing/incoming/both)
**Visualization:** `generate_mermaid` (call graph, file deps, or inheritance tree)
**Watch mode:** `watch_start`, `watch_stop` (auto-reindex on file changes via notify-debouncer-full)

### Response shapes

List-shaped paginated tools (`get_orphans`, `get_file_symbols`, `get_callers`, `get_callees`, `search_symbols`) return the shared `Page<T>` envelope: `{ results: T[], total: u32, offset: u32, limit: u32 }`. `total` is the pre-pagination match count; `offset`/`limit` are echoed as the resolved values actually used (so silent clamp-to-1000 is visible to clients). Sort order is `symbol_id` ascending for symbol lists; `(depth, symbol_id)` ascending for `get_callers`/`get_callees` so the closest results appear on page 1. `limit = 0` is treated as "use default".

Tree-shaped `get_class_hierarchy` returns `{ hierarchy: HierarchyNode, truncated: bool, max_nodes: u32, total_nodes_seen: u32 }`. `total_nodes_seen` counts unique class names actually walked — diamond inheritance costs one budget slot per shared ancestor, not one per arm reaching it. When `truncated: true`, the partial tree is well-formed (already-recursed children stay; new children are skipped after the budget is hit) and the agent can retry with a larger `max_nodes`. Hard ceiling: `max_nodes ≤ 1000`.

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
- Macro-prefixed class declarations (`class CORE_API MyClass : public Base {};`) when listed in `[cpp].macro_strip` (see the Configuration section). Default behavior with no `[cpp]` section leaves these declarations broken, preserving zero behavior change for non-UE users — see Known Limitation 7 below for the raw-string-delimiter caveat that comes with this opt-in.

### Known Limitations

1. **Macro-generated definitions** — Macros like `DEFINE_HANDLER(name)` that expand to function definitions are not visible to tree-sitter (it sees the macro call, not the expansion). Macro invocations that look like function calls ARE captured as call edges.

2. **Complex template metaprogramming** — Deeply nested template specializations may produce incomplete or error-containing AST nodes. The parser skips error nodes gracefully.

3. **Call resolution is heuristic** — Call edges are resolved via scope-aware heuristic matching (same file > same class > same namespace > global). This is syntactic, not semantic — overloaded functions may resolve to the wrong candidate.

4. **C++ cast expressions** — `static_cast`, `dynamic_cast`, `const_cast`, `reinterpret_cast` are filtered out (tree-sitter parses them as call expressions).

5. **Forward declarations excluded** — Only `function_definition` (with body) produces symbols. Forward declarations (`void foo();`) are intentionally excluded to avoid duplicates.

6. **Template method calls** — `obj.foo<T>()` via `template_method` node type is not matched in tree-sitter-cpp v0.23.4. These calls fall through to the regular `field_expression` pattern when possible.

7. **`macro_strip` raw-string-delimiter collision** — when `[cpp].macro_strip` is configured, the C++ plugin's `preprocess` hook whole-word-replaces each listed macro with same-length spaces before tree-sitter parses. A raw string literal whose delimiter tag is also a stripped macro (e.g. `R"CORE_API(content)CORE_API"` with `CORE_API` in `macro_strip`) has both delimiters overwritten and tree-sitter fails to close the raw string — the rest of the file becomes an `ERROR` node and zero symbols extract from it. The pattern does not occur in any known codebase (a raw-string tag that is also an API-export macro is contrived) but is documented because the failure mode is silent at the file level. Workaround: drop the offending macro from `macro_strip` for the affected file or rename the raw-string tag in the source.

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

## Python Parser Limitations

Validated against tree-sitter-python v0.25.0.

### Supported Python Patterns

- Free functions, methods inside classes (parent = enclosing class name), nested classes (inner class records the *immediate* enclosing outer class as parent — not a dotted path)
- `async def` — extracted as `Function` (or `Method` inside a class) with no separate kind. tree-sitter-python 0.25 wraps `async def` as a `function_definition`, not a separate `async_function_definition` node, so a single query path covers both sync and async forms.
- `class` definitions with single (`class D(B)`), multiple (`class D(A, B, C)` → 3 `Inherits` edges), and qualified (`class D(module.Base)` → 1 edge with `to = "module.Base"`) inheritance. Keyword arguments in superclasses (`class C(metaclass=Meta)`, `class C(total=False)`) are filtered as non-bases.
- Decorators are transparent for definition extraction. `@property` / `@staticmethod` / `@classmethod` / `@abstractmethod` / custom decorators all wrap `decorated_definition > function_definition`; queries match the inner `function_definition` directly.
- All call patterns: direct (`foo()`), attribute (`obj.method()`), chained (`a.b().c()` → 2 edges, one per chain link), constructor calls (`MyClass()` — recorded as a call to `MyClass`, agent interprets as construction), `super()`, calls inside list/dict/set comprehensions, calls inside `lambda` (lambda is transparent for the enclosing-function walk), calls inside default argument expressions
- All import forms: `import foo` → `to = "foo"`; `import foo.bar` → `to = "foo.bar"`; `import foo as f` → `to = "foo"` (alias dropped); `from foo import bar` → `to = "foo"` (the *module* is the dependency, NOT the imported name); `from foo.bar import baz` → `to = "foo.bar"`; `from . import utils` → `to = ".utils"` (relative imports preserved verbatim); `from typing import List, Dict` → 1 edge with `to = "typing"`; `from __future__ import annotations` → `to = "__future__"`
- `.pyi` stub files indexed identically to `.py`. `def f(x: int) -> str: ...` (a stub with `...` body) still parses as `function_definition` and produces a Function symbol.

### Known Limitations

1. **Call resolution is especially noisy due to dynamic typing.** `PythonParser` does NOT override `resolve_call` — the default scope-aware heuristic (same file > same class > same namespace > global) is the documented contract. Python's runtime polymorphism means most call resolutions are best-effort: `obj.foo()` cannot be resolved to a concrete `foo` without type inference, which is out of scope for a tree-sitter-based static analyzer. (Rationale documented in the Phase 7.1 verification: `PythonParser` accepts the default `resolve_call` for this reason.)

2. **Decorators are transparent for definition extraction.** `@property` / `@staticmethod` / `@classmethod` produce ordinary `Method` symbols with no separate flag. `@abstractmethod` is NOT flagged as a separate kind — it parses as a method like any other. The decoration metadata is not preserved as a structured field in the symbol record.

3. **Type hints not extracted as edges.** `def f(x: SomeType) -> OtherType` does not produce edges to `SomeType` or `OtherType`. Only call sites and explicit imports drive the dependency graph.

4. **Conditional imports NOT extracted.** Patterns like `if TYPE_CHECKING: import expensive_module` are wrapped in `if_statement > block` and the import queries do not enter conditional bodies — `extract_imports`'s module-top-level guard filters them out. `try: import x except ImportError: ...` is filtered for the same reason. Anti-regression tests in the Python plugin's import tests cover both the `if`-block and `try`-block forms.

5. **`from __future__` records `__future__` as the module path.** `from __future__ import annotations` produces an `Includes` edge with `to = "__future__"`. Handled via the dedicated `future_import_statement` node kind, NOT `import_from_statement` — tree-sitter-python 0.25 tags the future-import line with its own node type.

6. **Forward declarations don't apply.** Python doesn't have C/Go-style forward declarations. `.pyi` stubs are indexed identically to `.py` files (the grammar is the same; `...` body still parses as a function body).

7. **Method dispatch is heuristic.** Same as the C++/Rust/Go plugins — call edges resolve via scope-aware heuristic matching (same file > same parent > same namespace > global). This is syntactic, not semantic; methods on different classes that share a name may resolve to the wrong candidate, especially given Python's duck-typing tradition.
