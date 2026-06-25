---
name: code-graph-dependencies
description: Analyze file-level dependencies, coupling, circular includes, and render call/dependency/inheritance diagrams with the code-graph MCP server. Use when the user asks what a file imports/includes, which files are most coupled, whether there are circular dependencies, how modules depend on each other, or wants a Mermaid/edge-list diagram of a call graph or file-dependency graph.
---

# code-graph dependencies & diagrams — file-level structure

Beyond symbols and calls, code-graph tracks **include/import edges** between files
(C++ `#include`, Rust `mod`, Python/Go/Java `import`, C# `using` — all surface as
`EdgeKind::Includes`). These tools answer module-architecture questions.

**Precondition:** codebase indexed (`analyze_codebase`; see **code-graph-indexing**).

## Pick the tool

| Question | Tool |
|---|---|
| What does this file include/import? | `mcp__code-graph__get_dependencies` |
| Which files are most coupled to this one? | `mcp__code-graph__get_coupling` |
| Are there circular dependencies? | `mcp__code-graph__detect_cycles` |
| Draw a call / dep / inheritance diagram | `mcp__code-graph__generate_diagram` |

## get_dependencies

`get_dependencies(file="<abs path>")` → `Page<DependencyEntry>` of `{file, kind, line}`.
`kind` is **always `"includes"`** for every language. Only includes that resolve to
an *indexed* source file appear — system headers, external paths, and non-source
files are dropped at index time. (Rust: only intra-crate `mod foo;` survives;
`use`/`extern crate` paths are intentionally out of scope.)

## get_coupling

`get_coupling(file=…, direction=…)`:
- `"outgoing"` (default) / `"incoming"` → `Page<CouplingEntry>` `{file, count}`,
  sorted by `count` desc. `count` = call+include edges between the two files.
- `"both"` → `{incoming, outgoing}` (two pages, no top-level `results`).

Use it to find a file's heaviest neighbors before a refactor or a move.

## detect_cycles

`detect_cycles()` finds circular include chains (SCCs of the include graph).
Returns the standard envelope; each `Cycle` is `{files, truncated, original_len?}`.
Two independent `truncated` notions:
- **envelope** `truncated` → more cycle *pages* exist (resume with `next_offset`).
- **per-cycle** `truncated` → that one cycle's `files` list was capped by
  `max_cycle_size` (default 50, raise to see fuller lists).

Pagination here is **by-count only** (`limit`/`offset`), not byte-budgeted, so a
page with huge cycles can still be large. `subtree` filters to cycles fully under a
prefix. Default `limit` is 20 because cycles are rare in healthy codebases.

## generate_diagram

`generate_diagram` has three modes:
- `symbol="<id>"` → **call graph**. Takes `direction` (`callees`/`callers`/`both`)
  and `min_confidence`.
- `file="<abs path>"` → **file-dependency** graph.
- `class="<name>"` → **inheritance** graph.

Output `format`: `edges` (default — JSON `{from, to, label, direction}` rows) or
`mermaid` (flowchart text to drop into docs). Endpoints that don't resolve to a
real symbol are dropped (no basename pseudo-nodes).

Caveat (`symbol=` mode only): the diagram dedupes on the *rendered label pair*, so
two distinct symbols whose display labels collide become one edge. When you need
ID-level or per-arm fidelity, use `get_callers`/`get_callees` instead.

## Recipes

- **"Does this project have circular includes?"** →
  `detect_cycles()`; raise `max_cycle_size` if a cycle reports `truncated`.
- **"What's tangled up with `net/socket.cpp`?"** →
  `get_coupling(file="…/net/socket.cpp", direction="both")`.
- **"Give me a Mermaid call graph of `handle_request`."** →
  `generate_diagram(symbol="…:handle_request", direction="both", format="mermaid")`.
