---
title: "Symbol Model & Crate Module Foundation"
type: phase
plan: RustSupportGaps
phase: 1
status: complete
created: 2026-05-18
updated: 2026-05-21
tags: [rust, parser, namespace, hierarchy, cache]
related: [Plans/RustSupportGaps/README.md, Designs/RustSupportGaps/README.md]
deliverable: "RCMM + post_index hook; crate-qualified Rust namespaces; trait methods (default + abstract) classified as Method/parent=trait; hierarchy collision dedup; CACHE_VERSION 3→4 with silent re-index."
tasks:
  - id: "1.1"
    title: "Add post_index hook to LanguagePlugin + wire BOTH re-index call sites (analyze + watch)"
    status: complete
    verification: "Trait stays object-safe (Box<dyn LanguagePlugin> storage compiles); default body is a no-op with underscored params (clippy -D warnings clean); every non-Rust plugin unaffected and all existing non-Rust tests green; a LanguageRegistry::plugins() iterator (yielding &dyn LanguagePlugin over the private plugins map) is added and used at the call sites. post_index runs on BOTH re-index paths: (a) the analyze path — after index_directory returns its full freshly-parsed Vec<FileGraph> (which on every analyze re-index already contains ALL files, so the complete-set invariant holds without any cache merge), verified by a multi-file Cargo fixture where every file's symbols carry rewritten namespaces; (b) the watch path — inside handlers/watch.rs try_reindex_file, post_index runs over the existing+newly-parsed graph set before the resolve_include/resolve_call loop, verified by extending watch_rust_reindex to assert a single-file watch re-index yields crate-qualified namespaces (not stale empty/inline-only ones)."
  - id: "1.2"
    title: "RCMM core: Cargo.toml discovery + canonical module-path computation"
    status: complete
    verification: "Pure-logic unit tests cover: lib.rs/main.rs → crate_name; src/foo.rs → crate::foo; src/foo/mod.rs → crate::foo; src/a/b.rs → crate::a::b; crate name '-' → '_'; #[path=\"x.rs\"] override; inline mod nesting composes; no Cargo.toml → empty-prefix fallback (no panic); malformed Cargo.toml → crate skipped with eprintln!, index continues."
    depends_on: ["1.1"]
  - id: "1.3"
    title: "Crate-qualified namespace assignment in RustParser::post_index (issue 3)"
    status: complete
    verification: "Symbol in src/reactor.rs of crate ark-core → namespace ark_core::reactor; src/a/b.rs → ark_core::a::b; mod.rs collapses; inline `mod tests` composes onto the crate prefix and nested_mods_produce_namespace_a_b_c passes adapted to the crate prefix; no-Cargo.toml symbols still render <global> via get_symbol_summary; post_index stores nothing on &self (no interior mutability)."
    depends_on: ["1.1", "1.2"]
  - id: "1.4"
    title: "Trait method classification + abstract signature extraction (issue 2)"
    status: complete
    verification: "find_enclosing_trait added; a function_item OR function_signature_item whose nearest definition ancestor is trait_item (not impl_item) → SymbolKind::Method, parent=trait_name, no new SymbolKind variant; default method and abstract `fn f(&self);` both produce a Method symbol; top_level_only:true drops them; trait_impl_method_parent_is_type_not_trait and call_inside_trait_impl_method_has_type_qualified_from_not_trait stay green; trait_item_produces_trait_kind is REPLACED (intentional inversion) with a test asserting abstract signature → Method/parent=Trait."
  - id: "1.5"
    title: "Hierarchy collision dedup (issue 5) + CACHE_VERSION 3→4 bump (D10)"
    status: complete
    verification: "build_hierarchy dedups the collected bases/derived target list by target-name string, first-encountered wins; a struct impl-ing a trait once → exactly one base (no [{X},{X}] from a tree-sitter duplicate-match artifact); legitimately distinct traits sharing a bare name still collapse — accepted per design Decision 5, because Inherits edges carry raw unqualified text (edge.to = trait_text), so the dedup removes only duplicate-match artifacts, not genuine distinct relationships; diamond ref:true contract preserved (get_class_hierarchy diamond test green); CACHE_VERSION = 4; a v3/v2 cache fails the version check → Graph::load returns Ok(false) → silent re-index, no force=true, no migration; a freshly written v4 cache round-trips."
    depends_on: ["1.3", "1.4"]
  - id: "1.6"
    title: "Phase 1 hardening: structural gate, CLAUDE.md in-phase edits, baseline re-measure"
    status: complete
    verification: "make lint, make fmt-check, make test, make snapshot-clean all green; intentional Rust snapshot changes cargo-insta-accepted (no *.snap.new); external/ripgrep baseline re-measured and testdata/rust/ripgrep-baseline.txt symbols:/tag:/commit: headers updated in the same commit (symbol count shifts from abstract-signature extraction + namespace); CLAUDE.md updated in-phase for issues 2 & 3 and the CACHE_VERSION-4 cache-invalidation sentence, with no contradiction left in sibling sections touched this phase."
    depends_on: ["1.1", "1.2", "1.3", "1.4", "1.5"]
---

# Phase 1: Symbol Model & Crate Module Foundation

## Overview

The spine phase. Introduces the `post_index` extension point and the Rust Crate Module Model
(RCMM), then uses them to fix the symbol-shape defects (issues 2, 3, 5) that force the
CACHE_VERSION bump. Phases 2–4 build on the hook and model landed here. Implements design
Decisions 1, 2, 4, 5, 10 and the namespace half of Decision 3.

## 1.1: Add post_index hook to LanguagePlugin + wire BOTH re-index call sites (analyze + watch)

### Subtasks
- [x] Add `fn post_index(&self, _graphs: &mut [FileGraph], _file_index: &FileIndex) {}` to the `LanguagePlugin` trait (`crates/code-graph-lang/src/lib.rs`, near `resolve_include`), default no-op, doc comment per design Interfaces block.
- [x] Confirm object safety: no generics, no `Self` return; `Box<dyn LanguagePlugin>` storage still compiles.
- [x] Add a `LanguageRegistry::plugins()` iterator (e.g. `self.plugins.values().map(|b| b.as_ref())`) yielding `&dyn LanguagePlugin` — the private `plugins` map has no iteration API today and the call sites need one.
- [x] **Analyze path:** in `index_directory` (`crates/code-graph-tools/src/indexer.rs`), after the rayon parse loop produces the full `Vec<FileGraph>`, build one `FileIndex` over it and loop `registry.plugins()` calling `post_index(&mut graphs, &file_index)` before returning. (No cache merge exists here — a stale analyze does a full re-parse of every file, so the returned vector IS the complete set; the design's "merged set" wording was inaccurate and is corrected, see Notes.)
- [x] **Watch path:** in `crates/code-graph-tools/src/handlers/watch.rs` `try_reindex_file`, after the existing+newly-parsed graph set and its `FileIndex` are built (~lines 368–376) and BEFORE the `resolve_include`/`resolve_call` loop, call `post_index` over that set so a single-file watch re-index gets crate-qualified namespaces and resolved `mod` edges (Phase 2). Without this, watch re-index silently regresses Rust namespaces to `""`/inline-only.
- [x] Ensure `resolve_all_edges` continuing to rebuild its own `SymbolIndex`/`FileIndex` afterward is harmless (it now sees post-processed graphs).
- [x] Quality-scanner follow-ups (commit a4d10dd): copy-back assert; trait-doc wording; `fg.path` immutability contract; shared `RecordingPlugin` helper; load-bearing copy-back round-trip test.

### Notes
Non-Rust plugins inherit the no-op; this task must not change any non-Rust behavior.
**Design correction (review Critical 1+2):** the approved design's Interfaces section said
`post_index` is "invoked at the end of `index_directory` … over the full merged set" — but
`index_directory` is a pure fresh-parse function with no cache integration, and the watch
handler is a *separate* re-index path the design never named. The intent ("every re-index
path runs `post_index` over its complete graph set") is preserved by wiring **both** call
sites above. The design doc's Interfaces paragraph has been corrected to match; this is a
factual fix, not a decision change. `watch_rust_reindex` currently does not assert on
namespace values, so the omission would otherwise fail silently.

## 1.2: RCMM core — Cargo.toml discovery + canonical module-path computation

### Subtasks
- [x] New module in `code-graph-lang-rust` (e.g. `crate_model.rs`): discover Cargo.toml among the indexed file set, map each crate `src` root → crate name (`[package].name`, `-`→`_`).
- [x] Compute each `.rs` file's canonical module path: root module (`lib.rs`/`main.rs`/bin root) → path-derived segments → `#[path]` overrides → inline `mod` composition. (bin-target roots deferred per implementer scope note; `#[path]` exposed as `with_path_overrides` seam for 1.3/2.2; inline `mod` composition is 1.3's job.)
- [x] Pure functions over an injected file list (no global FS walk beyond the indexed set) so it is unit-testable without a real workspace.
- [x] Fallback + error handling: no Cargo.toml → empty prefix (today's behavior); malformed Cargo.toml → skip crate, `eprintln!`, continue.
- [x] Quality-scanner follow-ups (commit 3d2bc8b): corrected `src/mod.rs` and `debug_assert` comments; added real depth-tiebreak test `inner_crate_under_outer_src_wins_via_depth_sort`.

### Notes
Keep this crate-aware logic entirely inside `code-graph-lang-rust`; the indexer stays
language-agnostic. This is the shared model Phase 2's `mod`→file resolution also consumes.

## 1.3: Crate-qualified namespace assignment in RustParser::post_index (issue 3)

### Subtasks
- [x] Implement `RustParser::post_index`: for each Rust `FileGraph`, compute the file's module path via 1.2 and overwrite each `Symbol.namespace`.
- [x] Compose inline-`mod` nesting onto the crate prefix; preserve `<global>` rendering for the empty/no-Cargo.toml case (handled downstream by `get_symbol_summary`).
- [x] Store nothing on `&self` — all work writes into the `graphs` slice in place.
- [x] Adapt `nested_mods_produce_namespace_a_b_c` (kept in fallback mode; added new `post_index_composes_deeply_nested_inline_mods_onto_crate_prefix` for the composed-path counterpart).
- [x] Lifted module-wide `#![allow(dead_code, …)]` from crate_model.rs; narrow per-item allows on `CrateInfo.root` and `with_path_overrides`.
- [x] Quality-scanner follow-ups (commit 029e09d): Cargo.toml plan-pointer fix; widened leak-scan default glob; strengthened statelessness test (`post_index_does_not_leak_state_between_calls_on_different_crates`).

### Notes
Depends on 1.1 (hook + call site) and 1.2 (model). This is the namespace half of design
Decision 3; the `mod`-edge half is Phase 2.

## 1.4: Trait method classification + abstract signature extraction (issue 2)

### Subtasks
- [x] Added `find_enclosing_trait` + composite `find_nearest_def_ancestor` + `NearestDefAncestor` enum in `helpers.rs`.
- [x] Dispatch uses nearest-ancestor-wins; abstract `function_signature_item` symbols emit only when nearest = `Trait(name)`.
- [x] Abstract `fn f(&self);` inside trait → Method symbol with parent=trait; bare signatures (e.g. `extern "C"`) remain excluded.
- [x] `trait_item_produces_trait_kind` inverted/renamed to `abstract_trait_method_signature_produces_method_with_trait_parent`.
- [x] `trait_impl_method_parent_is_type_not_trait` + companion call test stay green.
- [x] Quality-scanner follow-ups (commit 36552d2): Major #1 fix — `enclosing_function_id` now uses `find_nearest_def_ancestor` (orphaned-call-edge bug); Major #2 — surgical CLAUDE.md update for the two contradicting Rust sentences (full sweep deferred to 1.6); MANIFEST.md/corpus.rs line numbers; `find_enclosing_trait`/`find_enclosing_impl` → `#[cfg(test)]`; helpers.rs and queries.rs comments updated. New regression test `call_inside_trait_default_method_has_trait_qualified_from`.

### Notes
No new `SymbolKind` variant (CLAUDE.md invariant). Trait identity rides the `Inherits` edge
(Phase 2) and the parent field. Independent of RCMM but in the same crate; can proceed in
parallel with 1.2/1.3.

## 1.5: Hierarchy collision dedup (issue 5) + CACHE_VERSION 3→4 (D10)

### Subtasks
- [x] In `build_hierarchy` (`crates/code-graph-graph/src/algorithms.rs`), dedup the collected bases/derived `Vec<String>` by target-name string before recursing; first-encountered wins (consistent with `visited_unique`/`on_path`). Via private `dedup_inherits_targets` helper applied at both `adj` (bases) and `radj` (derived) collection sites.
- [x] Confirm diamond `ref:true` contract still holds.
- [x] Bump `CACHE_VERSION` 3 → 4 (`crates/code-graph-graph/src/persist.rs`).
- [x] Confirm a stale (v2/v3) cache → `Graph::load` returns `Ok(false)` → silent re-index, no `force=true`, no migration attempted; a fresh v4 cache round-trips (test: `stale_v3_cache_returns_ok_false_silent_reindex`).
- [x] Quality-scanner follow-ups (commit 806b72d): `mtimes` omission comment; replaced Decision-5 citation with self-contained inline rationale; added combined `class_hierarchy_dedup_and_diamond_compose` test (verified load-bearing in both layers).

### Notes
The namespace fix (1.3) removes the *accidental* bare-name collisions at the root; this
dedup is defense-in-depth so the invariant holds even for legitimately repeated edges.
Cache bump is here because issues 2 (1.4) and 3 (1.3) change cached symbol shape.

## 1.6: Phase 1 hardening — structural gate, CLAUDE.md edits, baseline re-measure

### Subtasks
- [x] `make lint` (clippy `-D warnings`), `make fmt-check`, `make test --workspace`, `make snapshot-clean`, `make leak-scan` all green.
- [x] No `*.snap.new` produced across Phase 1 — no insta acceptance needed.
- [x] `make submodules` then re-measured `external/ripgrep`; baseline `symbols: 3104 → 3118` (+14 from abstract-trait-signature extraction), `tag:`/`commit:` unchanged (pinned `15.1.0`).
- [x] CLAUDE.md in-phase edits: Rust trait methods now Method/parent=Trait + abstract-signature exception (limitation 3 rewrite + cross-language summary row); crate-qualified namespace bullet added; CACHE_VERSION-4 cache-invalidation sentence added. Sibling-section contradictions resolved.
- [x] Quality-scanner follow-ups (commit 5211201): corrected false `#[path]` claim (seam exists but unused); enumerated v3 in the prior cache-bump bullet; dropped pre-Phase-1 plan-pointer; widened `scripts/leak-scan.sh` to scan `CLAUDE.md` (which immediately surfaced two pre-existing CppMacroStrip plan-pointers — surgically fixed).

### Notes
Per-phase doc edits keep the "Documentation read cold" lens satisfied incrementally; Phase 4
does the final cross-section sweep.

## Acceptance Criteria
- [ ] `post_index` hook exists, object-safe, default no-op; invoked over the full merged set in `index_directory`; no non-Rust behavior change.
- [ ] RCMM derives correct crate-qualified module paths for all rule cases incl. fallbacks/errors.
- [ ] Rust symbols carry crate-qualified namespaces; trait default + abstract methods are `Method`/parent=trait; hierarchy shows no duplicate bases.
- [ ] `CACHE_VERSION` is 4; stale caches silently re-index with no `force=true`.
- [ ] Full structural gate green; ripgrep baseline + in-phase CLAUDE.md updated in the landing commit(s).
