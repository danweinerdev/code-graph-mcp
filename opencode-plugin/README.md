# code-graph-opencode

OpenCode plugin for [code-graph](https://github.com/danweinerdev/code-graph-mcp) -- a semantic
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

Or build from a checkout -- `make build` puts it at `target/release/code-graph-mcp` -- and point
the plugin at it:

```bash
export CODE_GRAPH_MCP_BIN=/path/to/target/release/code-graph-mcp
```

Then index the repo once, from inside your session:

```text
analyze_codebase(path: "/path/to/your/repo")
```

The same queries are also available from a terminal via the `code-graph` CLI
(crate `code-graph-cli`, built alongside the server): 21 subcommands mirroring
the MCP tool names kebab-cased, with `--json` printing the exact MCP payload.
`code-graph analyze-codebase` indexes from a shell; when a repository daemon is
running the CLI attaches to it, otherwise it answers from the on-disk cache.

## What this registers

- **MCP server** named `code-graph`, run as a local stdio process. Exposes the full 25-tool
  surface: symbol search, call graph, call-path finding, inheritance, dependencies, coupling,
  cycles, communities, orphans, diagrams, VCS blame/history, sync + async indexing, watch mode,
  and status.
- **Skills directory** -- seven scenario skills documenting when and how to drive the toolset
  (`code-graph-navigator`, `code-graph-callgraph`, `code-graph-dependencies`,
  `code-graph-refactor-survey`, `code-graph-indexing`, `code-graph-configure`,
  `code-graph-smoke-test`).
- **Commands directory** -- `/cg`, `/cg-index`, `/cg-impact`, `/cg-deps`, `/cg-survey`,
  `/cg-status`.
- **A session-start check** that warns once if the repo has no `.code-graph-cache.db`, so the
  agent indexes before querying instead of reading an empty graph as "no results".

## Configuration

Optional `.code-graph.toml` at the project root controls discovery, parsing threads, response
byte budget, C/C++ macro handling, and per-language file extensions. Without it, built-in
defaults apply -- which for engine-style C++ (`class CORE_API Foo`) means those declarations do
not extract. See
[`.code-graph.toml.example`](https://github.com/danweinerdev/code-graph-mcp/blob/main/.code-graph.toml.example).

## License

MIT
