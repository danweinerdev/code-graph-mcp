---
name: code-graph-smoke-test
description: Exercise the code-graph MCP server end-to-end against the current repo and report what works, what violates its contracts, and what looks anomalous. Use when asked to test or validate code-graph on a repo, after upgrading or redeploying the code-graph binary, after changing .code-graph.toml, when tool results look wrong and you need to isolate server vs usage error, or as an acceptance pass after indexing a new codebase. Covers index sanity, every query tier, response-contract checks, CLI parity, and known-pitfall probes.
---

# code-graph smoke test -- dogfood the server on this repo

A reusable acceptance pass: drive the real tool surface against whatever
repo you are in, verify the responses against their documented contracts,
and report pass/fail per tier plus anomalies. It found a real indexing bug
the first time it ran; treat surprising results as findings, not noise.

**Signal hygiene first.** Other sessions may share the daemon and the
binary. Never kill code-graph processes you did not spawn; never
`force=true` a shared repo's index without saying so in the report (it
rebuilds state other sessions are using); run destructive cache
experiments only in a throwaway temp project (recipe at the bottom).

## Tier 0 -- identity and index sanity

1. `get_status` -- record `binary_version`, `indexed`, counts. If a binary
   deploy just happened, the version proves which build is answering.
2. `analyze_codebase(path=<repo>)` (async + `get_analyze_status` on huge
   trees). Then the PLAUSIBILITY CHECK: does `files` match a rough census
   of source files on disk? A count far too low usually means a stale or
   subtree-scoped cache answered -- re-run once with `force=true` and
   compare. Record both numbers if they differ; a non-force analyze that
   cannot reach the on-disk file set is a server bug, not an expectation
   error.
3. Warnings: config discovery notices are informational; orphan-cache
   warnings and parse-failure warnings go in the report.

## Tier 1 -- symbols

- `search_symbols(query="^<known symbol>$")` -- exact anchor finds exactly
  it. A miss on a symbol you can see in a file is a finding (check macro
  config before filing).
- `get_file_symbols` on a real file; `count_only=true` returns the bare
  envelope with `limit: 0`.
- `get_symbol_summary()` -- namespaces/kinds look like the repo.
- `get_symbol_at(file, line)` on a line INSIDE a known function -- the
  innermost enclosing symbol comes first; it is span containment, not
  goto-definition.
- `find_class_candidates` on a known class name.

## Tier 2 -- call graph contracts

- `get_callers` / `get_callees` on a hot function. Verify the envelope
  `{results, total, offset, limit, truncated, next_offset}` and that every
  row carries `{symbol_id, file, line, depth, candidates}` -- `symbol_id`
  is the DEFINITION site, `file`/`line` the CALL site, `candidates >= 1`.
- `find_path(from, to)` on a pair you believe connected: `found: true`,
  `hops[0]` has `entered_by: null` and `candidates: null`, later hops have
  both. On an unrelated pair: `found: false` with `cap_reached: false`
  (SUCCESS shape, not an error).
- Non-callable soft-hint: `get_callers` on a Struct/Enum/Trait/Interface
  returns SUCCESS with a plain-text advisory, NOT the JSON envelope. A
  callable with zero hops returns the empty envelope instead. Verify the
  trichotomy holds.
- `find_overrides` on a virtual method if the repo has C++ inheritance;
  empty page on a non-virtual is correct, not a failure.

## Tier 3 -- structure

- `detect_cycles()` -- if the repo has a known cycle (or a fixture pair),
  it appears; per-cycle `truncated` vs envelope `truncated` are
  independent.
- `get_orphans(reliability="high", subtree=<module>)` -- spot-check one
  result with `get_callers` to confirm zero inbound.
- `get_coupling(file, direction="both")` -- returns `{incoming, outgoing}`
  with NO top-level `results`.
- `detect_communities()` -- `termination` is `converged` or
  `iteration_ceiling`; a `degenerate` partition is flagged, not silently
  returned; per-community member caps set `truncated`/`original_len`
  while `size` stays the true total.
- `get_class_hierarchy` on a known class -- diamonds collapse to
  `ref: true` stubs.

## Tier 4 -- viz and history

- `generate_diagram(symbol=..., format="mermaid")` renders; `format=edges`
  rows carry `direction` and (symbol mode only) `candidates`.
- With a VCS checkout: `blame_symbol` on a known symbol -- hunks
  non-overlapping, in file order; `available: false` + `reason` is a
  SUCCESS shape (untracked path, no VCS), never an error.
- `symbol_history` on a symbol with known history -- transitions only.
  The sharpest contract check available: a comment-only or reformat-only
  commit must appear in `blame_symbol` hunks yet be ABSENT from
  `symbol_history` entries under `normalized` mode.

## Tier 5 -- CLI parity (when the `code-graph` binary is available)

- `code-graph get-status --json` -- same shape as the MCP tool; if a
  daemon is live the CLI attaches (never spawns) and sees the same graph.
- One query subcommand with `--json` -- byte-identical payload to the MCP
  response for the same arguments.
- Exit codes: a success (even an empty page) exits 0; an unknown symbol
  exits 1.

## Optional deep probe -- scoped-cache correctness (isolated)

In a THROWAWAY temp dir (never the shared repo): create `root/a/one.cpp`
and `root/b/two.cpp` plus an empty `[discovery]` toml at root; analyze
`root/a`, then analyze `root` WITHOUT force. The second result must report
BOTH files and a search for the `b/` symbol must hit. Also: add a new file
after a full index and re-analyze non-force -- it must appear. Both cases
regressed once (fast-path cache short-circuit); this probe pins them
against a live binary.

## Report format

Per tier: pass / fail / skipped-with-reason. Then findings, each with the
exact call, expected contract, observed response, and a severity guess
(server bug vs config gap vs usage error). Close with binary_version, the
index counts, and whether `force` was used on a shared index.
