# code-graph-mcp

An MCP server that builds a semantic code graph from C/C++ source files using [tree-sitter](https://tree-sitter.github.io/), enabling AI agents to query callers, callees, dependencies, class hierarchies, and more — in real time, without exhaustive file searching.

## Installation

```bash
git clone https://github.com/danweinerdev/code-graph-mcp.git
cd code-graph-mcp
make build
```

Requires Go 1.25+ and a C compiler (CGo is needed for tree-sitter).

The binary is built to `bin/<platform>/code-graph-mcp`.

## MCP Client Configuration

### Claude Desktop

Add to `~/Library/Application Support/Claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "code-graph": {
      "command": "/path/to/bin/linux-amd64/code-graph-mcp"
    }
  }
}
```

### Claude Code

Add to your project's `.claude/settings.json`:

```json
{
  "mcpServers": {
    "code-graph": {
      "command": "/path/to/bin/linux-amd64/code-graph-mcp"
    }
  }
}
```

## Tools

### Indexing

| Tool | Description | Parameters |
|------|-------------|------------|
| `analyze_codebase` | Index a directory and build the code graph | `path` (required): directory path; `force` (optional): skip cache |

### Symbol Queries

| Tool | Description | Parameters |
|------|-------------|------------|
| `get_file_symbols` | List symbols defined in a file | `file`: absolute file path |
| `search_symbols` | Search symbols by name pattern | `query`: substring/regex; `kind` (optional): filter |
| `get_symbol_detail` | Get full info for a symbol | `symbol`: symbol ID (`file:name`) |

### Call Graph

| Tool | Description | Parameters |
|------|-------------|------------|
| `get_callers` | Find upstream callers | `symbol`: symbol ID; `depth` (optional) |
| `get_callees` | Find downstream callees | `symbol`: symbol ID; `depth` (optional) |

### Dependencies

| Tool | Description | Parameters |
|------|-------------|------------|
| `get_dependencies` | List included files | `file`: absolute file path |

### Structural Analysis

| Tool | Description | Parameters |
|------|-------------|------------|
| `detect_cycles` | Find circular include dependencies | (none) |
| `get_orphans` | Find uncalled functions/methods | `kind` (optional): filter |
| `get_class_hierarchy` | Get inheritance tree | `class`: class name |
| `get_coupling` | Cross-file dependency counts | `file`: absolute file path |

## Workflow

1. Call `analyze_codebase` with the project root to index the codebase
2. Use `search_symbols` or `get_file_symbols` to find symbols of interest
3. Use `get_callers`/`get_callees` to navigate the call graph
4. Use `get_dependencies` and `detect_cycles` to understand file structure
5. Use `get_class_hierarchy` to explore inheritance

Symbol IDs are in the format `file:name` (e.g., `/path/engine.cpp:Engine::update`) and are returned by symbol query tools for use in call graph queries.

## Supported Languages

- **C/C++** (`.cpp`, `.cc`, `.cxx`, `.c`, `.h`, `.hpp`, `.hxx`) via tree-sitter-cpp v0.23.4

The parser interface is pluggable — additional languages can be added by implementing the `Parser` interface.

## Limitations

See [CLAUDE.md](CLAUDE.md#c-parser-limitations) for details on C++ parser limitations including function pointer typedefs, macro-generated definitions, and template metaprogramming edge cases.
