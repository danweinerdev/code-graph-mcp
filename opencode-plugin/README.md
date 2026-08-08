# code-graph-opencode

OpenCode plugin for [code-graph](https://github.com/danweinerdev/code-graph-mcp) — a semantic
code-graph MCP server built on tree-sitter, with resolved call, include, and inheritance edges
across C++, Rust, Go, Python, C#, and Java.

## Install

Add to your `opencode.json` (global or project-level):

```json
{
  "plugin": ["code-graph-opencode@latest"]
}
```

Restart OpenCode. You also need the `code-graph-mcp` binary on your `PATH`:

```bash
cargo install --git https://github.com/danweinerdev/code-graph-mcp code-graph-mcp
```

Or build from a checkout — `make build` puts it at `target/release/code-graph-mcp` — and point
the plugin at it:

```bash
export CODE_GRAPH_MCP_BIN=/path/to/target/release/code-graph-mcp
```

Then index the repo once, from inside your session:

```text
analyze_codebase(path: "/path/to/your/repo")
```

There is no CLI — `code-graph-mcp` is a pure stdio MCP server, so indexing is a tool call, not a
shell command.

## What this registers

- **MCP server** named `code-graph`, run as a local stdio process. Exposes the full 19-tool
  surface: symbol search, call graph, inheritance, dependencies, coupling, cycles, orphans,
  diagrams, watch mode, and status.
- **Skills directory** — five scenario skills documenting when and how to drive the toolset
  (`code-graph-navigator`, `code-graph-callgraph`, `code-graph-dependencies`,
  `code-graph-refactor-survey`, `code-graph-indexing`).
- **Commands directory** — `/cg`, `/cg-index`, `/cg-impact`, `/cg-deps`, `/cg-survey`,
  `/cg-status`.
- **A session-start check** that warns once if the repo has no `.code-graph-cache.db`, so the
  agent indexes before querying instead of reading an empty graph as "no results".

## Configuration

Optional `.code-graph.toml` at the project root controls discovery, parsing threads, response
byte budget, C/C++ macro handling, and per-language file extensions. Without it, built-in
defaults apply — which for engine-style C++ (`class CORE_API Foo`) means those declarations do
not extract. See
[`.code-graph.toml.example`](https://github.com/danweinerdev/code-graph-mcp/blob/main/.code-graph.toml.example).

## License

MIT
