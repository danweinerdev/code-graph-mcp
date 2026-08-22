---
name: cg-survey
description: Structural health survey of a codebase (or subtree) with code-graph -- size and shape, dead code, dependency cycles, and coupling hotspots, as one report.
argument-hint: "[subtree path]"
---

# cg-survey -- structural health report

Produce an orientation report for `${ARGUMENTS:-this repo}` from the `code-graph` MCP server.
Use it to get oriented in an unfamiliar codebase before changing anything, or as a periodic
health check.

## Workflow

1. **Shape.** `get_symbol_summary()` -- `(namespace, kind, count)` rows. This is the cheapest
   possible map of what exists and where the mass sits. Scope with `subtree` when given a path.
2. **Dead code.** `get_orphans(reliability: "high", subtree: ...)` -- symbols with zero incoming
   call edges. `"high"` drops virtual methods and macro-synthesized symbols, which are the
   dominant false-positive classes.
3. **Cycles.** `detect_cycles(subtree: ...)` -- circular include/import chains.
4. **Hotspots.** `get_coupling` on the files the previous steps flag as central.
5. **Confirm before recommending deletion.** For anything from step 2 you would suggest removing,
   run `get_callers` on it. Orphan status is "no *resolved* inbound edge", which is not the same
   as "unreachable".

## Reporting

Give the user: total symbols by kind, the top namespaces by mass, a **ranked** orphan list with
the caveats attached, each cycle as a file chain, and the top coupled pairs. Ranked, not dumped --
a 400-row orphan list is not a finding.

## What "orphan" does not mean

State these explicitly whenever you recommend a deletion:

- **Entry points and exports look like orphans.** `main`, `init`, test functions, FFI/`extern`
  exports, trait-impl methods invoked through dynamic dispatch, and anything called only from
  outside the indexed scope all have zero inbound edges in-graph.
- **Reflection, callbacks, and dynamic dispatch are invisible.** Python especially: dynamic typing
  makes call resolution noisiest there, and nothing infers types.
- **Go interface satisfaction is structural** and produces no edges at all.
- **C++ macro-generated definitions** don't exist in the graph unless `[cpp].macro_define_*` is
  configured, so their callees look orphaned.
- **The reliability filter is signature-driven, not confidence-based.** Heuristic-vs-resolved has
  no leverage here -- an orphan has no inbound edges to grade.

A survey finds *candidates*. The user decides.

## Notes

- If the index is missing or stale, run `/cg-index` first; a survey over a stale graph reports
  deleted code as live.
- Scope aggressively on large repos. `subtree` on `get_orphans` / `detect_cycles` /
  `search_symbols` keeps the walk O(subtree), not O(graph).

## See also

The `code-graph-refactor-survey` skill.
