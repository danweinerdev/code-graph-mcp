---
name: cg-index
description: Build or refresh the code-graph index for this repo -- use when a code-graph tool reports the codebase is not indexed, results look stale after edits, or a huge tree needs the async path.
argument-hint: [path] [force]
---

# cg-index -- build or refresh the code-graph index

Index the repo so the structural query tools have a graph to answer from.

## When to use

A code-graph tool reports the codebase is not indexed; results look stale after edits; you just
changed `.code-graph.toml`; or you are starting structural work on an unfamiliar repo.

## How to use

Default -- index the whole project:

```text
analyze_codebase(path: "<repo root or ${ARGUMENTS}>")
```

**On a large tree (roughly >20k files), use the async path instead.** Claude Code's
`MCP_TOOL_TIMEOUT` is a hard wall-clock per call and progress notifications do not extend it, so
a UE4/LLVM-scale sync analyze can surface as `"[Tool result missing due to internal error]"`
while the server runs happily to completion:

```text
analyze_codebase_async(path: "...")   -> { job_id, status: "running", ... }
get_status()                        -> poll analyze_job.progress / .progress_message
                                    -> analyze_job.result once status == "completed"
```

Every poll is sub-second, so the per-call timer never fires. `analyze_job.result` is
shape-identical to sync `analyze_codebase`'s body.

## Arguments

- **`path`** -- indexing scope. Invocation-local: `analyze_codebase("<subtree>")` parses only that
  subtree even when the project root is upstream. Scoped runs **merge** into the project cache
  rather than clobbering it, so sibling subtrees accumulate.
- **`force: true`** -- bypass the cache and re-parse. Required after changing `[cpp].macro_strip`,
  `macro_strip_with_args`, `macro_define_function`, `macro_define_type`, or `[extensions]`:
  those do **not** retroactively re-parse files whose mtime is unchanged. Scoped `force` only
  invalidates inside the invoked subtree.

`[response].max_bytes` changes do **not** need `force=true` -- just re-run `analyze_codebase`.
If a positive byte budget yields an empty `truncated: true` page with `next_offset` equal to the
requested `offset`, it is a start-fresh marker, not a continuation: raise the budget, re-run the
analysis, then retry instead of re-calling the same offset.

## Notes

- Read `AnalyzeResult.warnings`. A "no config found" warning means engine-style declarations
  (`class CORE_API Foo`) will not extract until `[cpp].macro_strip` is configured; an "orphan
  cache" warning means a stale cache sits at an invocation subdir and is being ignored.
- Cache lives at `<project_root>/.code-graph-cache.db` (rkyv, v8), co-located with the discovered
  `.code-graph.toml`. A version mismatch silently re-indexes -- no `force` needed.
- To keep the graph live while you edit, call `watch_start` (auto-reindex on change) and
  `watch_stop` when done.
- Report files, symbols, edges, and the root path back to the user.

## See also

The `code-graph-indexing` skill for scoping, cache invalidation, watch mode, and
`.code-graph.toml` configuration in full.
