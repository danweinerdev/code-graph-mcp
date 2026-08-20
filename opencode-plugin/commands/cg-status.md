---
name: cg-status
description: Diagnose the code-graph MCP server -- indexed root, graph stats, config path, last analyze, in-flight or completed async job progress.
---

# cg-status -- is code-graph healthy and current?

Call `get_status()` and interpret it. Use this when a code-graph tool returns something
surprising, when an async analyze is in flight, or before trusting a query on a repo you have
been editing.

## What to check, in order

1. **Is anything indexed at all?** Graph stats of zero, or an `indexed root` that is not this
   repo, means every query tool will come back empty for the right reason. Run `/cg-index`.
2. **Is the indexed root the repo you think it is?** A scoped `analyze_codebase(<subtree>)`
   leaves the project cache covering only what has been walked so far.
3. **Config path.** If no `.code-graph.toml` was discovered, built-in defaults are in force --
   which for a C++ engine tree means macro-prefixed classes silently did not extract.
4. **Last analyze timestamp + force flag.** Older than your edits, and with no watcher running,
   the graph is stale.
5. **`analyze_job`** -- present once any analyze has ever run. When you hold a `job_id` from
   `analyze_codebase_async`, prefer `get_analyze_status(job_id)` -- the dedicated per-job poll
   with the same view (an unknown/expired ID is a tool error there):
   - `status: "running"` -> report `progress` / `progress_total` / `progress_message` and poll
     again. `progress` is monotonic **within a phase** and resets at each phase boundary
     (parse -> resolve -> merge); phase identity rides on `progress_message`, so do not report a
     reset as a regression.
   - `status: "completed"` -> `result` holds the analyze body (`files`, `symbols`, `edges`,
     `root_path`, `warnings`), shape-identical to sync `analyze_codebase`.
   - `status: "failed"` -> `error` holds why.
   - `error` and `result` are mutually exclusive and both `null` while running.
6. **`analyze_job_previous_terminal`** -- the prior job, preserved across exactly one further
   kickoff. Two analyses back-to-back without reading the first lose the oldest.

Both job fields serialize as explicit `null` when absent, so a `null` means "no analyze yet",
while a missing field means an older server.

## Diagnostic: analyze "failed" but the server looks fine

If a sync `analyze_codebase` surfaced `"[Tool result missing due to internal error]"`, check
whether the cache file's mtime landed and process RSS plateaued. If both did, the server
**completed** and the client's `MCP_TOOL_TIMEOUT` gave up -- raise it to `900000`, or switch to
`analyze_codebase_async` + polling, where no single call is long enough to trip the timer. If RSS
is still climbing and no cache wrote, it is genuinely still working.

## See also

The `code-graph-indexing` skill.
