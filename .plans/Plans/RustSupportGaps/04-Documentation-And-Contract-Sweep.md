---
title: "Documentation & Contract Sweep"
type: phase
plan: RustSupportGaps
phase: 4
status: complete
created: 2026-05-18
updated: 2026-05-22
tags: [rust, docs, tool-descriptions, claude-md]
related: [Plans/RustSupportGaps/README.md, Designs/RustSupportGaps/README.md]
deliverable: "Agent-facing tool descriptions document CallChain field semantics and the search_symbols suggestions field; CLAUDE.md is internally consistent cold-read across every section this plan touched; full-suite green with only the intended deliberate test changes."
tasks:
  - id: "4.1"
    title: "Tool-description updates in server.rs (issues 8, 9)"
    status: complete
    verification: "get_callers/get_callees #[tool(description=…)] strings state that `file` is the call SITE and `symbol_id` is the DEFINITION site (they differ across crates at depth ≥ 2); search_symbols description documents `suggestions` (populated only when query is anchored ^…$ AND total==0, ≤5 candidate ids, absent from wire when empty, never on count_only=true); both pass the CLAUDE.md 'Agent-facing tool descriptions' checklist (named args w/ default+ceiling, envelope shape named, action verb matches effect); grep confirms the load-bearing phrases are present."
  - id: "4.2"
    title: "CLAUDE.md cold-read consistency sweep"
    status: complete
    verification: "Read modified AND surrounding sections cold: cross-language summary table Rust 'Forward decls excluded' row reflects the trait-signature exception; Rust limitation 2 prose rewritten to match (both sites consistent); Response shapes reflect resolved-only callers/callees + CallChain field semantics + suggestions; Cache invalidation states CACHE_VERSION 4 + silent re-index; the implicit 'Rust has no Includes' cross-cutting limitation is removed/replaced; `grep` confirms all load-bearing 'must contain phrase' strings (e.g. force=true cache lines) survived; no sibling section contradicts another."
    depends_on: ["4.1"]
  - id: "4.3"
    title: "Final full-suite verification & plan closeout"
    status: complete
    verification: "make lint, make fmt-check, make test --workspace, make snapshot-clean, make leak-scan all green on the integrated branch; every name in the design's must-stay-green anti-regression set passes; the ONLY intentional test changes are the documented deliberate ones (trait_item_produces_trait_kind inversion, nested_mods_produce_namespace_a_b_c crate-prefix adaptation, any get_class_hierarchy_for_rust_trait expectation update from Phase 2); ripgrep baseline headers match the pinned commit; no *.snap.new."
    depends_on: ["4.1", "4.2"]
---

# Phase 4: Documentation & Contract Sweep

## Overview

Closes the loop. Issues 8 and 9 are pure contract-surface fixes (agents pattern-match on
tool-description strings, so a missing field semantic is functionally a bug). Then a
cold-read consistency sweep over every CLAUDE.md section this plan touched, and a final
full-suite gate confirming only the intended deliberate test changes are present.
Implements design Decision 9. Depends on Phases 1–3 (documents their landed behavior).

## 4.1: Tool-description updates in server.rs (issues 8, 9)

### Subtasks
- [x] `get_callers`/`get_callees` descriptions: explicit `Page<CallChain>` envelope; CallChain field semantics (call site vs definition site); resolved-only filter parity with `generate_diagram`; non-callable soft-hint trichotomy.
- [x] `search_symbols` description: explicit `Page<SymbolResult>` envelope (flattened on wire); `suggestions: string[]` trigger conditions (anchored `^…$`, length ≥ 2, `total == 0`, ≤5 candidate ids); absent-when-empty contract; `count_only=true` exclusion.
- [x] Agent-facing-description checklist applied: envelope shape named explicitly, `offset` default + resume hint, action verb matches effect.
- [x] 8 new description-substring regression tests pin the load-bearing phrases; 3 snapshot files re-accepted for the intentional description byte changes.
- [x] Quality-scanner follow-ups (commit b595a9a): get_callees depth-1 framing rewritten to honestly state `file` diverges from `symbol_id`'s file at ALL depths whenever the callee lives elsewhere (NOT only at depth ≥ 2); structural-kinds alt-tool list updated in both get_callers and get_callees to mention BOTH `get_class_hierarchy` AND `get_symbol_detail` (matching what `alternative_tool_hint` emits).

### Notes
No code/behavior/cache/test change — description strings only. Phases 1–3 already updated
their own behavior-adjacent CLAUDE.md sections in-phase; this task is the agent-facing
`server.rs` surface specifically.

## 4.2: CLAUDE.md cold-read consistency sweep

### Subtasks
- [x] Cold-read sweep of all Plan-touched sections + siblings. All Phase 1+2+3+4.1 edits verified consistent; zero CLAUDE.md changes needed (commit ba119e9 is an empty verification marker).
- [x] Cross-language summary table Rust forward-decl exception ↔ Rust limitation prose: both sites consistent.
- [x] Response shapes verified (resolved-only callers/callees + CallChain field semantics + suggestions); Cache invalidation verified (CACHE_VERSION 4 + silent re-index of v1/v2/v3); "Rust has no Includes" implicit limitation cleanly removed.
- [x] `grep` confirmed 12 load-bearing phrases (force=true, Page<T>, Page<CallChain>, Page<SymbolResult>, is_resolved_node, CACHE_VERSION 4, 5-kind set, Rust mod declarations, trait method signatures excepted, etc.) survived; 0 plan-pointer leaks; 0 stale references.
- [x] **Out-of-scope finding flagged for follow-up**: the `count_only` Response-shapes paragraph (line 90) omits `get_symbol_summary` from its tools list, but the handler at `crates/code-graph-tools/src/handlers/symbols.rs:465-494` does support `count_only`. ResponseShapePolish-era surface issue, not RustSupportGaps-touched.

### Notes
This is the "Documentation read cold" lens applied repo-wide for the plan. Per-phase edits
already happened; this catches cross-section contradictions only a whole-plan view reveals.

## 4.3: Final full-suite verification & plan closeout

### Subtasks
- [x] All 5 `make` gates green on integrated `rust-main` (lint/fmt-check/test/snapshot-clean/leak-scan); 1344 individual tests, 0 failed.
- [x] Must-stay-green anti-regression list: all 24 named tests pass + `response_shape_acceptance.rs` (3 tests, all green) + `watch_rust_reindex.rs` (2 tests, all green) + 4 RecordingPlugin-using tests.
- [x] Only documented deliberate diffs present: `trait_item_produces_trait_kind` → `abstract_trait_method_signature_produces_method_with_trait_parent` (1.4 inversion); paired `trait_default_method_*` rename; 2.1 `mod_only` test contract inversion; mechanical `new_compiles_all_queries` rename; `nested_mods_produce_namespace_a_b_c` kept in fallback path; `get_class_hierarchy_for_rust_trait` assertion shape unchanged (Greet is leaf — new sibling test added in 2.3).
- [x] `testdata/rust/ripgrep-baseline.txt`: `symbols: 3118, tag: 15.1.0, commit: af60c2d` — matches submodule pin. No `*.snap.new` anywhere.
- [x] Verification commit: none — implementer correctly judged the verification log itself as the deliverable (matching 4.2's empty-marker-optional pattern).

## Acceptance Criteria
- [ ] Tool descriptions document CallChain field semantics and `suggestions`; agent-facing-description checklist satisfied.
- [ ] CLAUDE.md is internally consistent cold-read across all touched sections; load-bearing phrases survive.
- [ ] Full structural + anti-regression suite green; only the documented deliberate test changes present.
