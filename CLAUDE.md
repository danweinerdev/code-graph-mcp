# CLAUDE.md

## Project: code-graph-mcp

MCP server that builds an in-memory semantic code graph from source files using tree-sitter, exposing graph query tools for AI agents.

## Build

```bash
make build    # bin/<platform>/code-graph-mcp
make test     # go test -race ./...
make vet      # go vet ./...
```

Requires `CGO_ENABLED=1` (tree-sitter is a C library).

## Test

```bash
# Unit tests
go test -race ./...

# Integration tests (when available)
go test -tags integration -race ./internal/tools/ -v

# Parse test harness (manual inspection)
go run ./cmd/parse-test <directory>
```

## Architecture

```
AI Agent <-stdio/MCP-> [Go MCP Server (mcp-go)]
                              |
                     +--------+--------+
                     |                 |
              [Tool Handlers]    [Graph Engine]
              (internal/tools)   (internal/graph)
                     |                 |
              [Parser Registry]   [In-Memory Graph]
              (internal/parser)
                     |
              [C++ Parser]
              (internal/lang/cpp)
                     |
              [go-tree-sitter + tree-sitter-cpp]
```

## MCP Tools (15 total)

**Indexing:** `analyze_codebase` (with JSON cache + mtime-based incremental re-index)
**Symbol queries:** `get_file_symbols` (with `top_level_only`/`brief`), `search_symbols` (with `namespace`, `limit`/`offset`, `brief` default true), `get_symbol_detail`, `get_symbol_summary` (counts by namespace/kind)
**Call graph:** `get_callers`, `get_callees`
**Dependencies:** `get_dependencies`
**Structural analysis:** `detect_cycles`, `get_orphans`, `get_class_hierarchy` (with `depth` for transitive walk), `get_coupling` (outgoing/incoming/both)
**Visualization:** `generate_mermaid` (call graph, file deps, or inheritance tree)
**Watch mode:** `watch_start`, `watch_stop` (auto-reindex on file changes via fsnotify)

## Code Conventions

- All tool handlers return `(*mcp.CallToolResult, error)` — use `mcp.NewToolResultError()` for user errors, never return non-nil Go error
- State guards check indexed state before executing query handlers
- Parser interface: `Extensions()`, `ParseFile(path, content)`, `Close()`
- All stored file paths are absolute
- Symbol ID format: `file:name` for free functions, `file:Parent::name` for methods
- `SymbolKind` and `EdgeKind` are string types for readable JSON serialization
- Integration tests use `//go:build integration` tag

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
