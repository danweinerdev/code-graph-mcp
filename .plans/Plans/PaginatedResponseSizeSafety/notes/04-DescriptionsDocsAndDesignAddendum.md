---
title: "Phase 4 Debrief: Tool descriptions, CLAUDE.md, design addendum"
type: debrief
plan: "PaginatedResponseSizeSafety"
phase: 4
phase_title: "Tool descriptions, CLAUDE.md, design addendum"
status: complete
created: 2026-05-12
---

# Phase 4 Debrief: Tool descriptions, CLAUDE.md, design addendum

## Decisions Made

- **Tool descriptions grew ~3x in total length** (1920 → 5917 chars across 5 strings). The pre-Phase-4 strings averaged 384 chars and lacked envelope shape, paging-resume protocol, byte-budget cap source, and count_only mention. Post-Phase-4 strings average 1183 chars — long, but each adds load-bearing information for LLM clients. `search_symbols` got the largest absolute and relative bump (148 → 1368, 9.2×) — it was notably thinner pre-rewrite.
- **"results.length < limit ≠ no more pages" warning** included in every paginated tool description. This was an unprompted addition by the 4.1 implementer based on the common LLM bug pattern (using length-based loop termination). The byte-budget can return a short page even when more results exist; agents must check `truncated`, not `length`.
- **Per-task scan skipped for pure-doc tasks (4.2, 4.3, 4.4, 4.5).** Replaced with one consolidated end-of-phase cold-read scan. Rationale: per-task scans on small doc edits return Accept; the cross-section view catches contradictions that per-task can't see. This decision paid off — the consolidated scan caught 1 Major (design doc Architecture still denying truncated/next_offset existence) + 2 Minors that the per-task implementer pass missed.
- **Design doc status stays at `review`, not flipped to `approved`.** Per the task spec, flipping is a separate workflow. The addendum added Decisions 8–13 and bumped `updated:` only.
- **Both files (CLAUDE.md and `.code-graph.toml.example`) get the `[response]` block** with similar prose but different formats (the inline TOML sample uses a tighter style than the `.example` file). The `[response]` block in the example file is the one users will see when they bootstrap a new project; the CLAUDE.md sample is for agents reading the canonical doc.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| 5 tool descriptions rewritten with checklist applied | Met | All 7 lens checklist items per description |
| CLAUDE.md Response shapes: 6-field envelope + count_only | Met | Plus the "limit is upper bound" reframing |
| CLAUDE.md MCP tools table: count_only + byte-budget Notes | Met | All 5 paginated tools |
| `.code-graph.toml.example`: `[response]` section | Met | Mirrors CLAUDE.md inline sample |
| Cache invalidation carve-out for `[response]` | Met | Explicit "no force=true required" + concrete action ("re-run analyze_codebase") |
| Pagination design: D8–D13 added | Met | Plus pre-existing Architecture-section contradictions fixed in polish |
| `force=true` load-bearing phrase preserved | Met | 7 occurrences across both files |

## Deviations

- **`detect_cycles` and `SearchSymbolsArgs.kind` schemars were out-of-scope but adjacent.** 4.1's quality scan caught both as cold-read contradictions with the 5 rewritten descriptions. Cleaned up in 4.1 polish. These were pre-existing gaps that became more visible after the 5 sibling tools were updated.
- **Pagination design Architecture section had stale 4-field text + diagrams** that explicitly DENIED the existence of `truncated`/`next_offset`. D8 introduced them. Without the consolidated cold-read scan, this Major contradiction would have shipped as-is — D8 says one thing, the Architecture section above it says the opposite. Fixed in 4.5 polish with a "Superseded by D8" note and updated Mermaid + sequence diagrams.
- **CLAUDE.md Agent-facing tool descriptions lens checklist example was itself stale.** It showed `{results, total, offset, limit}` as the canonical envelope shape — a future quality reviewer applying the checklist would have flagged correct 6-field descriptions as "extra fields not in example." Fixed in 4.5 polish.
- **Phase 4.4 cache-invalidation guidance was ambiguous.** Pre-polish: "no force=true is required to apply a changed value at the next reload." Cold reader couldn't tell what "reload" meant (re-run analyze_codebase? restart MCP server?). The macro_strip guidance names exactly "re-run analyze_codebase with force=true"; mirrored that style in polish.

## Risks & Issues Encountered

- **Per-task doc scans were insufficient.** Each 4.2/4.3/4.4 implementer did a "Documentation read cold" self-sweep and flagged contradictions to be fixed by the next task. The flag-forward pattern worked partially: 4.4 fixed the `[response]`-section dangling references that 4.2 and 4.3 introduced. But the design doc's Architecture section contradiction (Major) wasn't in scope of any per-task self-sweep — it required a cross-section view. The end-of-phase consolidated scan caught it.
- **Tool description rewrites are easy to under-spec.** `search_symbols`'s 148-char pre-rewrite description was a hint that some descriptions get less attention than others. The 4.1 dispatch prompt included an explicit "search_symbols gets the largest rewrite — bring it to parity with get_callers" instruction; without that, the implementer might have done minimum-viable on the thinnest description.
- **Snapshot regenerations cascaded.** Each description rewrite regenerated 1 tools-list snapshot. 4.1 → 5 snapshots. 4.1 polish → 2 more (detect_cycles + search_symbols). Manageable but illustrative of how a single agent-facing string change has wide test-side blast radius.

## Lessons Learned

- **Stale-finding deferral catches up at scope-boundary, not before.** "Tool descriptions advertise 4-field envelope" was flagged in 6+ quality scans across Phases 1–3. Each time, I correctly noted "this is the planned Phase 4.1 work" and deferred. When Phase 4.1 landed, the work was clean because the scope was crisp. The deferral pattern is correct when the scope is explicit; it would be incorrect for an undated/unplanned fix.
- **`#[tool(description=...)]` strings are production behavior, not docs.** This was already in CLAUDE.md as a quality lens, but Phase 4.1 made it operationally vivid. Each of the 5 string rewrites was an act of communication design with a specific consumer (an LLM agent reading the tool list). The checklist (envelope shape verbatim, defaults + ceilings, paging resume protocol, length-vs-truncated warning) gave structure to what would otherwise be a fuzzy "make the description better" task.
- **Cold-read at phase end caught what task-level review couldn't.** The Major finding in 4.5 polish (design doc denying truncated/next_offset) wasn't a per-task miss — it was a structural mismatch between an old artifact's framing and a new artifact's content. Per-task scans evaluate "is this commit good?"; cross-section cold-read evaluates "do these N commits add up to a coherent document?"

## Impact on Subsequent Phases

- Phase 5 inherits agent-facing descriptions that LLMs can use correctly on the first call. The acceptance test in 5.2/5.3 doesn't need to compensate for misleading descriptions because the descriptions now name the 6-field envelope and paging-resume protocol directly.
- Phase 5 also inherits the documented `[response].max_bytes` knob — the acceptance test uses `DEFAULT_RESPONSE_MAX_BYTES` from the public `code-graph-core` API rather than a magic number.
- The Pagination design doc's Decisions 8–13 are now the canonical record of Phase 1–3 architectural choices. Future planners exploring "why byte budget" land in the design doc, not the execution plan.

## Skill Opportunities

### Tool description audit
- **What you did repeatedly:** Phase 4.1's work was 5 separate rewrites following the same 7-item checklist (envelope shape, args + defaults/ceilings, verbs, paging-resume, length-vs-truncated, byte-budget cap source, count_only-if-applicable). The 4.1 polish then audited `detect_cycles` against the same checklist and found it stale. The audit logic is mechanical.
- **Where it belongs:** A `/sdd-planner:audit-tool-descriptions` skill OR a Rust test that fails when descriptions diverge from a canonical envelope reference (e.g., parse `Page<T>` definition, verify each tool's description names all fields).
- **Why a skill:** The "tool description still 4-field envelope" stale-finding noise across 6+ scans suggests the codebase needs an automated check, not just a human checklist. A test that pattern-matches the description against the current envelope shape would catch drift at PR time, not at planner-skill time.
- **Rough shape:** Input — none (or `(tool_name)` to scope). Output — a list of `(tool, deviation_from_canonical)` pairs. Invocation — as a `#[cfg(test)]` test in `crates/code-graph-tools/src/server.rs`, OR as a `/sdd-planner:audit-tool-descriptions` slash command during planning.

### Documentation-read-cold consolidated scan
- **What you did repeatedly:** After 4.2/4.3/4.4 doc-only commits, dispatched a single quality-scanner reading the full touched-doc surface area through the "Documentation read cold" lens. Caught a Major + 2 Minors that per-task scans missed.
- **Where it belongs:** A new `/sdd-planner:cold-read` skill, OR an option to `/sdd-planner:code-review` that scopes the four-lane review to docs only with the cold-read lens emphasized.
- **Why a skill:** Per-task scans are a wrong granularity for cross-section consistency. The cold-read perspective is fundamentally different: read the doc as a future agent would, without context from individual commits, and ask whether the parts add up to a coherent whole.
- **Rough shape:** Input — `(commit_range OR file_glob)`. Output — a quality-scanner findings table emphasizing framing contradictions, dangling references, stale examples. Invocation — at end of any phase whose primary deliverable was docs, or after any docs-heavy commit series.

### Stale-finding dedup
- **What you did repeatedly:** Each time the same finding surfaced in a quality scan ("tool descriptions are stale" — 6+ times across Phases 1–3), I had to remember "this is Phase 4.1's planned work" and tell the user "defer per plan." The mental tracking is reliable for one scope-boundary but degrades with multiple parallel deferrals.
- **Where it belongs:** A `/sdd-planner:track-findings` skill that maintains a `notes/deferred-findings.md` per plan, listing findings flagged with "defer because Phase X owns the fix" + the issue tracker / task ID. Subsequent scans cross-reference and emit "(already deferred to Phase X)" rather than re-listing the finding.
- **Why a skill:** When 6+ scans return the same finding across 3 phases, each scan invocation incurs cost (LLM tokens, reviewer attention) without delivering new signal. Dedup would compress the noise.
- **Rough shape:** Input — quality-scan findings table + plan path. Output — a deduplicated table with `(already deferred)` annotations. Invocation — automatic in `/sdd-planner:implement` after each quality scan.
