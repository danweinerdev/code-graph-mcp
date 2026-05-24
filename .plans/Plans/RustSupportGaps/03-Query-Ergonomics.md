---
title: "Query Ergonomics"
type: phase
plan: RustSupportGaps
phase: 3
status: complete
created: 2026-05-18
updated: 2026-05-22
tags: [rust, query, callgraph, response-shape]
related: [Plans/RustSupportGaps/README.md, Designs/RustSupportGaps/README.md]
deliverable: "get_callers/get_callees return only resolved project symbols (parity with generate_diagram's filter); querying call edges on a non-callable symbol returns an actionable soft hint instead of a silent empty page."
tasks:
  - id: "3.1"
    title: "Resolved-only filter in Graph::callers/callees (issue 4)"
    status: complete
    verification: "A BFS hop is kept only if its resolved target id is a key in Graph::nodes (same predicate generate_diagram uses); unresolved raw tokens never enter the BFS visited set (depth ≥ 2 traversal of resolved neighbors stays faithful); get_callees on a fn calling Ok/Err/info/to_string + one project fn returns only the project fn; total/next_offset derived from the filtered set are self-consistent; a callable symbol whose only callees are unresolved → empty Page envelope (truncated:false,next_offset:null), NOT an error; callers_known_symbol_with_no_callers_returns_empty_envelope and callers_unknown_symbol_with_suggestions stay green (filtering never reclassifies a real symbol as unknown)."
  - id: "3.2"
    title: "Non-callable soft hint in get_callers/get_callees handler (issue 7)"
    status: complete
    verification: "Result empty AND symbol exists AND kind ∈ {Struct,Enum,Trait,Typedef,Field,Interface} → a CallToolResult SUCCESS carrying an advisory message naming the kind and pointing to get_class_hierarchy/get_symbol_detail (not an Err, per the core invariant); get_callers(struct_id) → soft hint; get_callers(fn_with_no_callers) → empty envelope unchanged; get_callers(unknown) → did-you-mean error unchanged; the new branch is gated on a kind set disjoint from the load-bearing empty-envelope/suggestion test fixtures."
    depends_on: ["3.1"]
  - id: "3.3"
    title: "Phase 3 hardening: structural gate, CLAUDE.md Response-shapes edit"
    status: complete
    verification: "make lint, make fmt-check, make test, make snapshot-clean green; intentional snapshot changes cargo-insta-accepted; CLAUDE.md Response-shapes section updated to (a) state get_callers/get_callees are now resolved-only, (b) document the non-callable soft-hint behavior, and (c) add a CallChain entry stating `file` = call site (edge source line) and `symbol_id` = definition site, which differ across crates at depth ≥ 2 (this is the design's required Response-shapes CallChain edit — Phase 4's 4.2 only verifies it, so it must be WRITTEN here alongside the behavior change); no sibling-section contradiction."
    depends_on: ["3.1", "3.2"]
---

# Phase 3: Query Ergonomics

## Overview

Graph/handler-local quality fixes. Brings `get_callers`/`get_callees` to parity with
`generate_diagram` by filtering unresolved std/macro noise, and replaces the silent
empty page on non-callable targets with an actionable hint. Implements design Decisions 7
and 8. Depends only on Phase 1 (so the filter is validated against post-Phase-1 symbols);
may be implemented concurrently with Phase 2.

## 3.1: Resolved-only filter in Graph::callers/callees (issue 4)

### Subtasks
- [x] Filter applied inside `Graph::bfs` (used by `Graph::callers`/`callees`) via new `Graph::is_resolved_node` predicate. Hop skipped BEFORE `visited.insert` so unresolved tokens never enter the visited set.
- [x] Same predicate now applied inside `diagram_call_graph`'s BFS expansion at both `visited.insert` sites (forward + reverse arms) — closed a residual sibling-path bug surfaced by the quality scanner; doc comment refined.
- [x] `total`/`next_offset` self-consistent post-filter (handler computes from `chains.len()` after the graph call).
- [x] Tests cover: noisy-callees (one resolved + multiple unresolved → only resolved); all-unresolved → empty envelope; symmetric callers-side filter; depth-2 `visited`-pollution discriminator with per-hop depth assertions (callgraph); diagram-side `visited`-pollution discriminator with `max_nodes` budget pressure (verified load-bearing by manual revert).
- [x] Quality-scanner follow-ups (commit 352823d): diagram BFS filter at both visited.insert sites; new `diagram_call_graph_unresolved_token_does_not_pollute_visited_at_depth_2` test; depth assertions on the callgraph test; `is_resolved_node` doc comment refined to name both BFS code paths + the post-BFS defense-in-depth in `mermaid_label`.

### Notes
Reuse the diagram's "present in `nodes`" predicate so the two tools stay consistent — the
explicit goal of the feedback. A callable symbol with only unresolved callees is still a
known symbol with zero resolved edges → empty envelope, never an error.

## 3.2: Non-callable soft hint in handler (issue 7)

### Subtasks
- [x] New branch in `callers_or_callees` keyed on `is_non_callable_kind(kind)` — set is `{Struct, Enum, Trait, Typedef, Interface}` (no `Field` variant in current `SymbolKind` enum; doc instructs future maintainers to extend the predicate when adding non-callable variants since `#[non_exhaustive]` gives no compile-time signal).
- [x] Hint message names the kind, the symbol basename, and routes to `get_class_hierarchy` (Struct/Enum/Trait/Interface) or `get_symbol_detail` (Typedef).
- [x] Callable-with-no-callers (incl. Phase 3.1's only-unresolved-callees case) → empty envelope unchanged. Unknown-symbol → did-you-mean error path unchanged.
- [x] Quality-scanner follow-ups (commit 30661ee): `is_non_callable_kind` doc warns about `#[non_exhaustive]` future-variant gap; article helper fixes `"a enum"`/`"a interface"` grammar; Typedef + Interface end-to-end tests added; `scripts/leak-scan.sh` DETECT regex tightened to catch `Phase-N` (hyphen) and ` (N.N)` (parenthesized) forms — surfaced and swept 13 pre-existing plan-pointer leaks across the workspace in one go.

### Notes
Gated strictly on a `SymbolKind` set disjoint from the load-bearing test fixtures, so
`callers_known_symbol_with_no_callers_returns_empty_envelope` and
`callers_unknown_symbol_with_suggestions` cannot regress.

## 3.3: Phase 3 hardening — structural gate, CLAUDE.md Response-shapes edit

### Subtasks
- [x] All 5 gates green (lint/fmt-check/test/snapshot-clean/leak-scan). No snapshot drift.
- [x] CLAUDE.md Response-shapes: new consolidated `get_callers`/`get_callees` bullet covers (a) resolved-only filter parity with `generate_diagram` (via `is_resolved_node` BFS-expansion predicate); (b) non-callable soft-hint behavior (kind set + per-kind alt-tool routing + `is_error: false` shape + full trichotomy); (c) `CallChain` field semantics (`symbol_id`=definition, `file`/`line`=callsite, depth-≥-2 cross-crate divergence, rsplit recovery rule). NOTE: 2.4 did NOT write the CallChain entry as the plan anticipated; 3.3 wrote it fresh.
- [x] Quality-scanner follow-ups (commit a3d06b0): alt-tool list updated to mention BOTH `get_class_hierarchy` AND `get_symbol_detail` for structural kinds; filter attribution corrected from `mermaid_label` (post-BFS defense-in-depth) to `is_resolved_node` (BFS-expansion primary gate).
- [x] Sibling-section consistency confirmed; no per-language sections still claim `get_callers`/`get_callees` includes unresolved tokens.

## Acceptance Criteria
- [ ] `get_callers`/`get_callees` return only resolved project symbols; behavior matches `generate_diagram`'s filter.
- [ ] `total`/pagination self-consistent on the filtered set; callable-no-resolved-callees → empty envelope, not error.
- [ ] Non-callable target → actionable soft-hint success; load-bearing empty-envelope and did-you-mean tests green.
- [ ] Structural gate green; in-phase CLAUDE.md Response-shapes updated with no contradictions.
