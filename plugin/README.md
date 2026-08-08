# code-graph Claude Code plugin

Steers Claude Code toward the **code-graph MCP server** for structural code
questions instead of raw text search.

This directory is also the **canonical source** for the Codex and OpenCode
plugin trees — see [Other harnesses](#other-harnesses) below.

Three parts:

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

3. **Six slash commands** for driving the toolset directly:

   | Command | Does |
   |---|---|
   | `/cg <question>` | Routes any structural question to the right tool |
   | `/cg-index [path] [force]` | analyze_codebase, sync or async, scoped or whole-tree |
   | `/cg-impact <symbol>` | Blast radius — callers, callees, overrides, hierarchy |
   | `/cg-deps [file]` | Dependencies, coupling, cycles, diagrams |
   | `/cg-survey [subtree]` | Structural health report — shape, dead code, cycles, hotspots |
   | `/cg-status` | Server + index diagnostics, async job progress |

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

## Other harnesses

The same skills and commands ship to Codex and OpenCode. **`plugin/` is the
single source of truth** — the other trees are generated:

| Tree | Harness | Contains |
|---|---|---|
| `plugin/` | Claude Code | canonical skills, commands, hooks, scripts |
| `.codex-plugin/` | Codex | `plugin.json`, generated skills + commands, `hooks/` |
| `opencode-plugin/` | OpenCode | npm package (`code-graph-opencode`), generated skills + commands |

Author in `plugin/`, then fan out:

```bash
make plugin-sync         # regenerate the mirrors
make plugin-sync-check   # fail if a mirror is stale (also part of `make verify`)
```

Never hand-edit a mirror — the next sync overwrites it. The one exception is
each mirror's own manifest (`.codex-plugin/plugin.json`,
`.codex-plugin/hooks/hooks.json`, `opencode-plugin/{package.json,code-graph.js,README.md}`),
which is hand-maintained and left alone by the sync.

Two hook scripts in `plugin/scripts/` are shared across harnesses and get
mirrored: `session-start.sh` (emits the SDK-standard `{"additionalContext": …}`
orientation blob) and `run-hook.cmd` (a cmd/bash polyglot wrapper so the
extensionless hook scripts run on Windows). The two `.sh` hooks are
Claude-only — they implement a `PreToolUse` Grep/Glob interception no other
harness exposes — and are deliberately not mirrored.

## Behavior notes

- The nudge fires at most once per `(session, tool)` per cooldown
  (`CODE_GRAPH_NUDGE_COOLDOWN`, default 900s).
- A `SessionStart` hook resets the throttle on `startup`/`resume`/`clear`/
  `compact`, so after `/clear` or a context compaction (which wipe the model's
  memory of the earlier nudge) the guidance re-injects on the next search.
- Both hook scripts fail open: any error path exits 0 with no output, so a
  broken hook can never wedge a search or session start.
