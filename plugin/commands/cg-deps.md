---
name: cg-deps
description: Inspect file-level dependencies, coupling, and circular includes with code-graph -- what a file imports, which files are most entangled, and where the dependency cycles are.
argument-hint: [file path]
---

# cg-deps -- file dependencies, coupling, and cycles

Answer dependency-structure questions about `${ARGUMENTS:-this repo}` from the `code-graph` MCP
server's include/import edges.

## Workflow

**For one file:**

1. `get_dependencies(file)` -- what it includes/imports. `DependencyEntry = { file, kind, line }`.
2. `get_coupling(file, direction: "both")` -- the two-way edge weight against every other file,
   sorted by count desc. Highest-count neighbours are the ones a change here will reach.
3. `generate_diagram(file: "...", format: "mermaid")` when the neighbourhood is worth drawing.

**For the repo:**

1. `detect_cycles()` -- circular include/import chains. Page through by `limit`/`offset`.
2. `detect_communities()` -- the de-facto module clusters (file-granularity label
   propagation over call+include edges); compare against the directory layout.
3. `get_coupling` on the hubs the cycles surface, to find where to cut.

## Reading the results

- **`kind` is always the literal string `"includes"`.** Rust `mod`, Python/Go/Java `import`, C#
  `using`, and C++ `#include` all map to `EdgeKind::Includes`. There is no `"imports"`.
- Only includes that resolve to an **indexed source file** appear. System headers, external
  paths, and anything no language plugin claims are dropped at index time -- so an empty result
  can mean "all its deps are external", not "no deps".
- `detect_cycles` is **count-paginated only**, not byte-budgeted. A page of cycles with very large
  `files` lists can exceed `[response].max_bytes` on the wire. `max_cycle_size` (default 50)
  shrinks oversized cycles in place -- setting `Cycle.truncated` and `original_len` -- it never
  drops a cycle.
- Two independent `truncated` notions: on the envelope it means more cycles on later pages; on a
  `Cycle` it means that one cycle's file list was capped. Neither implies the other.
- `get_coupling(direction: "both")` returns `{ incoming, outgoing }` with **no top-level
  `results`**, and the two pages share one byte budget sequentially -- if `incoming` exhausts it,
  `outgoing` comes back empty with `truncated: true` and `next_offset: 0`. That is a start-fresh
  marker, not a continuation: raise `[response].max_bytes`, re-run `analyze_codebase`, then retry
  instead of re-calling offset 0.
- `subtree` on `detect_cycles` is a **post-detection filter**: cycles that cross the prefix
  boundary are silently dropped, not clipped.
- `detect_communities` has **two independent caps**: `limit`/`offset` page over communities,
  `members_per_community` (default 10, max 100) caps members inside each -- a member-capped
  community sets its own `truncated`/`original_len` while `size` stays the true total. A
  `degenerate` partition (giant blob / all singletons) is flagged, not silently returned.

## Language caveats to state in the answer

- **Rust:** only intra-crate `mod foo;` produces file edges. `use` / `extern crate` are extracted
  then dropped at resolve -- by design. A Rust dependency answer is a module-tree answer, not a
  crate-graph answer.
- **Go:** import paths are recorded verbatim and are **not** resolved to indexed files across
  modules, so `get_dependencies` on Go is thin. `go.mod` is consulted for namespaces only.
- **Python:** `from foo import bar` records `"foo"` -- the module is the dependency, not the name.
  Conditional imports under `if TYPE_CHECKING:` / `try:` are not extracted.

## See also

The `code-graph-dependencies` skill.
