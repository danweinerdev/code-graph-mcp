---
name: cg-status
description: Diagnose the code-graph MCP server — indexed root, graph stats, config path, last analyze, in-flight or completed async job progress.
---

# cg-status — is code-graph healthy and current?

Call `get_status()` for the server/current-FIFO diagnostic. For an async analyze, retain its
`job_id` and call `get_job_status(job_id)` as the primary queued/running/terminal poll and result
retrieval endpoint.

## What to check, in order

1. **Is anything indexed at all?** Graph stats of zero, or an `indexed root` that is not this
   repo, means every query tool will come back empty for the right reason. Run `/cg-index`.
2. **Is the indexed root the repo you think it is?** A scoped `analyze_codebase(<subtree>)`
   leaves the project cache covering only what has been walked so far.
3. **Config path.** If no `.code-graph.toml` was discovered, built-in defaults are in force —
   which for a C++ engine tree means macro-prefixed classes silently did not extract.
4. **Last analyze timestamp + force flag.** Older than your edits, and with no watcher running,
   the graph is stale.
5. **Per-job async lifecycle.** Call `get_job_status(job_id)`:
    - `status: "queued"` or `"running"` → report `progress` / `progress_total` /
      `progress_message` and poll again. Progress is monotonic **within a phase** and resets at
      phase boundaries.
    - `status: "completed"` → `result` holds the analyze body (`files`, `symbols`, `edges`,
      `root_path`, `warnings`), structurally/deserializer-compatible and byte-identical to a
      non-coalesced sync `analyze_codebase` body; a coalesced sync response adds `coalesced_by`.
    - `status: "failed"` → `error` holds why. `error` and `result` are mutually exclusive.
    - Displaced terminal jobs remain retrievable by ID for a bounded 32-job history; an unknown
      or expired ID is a tool error.
6. **FIFO diagnostic.** `get_status.analyze_job` is only the single current job;
    `analyze_job_pending_count` and `analyze_job_pending_ids` show queued jobs in promotion
    order. `analyze_job_previous_terminal` is the prior terminal job preserved across one
    rotation for compatibility.

The two `get_status` job-view fields serialize as explicit `null` when absent, so a `null` means
"no analyze yet", while a missing field means an older server.

## Diagnostic: analyze "failed" but the server looks fine

If a sync `analyze_codebase` surfaced `"[Tool result missing due to internal error]"`, check
whether the cache file's mtime landed and process RSS plateaued. If both did, the server
**completed** and the client's `MCP_TOOL_TIMEOUT` gave up — raise it to `900000`, or switch to
`analyze_codebase_async` + polling, where no single call is long enough to trip the timer. If RSS
is still climbing and no cache wrote, it is genuinely still working.

## See also

The `code-graph-indexing` skill.
