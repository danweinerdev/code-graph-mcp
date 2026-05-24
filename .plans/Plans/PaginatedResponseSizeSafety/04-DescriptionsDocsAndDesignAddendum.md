---
title: "Tool descriptions, CLAUDE.md, design addendum"
type: phase
plan: PaginatedResponseSizeSafety
phase: 4
status: complete
created: 2026-05-11
updated: 2026-05-12
deliverable: "All 5 tool descriptions rewritten with byte-budget + count_only awareness; CLAUDE.md Response shapes, MCP tools, Configuration, and Core invariants sections updated; .code-graph.toml.example gains [response]; Designs/Pagination/README.md extended with Decisions 8-13"
tasks:
  - id: "4.1"
    title: "Rewrite the 5 tool description strings under the Agent-facing lens"
    status: complete
    verification: "Each of get_orphans, search_symbols, get_file_symbols, get_callers, get_callees descriptions (a) names the envelope shape verbatim as {results, total, offset, limit, truncated, next_offset}, (b) documents every arg's default + ceiling AND mentions count_only where applicable, (c) explains the truncated/next_offset paging-resume protocol, (d) uses operationally-correct verbs (no 'raise offset for more results' or similar misdirection), (e) is checked against the Agent-facing tool descriptions lens checklist in CLAUDE.md item by item; tools-list snapshots regenerate"
  - id: "4.2"
    title: "Update CLAUDE.md Response shapes section"
    status: complete
    verification: "Page<T> envelope definition in CLAUDE.md gains truncated + next_offset with prose explaining the paging-resume contract; section explicitly notes that limit is now '<= this many records' rather than an exact guarantee when byte budget bites; the count_only response shape is documented as a subset of Page<T>; sibling sections (Core invariants, MCP tools table) checked for framing contradictions per the Documentation read cold lens"
    depends_on: ["4.1"]
  - id: "4.3"
    title: "Update CLAUDE.md MCP tools (15) table"
    status: complete
    verification: "Notes column for the 3 SymbolResult-emitting tools mentions count_only with its purpose; Notes column for all 5 paginated tools references the byte budget (with a default-100KB note pointing at [response].max_bytes); table layout unchanged otherwise; entry under Core invariants for 'Symbol ID format' references the id_to_file recovery helper as the documented contract for clients reading record ids"
    depends_on: ["4.2"]
  - id: "4.4"
    title: "Update .code-graph.toml.example and CLAUDE.md Configuration section"
    status: complete
    verification: ".code-graph.toml.example gains a [response] section with max_bytes commented out at the default; CLAUDE.md Configuration section gains a matching block in the TOML sample plus prose explaining what max_bytes controls; Cache invalidation subsection explicitly states [response].max_bytes is read at query time so changes take effect at next analyze_codebase call (no force=true needed); force=true load-bearing phrase still present in both CLAUDE.md and .code-graph.toml.example per the documentation-read-cold lens"
    depends_on: ["4.3"]
  - id: "4.5"
    title: "Append Decisions 8-13 to Designs/Pagination/README.md"
    status: complete
    verification: "Designs/Pagination/README.md gains a Decisions 8 through 13 section matching the plan README's K Decisions verbatim (byte cap + config, count_only, file-drop, CallChain.file retained, search_symbols handler-layer trim, limit ceiling unchanged); updated: frontmatter bumped to today; design status stays at review (not flipped to approved here — separate workflow); rationale paragraphs cite the user report and acceptance criteria from this plan"
    depends_on: ["4.4"]
tags: [pagination, mcp, llm-optimization, byte-budget, regression-fix]
---

# Phase 4: Tool descriptions, CLAUDE.md, design addendum

## Overview

This is the cross-cutting docs and contract-text phase. By the time this lands, all behavior is shipped; what remains is making the new behavior discoverable to (a) MCP clients via tool descriptions, (b) future Claude sessions via CLAUDE.md, and (c) future planners via the design doc.

Two quality lenses from CLAUDE.md drive this phase:

- **Agent-facing tool descriptions**: descriptions are production behavior. Misleading text in a description is a bug.
- **Documentation read cold**: read the modified AND surrounding doc sections cold, as a future agent would, to catch framing contradictions.

Both lenses apply explicitly to specific tasks below.

## 4.1: Rewrite the 5 tool description strings under the Agent-facing lens

### Subtasks
- [ ] Edit `crates/code-graph-tools/src/server.rs` description strings for each of the 5 tools
- [ ] Apply the "Agent-facing tool descriptions" checklist from CLAUDE.md item by item to each rewritten string:
  - Every named arg documented with default + ceiling
  - Verb in suggested action operationally produces the claimed result
  - Response envelope shape named, not implied: `{results, total, offset, limit, truncated, next_offset}`
  - Hint when non-default values are appropriate
  - Plurality + units match field type
  - count_only documented where applicable (orphans, search_symbols, file_symbols)
  - Paging-resume protocol: when `truncated=true`, callers should re-call with `offset = next_offset`
- [ ] `search_symbols` description in particular gets the largest rewrite — Researcher flagged it as notably thinner than the others (no envelope shape, no defaults documented). Bring it to parity with `get_callers` quality
- [ ] Regenerate 5 tools-list snapshots
- [ ] Self-review: read each description cold (without referencing the diff) and confirm it'd let an LLM caller plan a call correctly on the first try

### Notes
PaginationOverhaul Phase 4 caught two real agent-misleading bugs in description rewrites — the "Agent-facing tool descriptions" lens is repo-validated. Lean on the checklist.

## 4.2: Update CLAUDE.md Response shapes section

### Subtasks
- [ ] Edit `CLAUDE.md` Response shapes section (under MCP tools)
- [ ] Update the Page<T> envelope description to include `truncated: bool` and `next_offset?: u32 | null`
- [ ] Add prose: "When `truncated=true`, the page was cut short by the byte budget. Re-call with `offset = next_offset` to continue."
- [ ] Add prose: "`limit` is now an upper bound. The returned page may have fewer records when the byte budget bites — check `truncated` rather than `results.length == limit`."
- [ ] Add a sub-block for the count_only response shape: `{results: [], total, offset: 0, limit: 0, truncated: false, next_offset: null}` — explicitly note that `limit: 0` is a deliberate exception to the "envelope echoes resolved limit" contract, since count_only callers opted out of paging entirely
- [ ] Cross-check against the Documentation-read-cold lens: read sibling sections (Core invariants, the MCP tools table) cold and flag any framing contradictions before committing

### Notes
"Core invariants" item "Symbol ID format" already documents `file:name` and `file:Parent::name` — this is the anchor we lean on for the id-recovery contract in 4.3.

## 4.3: Update CLAUDE.md MCP tools (15) table

### Subtasks
- [ ] Edit CLAUDE.md MCP tools (15) table — Notes column for `get_orphans`, `search_symbols`, `get_file_symbols`: append "; `count_only=true` returns total without records (< 1KB response)"
- [ ] Notes column for all 5 paginated tools: append "; response capped at `[response].max_bytes` (default 100KB) — see Response shapes"
- [ ] Core invariants section, "Symbol ID format" entry: append a line — "Records returned by paginated tools no longer include a separate `file` field; clients recover it via the documented id-to-file split (rsplit on the rightmost `:` not part of `::`)"
- [ ] Verify no entries collide with the existing line about `get_class_hierarchy` `total_nodes_seen` etc.

### Notes
Table layout is the main risk here — column widths and the Notes column are dense. Use existing formatting (markdown pipes) verbatim.

## 4.4: Update .code-graph.toml.example and CLAUDE.md Configuration section

### Subtasks
- [ ] Edit `.code-graph.toml.example` at the repo root — add a new `[response]` section after `[parsing]`, before `[cpp]`. Show `max_bytes` commented out at the default value with explanatory comment:
  ```toml
  [response]
  # Byte cap on paginated MCP responses. When a page would exceed this size,
  # the server truncates mid-page and surfaces `truncated: true` plus a
  # `next_offset` to resume paging. Default chosen to fit Claude Code's
  # harness; raise for clients with larger token budgets.
  # max_bytes = 102400
  ```
- [ ] Edit CLAUDE.md Configuration section — add a matching `[response]` block to the inline TOML sample
- [ ] Add prose under the TOML sample: "`[response].max_bytes`: byte cap on paginated tool responses. Default 102400. Consulted from the cached `RootConfig` on each tool call (the TOML file is NOT re-read per query)."
- [ ] Add to the Cache invalidation subsection: "`[response].max_bytes` is consulted from the cached `RootConfig` on each tool call. The cache is refreshed by `analyze_codebase`; the value affects response shaping only, so no `force=true` is required to apply a changed value at the next reload."
- [ ] Documentation-read-cold lens: `grep "force=true" CLAUDE.md .code-graph.toml.example` to confirm the load-bearing phrase still appears in both after edits

### Notes
The Cache invalidation subsection already documents that `[cpp].macro_strip` and `[extensions]` need `force=true`. The new `[response]` row is the exception (read at query time), and the doc should explicitly call this out to prevent the reader from over-generalizing the `force=true` rule.

## 4.5: Append Decisions 8-13 to Designs/Pagination/README.md

### Subtasks
- [ ] Edit `.plans/Designs/Pagination/README.md`
- [ ] After Decision 7, add Decisions 8 through 13 matching the plan README's Key Decisions verbatim
- [ ] Each decision includes a "Rationale" paragraph referencing the original user feedback (1,031-orphan repro on rust-main, ~74K-token payload, harness rejection)
- [ ] Bump frontmatter `updated:` to today
- [ ] Status stays at `review` — not flipped to `approved` here
- [ ] Cross-link: design doc references this plan's path; plan README already references the design

### Notes
The design doc is the canonical record of decisions; this plan is the execution scaffolding. Future planners exploring "why byte budget" should land in the design doc, not have to read the plan to reconstruct the reasoning.

## Acceptance Criteria
- [ ] All 5 tool descriptions pass the Agent-facing checklist item by item
- [ ] CLAUDE.md Response shapes, MCP tools table, Core invariants (Symbol ID format) updated and internally consistent
- [ ] CLAUDE.md Configuration + `.code-graph.toml.example` agree on the `[response]` section
- [ ] Documentation-read-cold sweep finds no framing contradictions
- [ ] `force=true` load-bearing phrase preserved in both CLAUDE.md and `.code-graph.toml.example`
- [ ] `Designs/Pagination/README.md` extended with Decisions 8-13; `updated:` bumped
- [ ] All 5 tools-list snapshots regenerate cleanly
- [ ] `cargo insta review` clean; `make snapshot-clean` passes
