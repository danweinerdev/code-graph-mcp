# code-graph Claude Code plugin

Steers Claude Code toward the **code-graph MCP server** for structural code
questions instead of raw text search.

Two parts:

1. **A `PreToolUse` hook** on `Grep`/`Glob` that injects a one-time,
   non-blocking nudge when the search looks like a symbol query — pointing the
   model at the right code-graph tool (`search_symbols`, `get_callers`, …).
   It **never blocks**: grep/glob still run, and free-text/regex searches are
   left alone.
2. **Five skills** that teach the model when and how to use the code-graph API:

   | Skill | Covers |
   |---|---|
   | `code-graph-navigator` | Find/inspect symbols (search_symbols, get_file_symbols, get_symbol_detail/summary, find_class_candidates) |
   | `code-graph-callgraph` | Callers/callees, overrides, inheritance (get_callers/callees, find_overrides, get_class_hierarchy) |
   | `code-graph-dependencies` | Imports, coupling, cycles, diagrams (get_dependencies, get_coupling, detect_cycles, generate_diagram) |
   | `code-graph-refactor-survey` | Dead code, blast radius, codebase orientation (get_orphans + workflows) |
   | `code-graph-indexing` | analyze_codebase (sync/async), watch mode, scoping, caching, config |

## Requirements

The `code-graph` MCP server must be available to Claude Code. In this repo's dev
container it's baked in and wired via `/etc/claude-code/managed-mcp.json`
(`tools/claude/`). On a host, register it in your MCP config, e.g.:

```json
{ "mcpServers": { "code-graph": { "type": "stdio",
  "command": "/path/to/code-graph-mcp", "args": [] } } }
```

If the server is registered under a name other than `code-graph`, the
`mcp__code-graph__*` tool names in the skills/nudge won't resolve — keep the
server name `code-graph`.

## Enable it

Load the plugin directory directly — this activates its hooks and skills for the
session with no install step and no writes under `~/.claude`:

```bash
claude --plugin-dir ./plugin
```

In the dev container this is automatic: the image bakes the plugin at
`/opt/code-graph-plugin/plugin` and the launcher's default command passes
`--plugin-dir /opt/code-graph-plugin/plugin`, so `./claude-container.sh` starts
with the plugin loaded (`claude plugin list` → `Status: ✔ loaded`).

Alternatively, install it via the marketplace manifest at
`.claude-plugin/marketplace.json` (interactive `/plugin` menu, or
`claude plugin marketplace add <repo-root>` + `claude plugin install
code-graph@code-graph-mcp`). Note: managed-settings `enabledPlugins` /
`extraKnownMarketplaces` alone do **not** auto-install a directory-sourced
plugin headlessly — they only declare intent — so the container uses
`--plugin-dir`, which actually loads it.

## Behavior notes

- The nudge fires at most once per `(session, tool)` per cooldown
  (`CODE_GRAPH_NUDGE_COOLDOWN`, default 900s).
- A `SessionStart` hook resets the throttle on `startup`/`resume`/`clear`/
  `compact`, so after `/clear` or a context compaction (which wipe the model's
  memory of the earlier nudge) the guidance re-injects on the next search.
- Both hook scripts fail open: any error path exits 0 with no output, so a
  broken hook can never wedge a search or session start.
