---
name: cg-index
description: Build or refresh the code-graph index for this repo — use when a code-graph tool reports the codebase is not indexed, results look stale after edits, or a huge tree needs the async path.
argument-hint: [path] [force]
---

# cg-index — build or refresh the code-graph index

Index the repo so the structural query tools have a graph to answer from.

## When to use

A code-graph tool reports the codebase is not indexed; results look stale after edits; you just
changed `.code-graph.toml`; or you are starting structural work on an unfamiliar repo.

## How to use

Default — index the whole project:

```text
analyze_codebase(path: "<repo root or ${ARGUMENTS}>")
```

**On a large tree (roughly >20k files), use the async path instead.** Claude Code's
`MCP_TOOL_TIMEOUT` is a hard wall-clock per call and progress notifications do not extend it, so
a UE4/LLVM-scale sync analyze can surface as `"[Tool result missing due to internal error]"`
while the server runs happily to completion:

```text
analyze_codebase_async(path: "…")   → { job_id, status, … }
get_job_status(job_id: "…")          → poll progress / progress_message
                                     → result once status == "completed"
```

`analyze_codebase_async` returns before indexing and normally quickly, but admission
canonicalization/config discovery may wait on a slow filesystem. It still avoids the
per-call timeout during indexing. `get_job_status.result` is structurally/deserializer-compatible and byte-identical to a non-coalesced sync
`analyze_codebase` body; a coalesced sync response adds `coalesced_by`. Use `get_status()` for
current-job and FIFO queue diagnostics.

When a sync request is covered by an existing analyze, it waits for that outcome and its result
adds `coalesced_by` with the covering job ID (or its error ends with `(coalesced_by: <job_id>)`).
The first distinct request behind a running job blocks to its own terminal result. If distinct work
is already pending, sync returns a queued `{ job_id, status, started_at, existing, note }` response
immediately; poll that job.

The shared FIFO accepts at most 32 pending jobs. A covered analyze still coalesces when it is full;
an additional distinct analyze or community request instead returns the retryable queue-full error.
Wait for queued work to complete and retry, or poll `get_status()` for FIFO diagnostics.

## Arguments

- **`path`** — indexing scope. Invocation-local: `analyze_codebase("<subtree>")` parses only that
  subtree even when the project root is upstream. Scoped runs **merge** into the project cache
  rather than clobbering it, so sibling subtrees accumulate.
- **`force: true`** — bypass the cache and re-parse. Required after changing `[cpp].macro_strip`,
  `macro_strip_with_args`, `macro_define_function`, `macro_define_type`, or `[extensions]`:
  those do **not** retroactively re-parse files whose mtime is unchanged. Scoped `force` only
  invalidates inside the invoked subtree.

`[response].max_bytes` changes do **not** need `force=true` — just re-run `analyze_codebase`.

For `detect_communities_async`, an empty `truncated` terminal page with an unchanged
`next_offset` is a start-fresh marker, not a continuation. Do **not** retry it unchanged: raise
`[response].max_bytes`, re-run `analyze_codebase` to refresh cached config, then retry.

## Notes

- Read `AnalyzeResult.warnings`. A "no config found" warning means engine-style declarations
  (`class CORE_API Foo`) will not extract until `[cpp].macro_strip` is configured; an "orphan
  cache" warning means a stale cache sits at an invocation subdir and is being ignored.
- Cache lives at `<project_root>/.code-graph-cache.db` (rkyv, v10), co-located with the discovered
  `.code-graph.toml`. A version mismatch silently re-indexes — no `force` needed.
- To keep the graph live while you edit, call `watch_start` (auto-reindex on change) and
  `watch_stop` when done.
- Report files, symbols, edges, and the root path back to the user.

## See also

The `code-graph-indexing` skill for scoping, cache invalidation, watch mode, and
`.code-graph.toml` configuration in full.
