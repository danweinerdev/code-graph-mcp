---
name: code-graph-indexing
description: Index a codebase into the code-graph MCP server and keep it fresh. Use when a code-graph query tool reports the codebase is not indexed, when starting structural analysis on a new repo, when results look stale after edits, when indexing a huge tree (UE4/LLVM-scale) that may time out, or when configuring extraction (e.g. engine API-macro classes via .code-graph.toml). Covers analyze_codebase vs analyze_codebase_async, watch mode, scoping, caching, and force re-index.
---

# code-graph indexing — build & refresh the graph

Every query tool (`search_symbols`, `get_callers`, `detect_cycles`, …) needs the
graph built first. This skill covers getting it indexed, keeping it current, and
the gotchas on large or specially-configured codebases.

## First index

`mcp__code-graph__analyze_codebase(path="<abs dir>")` parses the tree and builds
the graph. It uses an on-disk rkyv cache at `<project_root>/.code-graph-cache.db`
plus mtime-based incremental re-index, so repeat calls are cheap.
- `force=true` bypasses the cache and fully rebuilds — use it after changing
  `.code-graph.toml` (macro config, extensions) or when the graph looks wrong.
- **Scoping:** `analyze_codebase("<subtree>")` indexes only that subtree and
  *merges* into the project graph (files outside scope are preserved). Sibling
  scoped runs accumulate. Config is still discovered from the project root.

## Large codebases — prefer the async path

On big trees (tens of thousands of files), sync `analyze_codebase` can exceed the
MCP client's per-call timeout and surface as a tool error *even though the server
finishes*. Avoid this:

1. `mcp__code-graph__analyze_codebase_async(path=…)` → returns sub-second with a
   `job_id` and `status: "running"`.
2. Poll `mcp__code-graph__get_status` — read `analyze_job.progress` /
   `progress_message` for live progress, and `analyze_job.result` (or `.error`)
   once `status` becomes `"completed"` / `"failed"`.

Because each call is sub-second, the per-call timeout never fires. (If you must use
sync analyze on a large tree, raising `MCP_TOOL_TIMEOUT` to ~900000 is the
alternative.)

## Keep it fresh

- `mcp__code-graph__watch_start` watches the indexed directory and auto-reindexes
  changed files (debounced). `watch_stop` ends it.
- Without watch, just call `analyze_codebase` again — mtime incremental keeps it fast.
- If results look stale right after edits and no watch is running, re-run
  `analyze_codebase` (add `force=true` only if a *config* change is involved;
  ordinary edits don't need it).

## Check state

`mcp__code-graph__get_status` is a no-side-effect snapshot: indexed root, graph
stats (files/symbols/edges), the discovered `.code-graph.toml` path, active
`[cpp].macro_strip` counts, the last analyze timestamp + force flag, and any
in-flight/most-recent `analyze_job`. Use it to confirm something is indexed before
assuming a query failure is a real "not found."

## Configuration that affects extraction (.code-graph.toml)

Lives at the project root (nearest ancestor dir containing `.code-graph.toml`).
The cases worth flagging to the user:
- **Engine / API-macro classes won't extract by default.** `class CORE_API AActor
  : public UObject {}` is invisible unless `CORE_API` is in `[cpp].macro_strip`
  (and `UCLASS(...)`-style macros in `[cpp].macro_strip_with_args`). With **no**
  config, `analyze_codebase` emits a warning about exactly this in
  `AnalyzeResult.warnings`.
- Macro-hidden definitions can be recovered via `[cpp].macro_define_function` /
  `[cpp].macro_define_type` (opt-in).
- `[extensions]` adds/disables file extensions per language.
- Any of these config changes require `force=true` to re-parse files with
  unchanged mtime.

## Quick decision

- Small/medium repo, interactive → `analyze_codebase`.
- Large repo, or you've hit a tool timeout → `analyze_codebase_async` + poll
  `get_status`.
- Actively editing and querying → `watch_start` once, then just query.
- Changed `.code-graph.toml` → `analyze_codebase(force=true)`.
