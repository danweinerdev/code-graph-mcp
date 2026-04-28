# code-graph-mcp

An MCP server that builds a semantic code graph from source files using [tree-sitter](https://tree-sitter.github.io/), enabling AI agents to query callers, callees, dependencies, class hierarchies, and more — in real time, without exhaustive file searching.

Instead of an agent spending thousands of tokens grepping through files to understand how code is connected, it calls `analyze_codebase` once, then uses targeted queries like `get_callers` or `get_class_hierarchy` to navigate the codebase instantly.

## Installation

```bash
git clone https://github.com/danweinerdev/code-graph-mcp.git
cd code-graph-mcp
make build
```

Requires Go 1.25+ and a C compiler (`CGO_ENABLED=1` is needed for tree-sitter).

The binary is built to `bin/<platform>/code-graph-mcp` (e.g., `bin/linux-amd64/code-graph-mcp`).

## Quick Start

### Claude Code

Add to your project's `.claude/settings.json`:

```json
{
  "mcpServers": {
    "code-graph": {
      "command": "/absolute/path/to/bin/linux-amd64/code-graph-mcp"
    }
  }
}
```

### Claude Desktop

Add to `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "code-graph": {
      "command": "/absolute/path/to/bin/darwin-arm64/code-graph-mcp"
    }
  }
}
```

### Any MCP Client

The server communicates over stdio using the [Model Context Protocol](https://modelcontextprotocol.io/). Point any MCP-compatible client at the binary.

## How It Works

```
AI Agent ←─ stdio/MCP ─→ code-graph-mcp
                              │
                    ┌─────────┴─────────┐
                    │                   │
              Tool Handlers       Graph Engine
              (15 tools)         (in-memory graph)
                    │                   │
              Parser Registry     Nodes + Edges
                    │             + Algorithms
              ┌─────┴─────┐
              │           │
          C++ Parser   Future Parsers
         (tree-sitter)  (Go, Rust, Python)
```

1. **Index** — `analyze_codebase` walks a directory, parses source files with tree-sitter, extracts symbols (functions, classes, etc.) and relationships (calls, includes, inheritance), resolves names, and builds an in-memory directed graph.

2. **Query** — 14 query/visualization tools let the agent explore the graph: find callers, trace call chains, detect cycles, find orphaned code, visualize dependencies.

3. **Watch** — Optional file watcher auto-reindexes changed files in real time.

## Tools

### Indexing

| Tool | Description | Parameters |
|------|-------------|------------|
| `analyze_codebase` | Index a directory and build the code graph | `path` (required): directory to index; `force` (optional): bypass cache |

The index is cached to `.code-graph-cache.json` in the indexed directory. On subsequent calls, if no files have changed (by mtime), the cache is loaded instantly. Use `force=true` to re-index from scratch.

### Symbol Queries

| Tool | Description | Parameters |
|------|-------------|------------|
| `get_file_symbols` | List all symbols defined in a file | `file`: absolute file path; `top_level_only` (optional): exclude nested methods/types; `brief` (optional): omit signature/column/end_line |
| `search_symbols` | Search symbols by name pattern (paginated, brief by default) | `query` / `kind` / `namespace` (at least one required); `limit` (default 20); `offset` (default 0); `brief` (default true) |
| `get_symbol_detail` | Get full details for a symbol | `symbol`: symbol ID (`file:name`) |
| `get_symbol_summary` | Count symbols grouped by namespace and kind | (none) |

Symbol IDs are in the format `file:name` for free functions or `file:Parent::name` for methods (e.g., `/path/engine.cpp:Engine::update`). They are returned by `get_file_symbols` and `search_symbols` for use in other queries.

`search_symbols` returns a paginated envelope: `{"results": [...], "total": N, "offset": X, "limit": Y}`. In `brief` mode (default), each result contains `id`, `name`, `kind`, `file`, `line`, `namespace`, `parent` — call `get_symbol_detail` for the full signature and source span.

### Call Graph

| Tool | Description | Parameters |
|------|-------------|------------|
| `get_callers` | Find functions that call a given symbol (upstream) | `symbol`: symbol ID; `depth` (optional, default 1) |
| `get_callees` | Find functions called by a given symbol (downstream) | `symbol`: symbol ID; `depth` (optional, default 1) |

Depth > 1 traces transitive callers/callees via BFS. Cycles are handled safely.

### Dependencies

| Tool | Description | Parameters |
|------|-------------|------------|
| `get_dependencies` | List files included/imported by a file | `file`: absolute file path |

### Structural Analysis

| Tool | Description | Parameters |
|------|-------------|------------|
| `detect_cycles` | Find circular include/import dependencies | (none) |
| `get_orphans` | Find symbols with no callers | `kind` (optional): filter by symbol kind |
| `get_class_hierarchy` | Get inheritance tree (bases + derived) | `class`: class name; `depth` (optional, default 1) |
| `get_coupling` | Count cross-file dependencies | `file`: absolute file path; `direction` (optional): `outgoing`, `incoming`, or `both` |

### Visualization

| Tool | Description | Parameters |
|------|-------------|------------|
| `generate_mermaid` | Generate a Mermaid diagram | `symbol`: call graph; `file`: dependency graph; `class`: inheritance tree; `depth` (optional); `max_nodes` (optional, default 30) |

Returns a Mermaid flowchart string that can be rendered by any Mermaid-compatible viewer. The center node is highlighted. Provide exactly one of `symbol`, `file`, or `class`.

### Watch Mode

| Tool | Description | Parameters |
|------|-------------|------------|
| `watch_start` | Start auto-reindexing on file changes | (none) |
| `watch_stop` | Stop file watching | (none) |

Uses [fsnotify](https://github.com/fsnotify/fsnotify) to monitor the indexed directory. Changed files are re-parsed and merged into the graph automatically.

## Example Workflow

```
Agent: analyze_codebase(path="/home/user/myproject")
→ {"files": 42, "symbols": 380, "edges": 1200}

Agent: search_symbols(query="Engine", kind="class")
→ [{"id": "/home/user/myproject/engine.h:Engine", "kind": "class", ...}]

Agent: get_callees(symbol="/home/user/myproject/engine.cpp:Engine::update", depth=2)
→ [{"symbol_id": "...:Physics::step", "depth": 1}, {"symbol_id": "...:Vec3::normalize", "depth": 2}]

Agent: generate_mermaid(class="Engine")
→ graph BT
    c0["Engine"]:::center
    c1["GameObject"]
    c2["PhysicsEngine"]
    c0 -->|inherits| c1
    c2 -->|inherits| c0
    classDef center fill:#f96,stroke:#333
```

## Supported Languages

| Language | Extensions | Parser |
|----------|-----------|--------|
| C/C++ | `.cpp`, `.cc`, `.cxx`, `.c`, `.h`, `.hpp`, `.hxx` | tree-sitter-cpp v0.23.4 |

The parser interface is pluggable. Go, Python, and Rust parsers are planned — see `.plans/Plans/` for implementation details.

### C++ Parser Coverage

**Supported patterns:** Free functions, qualified methods, inline methods, operator overloads, classes, structs, enums (including `enum class`), typedefs, `using` aliases, function pointer typedefs, nested classes, namespaces, all call patterns (direct, method, arrow, qualified, template), `#include` directives, single/multiple/qualified inheritance, lambda call edges.

**Known limitations:** Macro-generated definitions (tree-sitter sees the macro call, not the expansion), complex template metaprogramming, template method calls (`obj.foo<T>()`), and call resolution is heuristic-based (same file > same class > same namespace > global).

See [CLAUDE.md](CLAUDE.md) for the full list.

## Building

```bash
make build          # Build for current platform → bin/<os>-<arch>/code-graph-mcp
make build-all      # Build for all platforms (requires cross-compilers for non-host)
make test           # Run all tests with race detector
make test-integration  # Run integration tests
make vet            # Run go vet
make clean          # Remove build artifacts
```

Cross-compilation requires platform-specific C toolchains since tree-sitter uses CGo. The Makefile auto-detects the host platform and skips targets whose cross-compiler is not found.

## Architecture

```
cmd/code-graph-mcp/     Entry point — MCP server setup
cmd/parse-test/         CLI tool for manual parser inspection

internal/
  parser/               Parser interface, types (Symbol, Edge, FileGraph), Registry
  graph/                In-memory directed graph, BFS, Tarjan's SCC, Mermaid generation
  lang/cpp/             C++ parser (tree-sitter queries + extraction)
  tools/                MCP tool handlers, analyze_codebase worker pool, name resolution
```

## License

See [LICENSE](LICENSE).
