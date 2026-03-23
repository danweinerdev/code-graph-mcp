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

### Known Limitations

1. **Function pointer typedefs** — `typedef void (*Callback)()` uses a `pointer_declarator` which is not matched by the typedef query pattern. Only simple `type_identifier` typedefs are extracted.

2. **Macro-generated definitions** — Macros like `DEFINE_HANDLER(name)` that expand to function definitions are not visible to tree-sitter (it sees the macro call, not the expansion). Macro invocations that look like function calls ARE captured as call edges.

3. **Complex template metaprogramming** — Deeply nested template specializations may produce incomplete or error-containing AST nodes. The parser skips error nodes gracefully.

4. **Call resolution is syntactic** — Call edges are based on name matching, not semantic resolution. `foo()` inside a class method and `foo()` as a free function both produce a call edge to `foo`. Scope-aware matching is deferred to the graph engine.

5. **C++ cast expressions** — `static_cast`, `dynamic_cast`, `const_cast`, `reinterpret_cast` are filtered out (tree-sitter parses them as call expressions).

6. **Forward declarations excluded** — Only `function_definition` (with body) produces symbols. Forward declarations (`void foo();`) are intentionally excluded to avoid duplicates.

7. **Template method calls** — `obj.foo<T>()` via `template_method` node type is not matched in tree-sitter-cpp v0.23.4. These calls fall through to the regular `field_expression` pattern when possible.
