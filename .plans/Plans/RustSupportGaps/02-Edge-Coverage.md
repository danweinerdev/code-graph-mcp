---
title: "Edge Coverage"
type: phase
plan: RustSupportGaps
phase: 2
status: complete
created: 2026-05-18
updated: 2026-05-21
tags: [rust, parser, edges, includes, inherits]
related: [Plans/RustSupportGaps/README.md, Designs/RustSupportGaps/README.md]
deliverable: "Rust `mod` declarations modeled as file-level Includes edges (reviving get_dependencies / detect_cycles / generate_diagram(file=)); trait supertrait bounds modeled as Inherits edges. Additive — no CACHE_VERSION bump."
tasks:
  - id: "2.1"
    title: "Parser emits provisional mod-declaration Includes edges (issue 1)"
    status: complete
    verification: "Each `mod foo;` and `mod foo { … }` produces one Includes edge with from = declaring file path, to = bare modname token, line = the mod declaration's source line; inline `mod foo {}` whose body is in the same file resolves to a self-edge and is suppressed (no self Includes); use/extern crate edge emission is unchanged."
  - id: "2.2"
    title: "RCMM mod→file resolution in post_index + stateless resolve_include pass-through (issue 1)"
    status: complete
    verification: "In post_index, (declaring_file, modname) resolves via Rust module rules — sibling dir/foo.rs, then dir/foo/mod.rs, then #[path] override — and edge.to is rewritten to that absolute indexed path, or the edge is removed if no candidate is in the FileIndex; Rust resolve_include returns Some(path) verbatim for an indexed absolute path and None otherwise; behavioral: `mod foo;`+foo.rs → one lib.rs→foo.rs edge; foo/mod.rs layout resolves; #[path] resolves; mod with no indexed file → no edge; a `a mod b; b mod a;` fixture yields a real detect_cycles cycle; get_dependencies(lib.rs) and generate_diagram(file=lib.rs) non-empty; `use std::io;` still produces NO surviving edge (scope boundary held); resolve_all_edges_drops_include_to_non_source_target stays green."
    depends_on: ["2.1"]
  - id: "2.3"
    title: "Supertrait Inherits edges via separate supertrait_query (issue 6)"
    status: complete
    verification: "A new standalone supertrait_query (not a second pattern in inh_query) matches trait_item with a bounds/type_bound_clause field; a second independent loop in extract_inheritance emits one Inherits edge (from=sub_trait, to=super_trait) per nameable bound; `trait Sub: Super` → Inherits Sub→Super; `trait S: A + B` → two edges; lifetime/`?Sized`/marker bounds filtered; inherent_impl_produces_no_inheritance_edge and trait_impl_produces_one_inheritance_edge stay green; get_class_hierarchy_for_rust_trait pre-flight performed (leaf → stays green; supertrait fixture → expectation update is recorded as a deliberate change, not a regression)."
  - id: "2.4"
    title: "Phase 2 hardening: structural gate, CLAUDE.md in-phase edits"
    status: complete
    verification: "make lint, make fmt-check, make test, make snapshot-clean green; intentional snapshot changes cargo-insta-accepted; CLAUDE.md in-phase edits land: Rust `mod` decls now emit file Includes edges, supertrait Inherits edges supported, `use`/`extern crate` explicitly scoped-out (intentional, not an accident), and the implicit 'Rust has no Includes' cross-cutting limitation removed/replaced; no sibling-section contradiction introduced this phase."
    depends_on: ["2.1", "2.2", "2.3"]
---

# Phase 2: Edge Coverage

## Overview

Additive edge coverage built on the Phase 1 RCMM. Models the intra-crate module tree
(`mod` declarations) as file-level `Includes` edges — reviving `get_dependencies`,
`detect_cycles`, and `generate_diagram(file=)` for Rust — and trait supertrait bounds as
`Inherits` edges. No cache-shape change; the Phase 1 CACHE_VERSION-4 re-index materializes
these edges with no `force=true`. Implements design Decisions 3 (`mod`-edge half) and 6.

Depends only on Phase 1. May be implemented concurrently with Phase 3.

## 2.1: Parser emits provisional mod-declaration Includes edges (issue 1)

### Subtasks
- [x] Parser emits one `Includes` edge per external `mod foo;` via new `MOD_DECL_QUERIES` + `extract_mod_decls`; `from = declaring file path`, `to = bare modname token`, `line = mod_item start row`.
- [x] Handles both forms: external `mod foo;` emits edge; inline `mod foo { … }` suppressed via `body`-field discriminator.
- [x] Inline self-edge suppression at emission time, not resolution time.
- [x] `use`/`extern crate` edge emission untouched (`extract_uses` not modified).
- [x] Quality-scanner follow-ups (commit 86cdcdd): added `mod_inline_outer_external_inner_emits_edge_for_inner_only` baseline test for 2.2 + `mod_empty_inline_block_does_not_emit_includes_edge` boundary test; reworded docstring forward-reference to prose; updated `mod_only.rs` fixture header to reflect the 2 provisional Includes edges.

### Notes
Provisional `to` is a bare token; resolution to a file path happens in 2.2 inside
`post_index` (design Decision 3, the Critical-1/2 fix). Add/extend a `queries.rs` pattern
for `mod_item` if needed without disturbing existing definition queries.

## 2.2: RCMM mod→file resolution + stateless resolve_include pass-through (issue 1)

### Subtasks
- [x] `RustParser::post_index` resolves provisional `mod` Includes via priority chain: `#[path]` → sibling `dir/foo.rs` → `dir/foo/mod.rs`. Re-reads source + tree-sitter scans for `#[path]` attribute + inline-nested-flag (option c in the brief).
- [x] Probes `FileIndex` via new `contains_path(&Path) -> bool` API; rewrites `edge.to` to absolute indexed path on hit, removes edge on miss.
- [x] Rust `resolve_include` override implemented as stateless pass-through: `Some(path)` for absolute indexed paths, `None` otherwise. `use`/`extern crate` dotted tokens drop at this layer.
- [x] Behavioral coverage: sibling resolution, mod.rs layout, `#[path]` override (incl. chained attributes), no-indexed-file dropped, inline-nested v1 limitation pinned, real `detect_cycles` cycle, `get_dependencies`/file-diagram non-empty, `use std::io;` still drops, `extern crate` still drops.
- [x] Quality-scanner follow-ups (commit 6b5389f): defensive `else { return; }` guard in `collect_mod_decls` for nameless `mod_item` (ERROR-node case); test for `#[path]`-target-not-indexed (no silent fallback to sibling); test for `contains_path` with paths having no `file_name()`; cross-reference comment on duplicated `build_test_file_index`; in-code watch-mode O(N) I/O cost note + future optimization options.

### Notes
This unifies design review Criticals 1 & 2: no `FileIndex` exact-path map, no RCMM state
surviving `post_index`. Keep `resolve_all_edges_drops_include_to_non_source_target` green.

## 2.3: Supertrait Inherits edges via separate supertrait_query (issue 6)

### Subtasks
- [x] Added `SUPERTRAIT_QUERY` constant in `queries.rs` with `(_)` wildcard child capture; documented all three `trait_bounds` grammar arms (`_type` / `higher_ranked_trait_bound` / `lifetime`).
- [x] Second independent loop in `extract_inheritance` (separate `QueryCursor`, separate capture-name array, untouched `impl` loop). Filters by `Node::kind()` in `resolve_supertrait_bound`.
- [x] Lifetime + `?Sized` (`removed_trait_bound`) + unhandled-kind `_ => None` fallback; `type_identifier`/`scoped_type_identifier`/`generic_type`/`higher_ranked_trait_bound` extract base name.
- [x] `get_class_hierarchy_for_rust_trait` pre-flight: fixture `Greet` is a leaf, stays green trivially.
- [x] Quality-scanner follow-ups (commit edf3848): rewrote stale `DIAMOND_FIXTURE` comment to describe both loops; replaced MANIFEST.md forward-reference with past-tense behavioral prose (no plan pointers); added `get_class_hierarchy_for_rust_trait_with_multiple_supertraits` end-to-end integration test (3 distinct failure-shape discriminators for regressions in supertrait edge handling).

### Notes
Reuses `EdgeKind::Inherits` (no enum change). The existing `impl` loop is untouched so
inherent/trait-impl edge tests cannot regress. Independent of 2.1/2.2.

## 2.4: Phase 2 hardening — structural gate, CLAUDE.md in-phase edits

### Subtasks
- [x] `make lint`, `make fmt-check`, `make test`, `make snapshot-clean`, `make leak-scan` all green; no snapshot drift.
- [x] CLAUDE.md edits landed (commit 4bf78c3): Rust `mod` decls now emit file `Includes` edges (`#[path]` → sibling → `mod.rs`); supertrait `Inherits` supported; `use`/`extern crate` explicitly scoped-out at resolve_include; the implicit "Rust has no Includes" claim in the `get_dependencies` Response-shapes paragraph rewritten; Inline-nested-mod v1 limitation documented; cross-language summary table Rust Inheritance cell updated.
- [x] Sibling-section consistency confirmed (Supported vs Known limitations not contradictory; Response-shapes paragraph aligned with Supported bullets).
- [x] Quality-scanner style follow-ups (commit 8ef65f8): dropped `(v1)` qualifier from Limitation 6 heading (test name carries the suffix inline); trimmed cross-language table Rust cell to `✓ trait impl + super` for parallelism with Python's `✓ multi-base`.

## Acceptance Criteria
- [ ] `mod` declarations resolve to file-level `Includes` edges; `get_dependencies` / `detect_cycles` / `generate_diagram(file=)` are functional for a real Rust crate.
- [ ] `use`/`extern crate` still produce no surviving edge (scope boundary held).
- [ ] `trait Sub: Super` (and multi-bound) produce `Inherits` edges; bound filtering correct; inherent/trait-impl edge tests green.
- [ ] No CACHE_VERSION change; Phase 1's v4 re-index materializes these edges without `force=true`.
- [ ] Structural gate green; in-phase CLAUDE.md updated with no contradictions.
