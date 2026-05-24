---
title: "Fixture, snapshot, docs, cutover"
type: phase
plan: CppMacroStrip
phase: 3
status: complete
created: 2026-05-07
updated: 2026-05-07
deliverable: "Hand-crafted `testdata/ue/MyActor.h` fixture + `response_get_class_hierarchy_ue_aactor.snap` proves end-to-end class extraction on UE-style code. CLAUDE.md, sample `.code-graph.toml`, and `lib.rs` doc comments document the new `[cpp].macro_strip` config — explicitly including the cache-invalidation nuance (changes to `macro_strip` require `analyze_codebase` with `force=true` to re-parse already-cached files)."
tasks:
  - id: "3.1"
    title: "Create UE-style fixture testdata/ue/MyActor.h"
    status: complete
    verification: "New file `testdata/ue/MyActor.h` contains 4–6 hand-crafted class declarations covering the spread of UE patterns: (a) at least one `<MODULE>_API`-prefixed class with single inheritance (`class CORE_API AActor : public UObject {};`); (b) at least one chained inheritance via API macros (`class ENGINE_API APawn : public AActor {};`, `class GAMEPLAY_API ACharacter : public APawn {};`); (c) at least one class with two prefix macros (`class FOO_API BAR_EXTRA UDoubleMacro : public AActor {};`); (d) at least one no-macro baseline class to confirm the fix doesn't break the unaffected case (`class UNoMacro : public AActor {};`). All classes are syntactically valid C++ that would compile if the API macros were `#define`d to nothing — confirmed via a minimal compile check (godbolt or `clang -fsyntax-only -DCORE_API= -DENGINE_API= …`). **The header is hand-crafted, not derived from real Epic source** — Epic licensing prevents vendoring; the patterns are from publicly documented UE conventions. Add a comment at the top of the file: `// Synthetic UE-style header — not actual UE code, hand-crafted for testing.`. New file `testdata/ue/.code-graph.toml` declares the matching `[cpp].macro_strip = [\"CORE_API\", \"ENGINE_API\", \"GAMEPLAY_API\", \"FOO_API\", \"BAR_EXTRA\"]`."
  - id: "3.2"
    title: "Add snapshot test response_get_class_hierarchy_ue_aactor"
    status: complete
    depends_on: ["3.1"]
    verification: "New snapshot test in `crates/codegraph-tools/tests/snapshot_responses.rs` (function `response_get_class_hierarchy_ue_aactor`) builds a fixture from `testdata/ue/MyActor.h` (analyze_codebase with the matching `.code-graph.toml`), then calls `get_class_hierarchy { class: \"AActor\", depth: 2 }` and snapshots the result. Snapshot asserts: `hierarchy.name == \"AActor\"`; `hierarchy.bases` contains `UObject`; `hierarchy.derived` contains `APawn`; `APawn`'s derived contains `ACharacter`; `truncated: false`; `total_nodes_seen` matches the unique-name count of the tree. **A second snapshot test** `response_get_class_hierarchy_ue_double_macro` covers the multi-macro case: `get_class_hierarchy { class: \"UDoubleMacro\" }` returns the class with `AActor` as parent (proving multi-macro stripping works end-to-end through the public tool surface, not just the Phase 1 unit test). Both snapshots approved via `cargo insta accept`. Existing 4 hierarchy snapshots (`engine`, `rust_trait_greet`, `go_interface_reader`, `python_dog`) show ZERO diff — they don't use `macro_strip` so they're unaffected."
  - id: "3.3"
    title: "Documentation cutover — CLAUDE.md, sample .code-graph.toml, lib.rs doc comments"
    status: complete
    depends_on: ["3.2"]
    verification: "Three documentation surfaces updated and consistent with each other. **CLAUDE.md Configuration section** documents the `[cpp]` section schema with: (a) the `macro_strip: Vec<String>` field; (b) example for UE users; (c) **explicit cache-invalidation note**: 'Changes to `macro_strip` between `analyze_codebase` calls do NOT retroactively re-parse files whose mtime is unchanged (the cache uses mtime-based stale checking). To apply a new `macro_strip` list to already-indexed files, re-run `analyze_codebase` with `force=true`.' **CLAUDE.md C++ Parser Limitations section** updated: the existing limitation list adds an entry noting macro-prefixed classes are now supported via `[cpp].macro_strip` config; the raw-string-delimiter caveat is documented as a known limitation (`R\"CORE_API(...)CORE_API\"` with `CORE_API` in `macro_strip` will corrupt the raw string). **Sample `.code-graph.toml`** at the repo root gains a commented-out `[cpp]` section with: (a) the suggested UE macro list (`CORE_API`, `ENGINE_API`, `UMG_API`, `SLATE_API`, `RENDERCORE_API`, `NIAGARA_API`, `ONLINESUBSYSTEM_API`, `GAMEPLAYABILITIES_API`); (b) a one-paragraph explanation of when to use it; (c) **the same cache-invalidation note inline**: 'After changing this list, re-run `analyze_codebase` with `force=true` to invalidate the mtime-based cache.' **`crates/codegraph-lang-cpp/src/lib.rs`** doc comments where they describe parser behavior (the module-level doc + `CppParser` struct doc + `LanguagePlugin` impl doc) updated to mention `preprocess`/`strip_macros`. The cache-invalidation nuance appears in TWO of the three surfaces (CLAUDE.md and the sample `.code-graph.toml`) — load-bearing for users who edit the list and wonder why nothing changed."
  - id: "3.4"
    title: "Final workspace verification"
    status: complete
    depends_on: ["3.3"]
    verification: "`cargo fmt --all --check` clean. `cargo clippy --workspace --all-targets -- -D warnings` clean. `cargo test --workspace` passes — full suite, no regressions. `cargo insta pending-snapshots` zero. `git diff --stat tests/snapshots/` shows the expected churn: the 2 new UE hierarchy snapshots created in 3.2 plus zero modifications to any existing snapshot. If any existing snapshot regenerated unexpectedly (especially `engine.cpp` hierarchy), investigate — that signals an accidental cross-effect. **Cache-invalidation note grep check (load-bearing per user requirement):** `grep -l 'force=true' CLAUDE.md` and `grep -l 'force=true' .code-graph.toml` (or wherever the sample TOML lives in the repo root) must BOTH return a hit. The exact phrase 'force=true' must appear in both files. This catches accidental removal during reformat or last-minute doc edits — the cache-invalidation nuance is the user-flagged must-have for this plan and any commit lacking it does not satisfy Phase 3. **Optional manual smoke**: index a public C++ codebase with no UE macros (e.g., a small open-source project) — confirm parse output unchanged from pre-plan baseline. **Optional UE smoke**: if a UE-licensed user is available, point them at a real UE module with the suggested `macro_strip` list and confirm `get_class_hierarchy { class: \"AActor\" }` returns a populated tree."
tags: [cpp, tree-sitter, ue, unreal-engine, parser, config]
---

# Phase 3: Fixture, snapshot, docs, cutover

## Overview

The user-facing payoff. Phase 1 proved the algorithm; Phase 2 proved the wiring; Phase 3 proves the end-to-end story to a UE user opening `.code-graph.toml` for the first time. The fixture provides a regression anchor; the snapshot tests prove the public tool surface (`get_class_hierarchy`) returns correct results on macro-prefixed classes; the documentation tells users how to opt in AND warns them about the cache-invalidation gotcha that would otherwise make adding a macro to the list look like a no-op.

The cache-invalidation note is load-bearing. A user who adds `CORE_API` to `macro_strip`, runs `analyze_codebase` (without `force=true`), and sees no new symbols would conclude the feature is broken. Documenting this in BOTH `CLAUDE.md` AND inline in the sample `.code-graph.toml` is intentional duplication — users find one or the other.

## 3.1: Create UE-style fixture testdata/ue/MyActor.h

### Subtasks
- [x] Create `testdata/ue/` directory
- [x] Write `testdata/ue/MyActor.h` with 4–6 representative classes:
  ```cpp
  // Synthetic UE-style header — not actual UE code, hand-crafted for testing.
  class UObject {};  // forward declaration / base
  class CORE_API AActor : public UObject {
      void Tick(float DeltaTime);
  };
  class ENGINE_API APawn : public AActor {
      void SetupPlayerInputComponent();
  };
  class GAMEPLAY_API ACharacter : public APawn {};
  class FOO_API BAR_EXTRA UDoubleMacro : public AActor {};
  class UNoMacro : public AActor {};
  ```
- [x] Write `testdata/ue/.code-graph.toml`:
  ```toml
  [cpp]
  macro_strip = ["CORE_API", "ENGINE_API", "GAMEPLAY_API", "FOO_API", "BAR_EXTRA"]
  ```
- [x] Confirm the file compiles as valid C++ (e.g., paste into godbolt with the API macros `#define`d to nothing) — sanity check that the syntax is well-formed before relying on tree-sitter

### Notes
The `UObject` forward declaration in the fixture is for graph completeness — without it, the `Inherits` edges from `AActor → UObject` resolve to a dangling reference. Including it as a stub class keeps the graph well-formed for the snapshot. No method bodies are needed; we're testing class extraction, not method extraction.

## 3.2: Add snapshot test response_get_class_hierarchy_ue_aactor

### Subtasks
- [x] Add fixture-builder function in `crates/codegraph-tools/tests/snapshot_responses.rs` (or extend an existing builder pattern) that points `analyze_codebase` at `testdata/ue/`
- [x] Add `response_get_class_hierarchy_ue_aactor` test — calls `get_class_hierarchy { class: "AActor", depth: 2 }` and snapshots
- [x] Add `response_get_class_hierarchy_ue_double_macro` test — calls `get_class_hierarchy { class: "UDoubleMacro" }` and snapshots
- [x] Run `cargo test -p codegraph-tools --test snapshot_responses` to generate `.snap.new` files
- [x] Inspect via `cargo insta review`; confirm snapshots show:
  - `AActor` snapshot: bases include `UObject`; derived includes `APawn`; APawn's derived includes `ACharacter`; `truncated: false`
  - `UDoubleMacro` snapshot: bases include `AActor`; `truncated: false`
- [x] Approve via `cargo insta accept`
- [x] Confirm via `git diff --stat tests/snapshots/` that ONLY the 2 new snapshots appear; no existing snapshot regenerates

### Notes
The four existing hierarchy snapshots (`engine`, `rust_trait_greet`, `go_interface_reader`, `python_dog`) must show zero diff. They don't use `macro_strip` so they shouldn't be affected. If they regenerate, something is wrong with the default `preprocess` impl (it shouldn't allocate or change bytes for the default case).

## 3.3: Documentation cutover — CLAUDE.md, sample .code-graph.toml, lib.rs doc comments

### Subtasks
- [x] **`CLAUDE.md` Configuration section:** add `[cpp]` to the schema documentation. Include:
  - The `macro_strip: Vec<String>` field with type and default (`[]`)
  - A brief example for UE users (3–4 lines)
  - A standalone subsection or callout titled "Cache invalidation": "Changes to `macro_strip` between `analyze_codebase` calls do NOT retroactively re-parse files whose mtime is unchanged (the cache uses mtime-based stale checking). To apply a new `macro_strip` list to already-indexed files, re-run `analyze_codebase` with `force=true`."
- [x] **`CLAUDE.md` C++ Parser Limitations section:** **add a NEW entry** for macro-prefixed class declarations being supported via `[cpp].macro_strip`. **Leave the existing "Macro-generated definitions" limitation entry unchanged** — that limitation (macros that expand to whole function definitions, e.g. `DEFINE_HANDLER(name)`) is a different pattern and is NOT fixed by this plan. Conflating the two would falsely imply the macro-generated-definitions limitation is resolved. Also add a sub-note documenting the raw-string-delimiter limitation (`R"CORE_API(...)CORE_API"` with `CORE_API` in the strip list corrupts the raw string).
- [x] **Sample `.code-graph.toml`** at the repo root: add a commented-out `[cpp]` section with the suggested UE macro list (per design's Migration section), the brief usage explanation, AND **inline cache-invalidation note**: `# After changing this list, re-run analyze_codebase with force=true to invalidate the mtime-based cache.`
- [x] **`crates/codegraph-lang-cpp/src/lib.rs`** module-level doc + `CppParser` struct doc + `LanguagePlugin` impl doc updated to mention `preprocess` / `strip_macros` and reference the `[cpp].macro_strip` config

### Notes
The cache-invalidation note appears in TWO places (CLAUDE.md AND the sample `.code-graph.toml` inline comment). This is intentional duplication — users find one surface or the other depending on how they discover the feature. The sample TOML is what they edit; the CLAUDE.md is what an agent reads to understand the project. Both must agree on the wording: "re-run `analyze_codebase` with `force=true`."

## 3.4: Final workspace verification

### Subtasks
- [x] `cargo fmt --all --check` clean
- [x] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [x] `cargo test --workspace` passes
- [x] `cargo insta pending-snapshots` reports zero
- [x] `git diff --stat tests/snapshots/` shows: 2 new UE hierarchy snapshots, ZERO modifications to existing snapshots
- [x] Optional UE smoke: if access to a UE codebase is available, point `code-graph-mcp` at it with the suggested `macro_strip` and confirm `get_class_hierarchy { class: "AActor" }` returns a populated tree

### Notes
This is the final gate. If anything regenerates that shouldn't, do not approve — investigate. The plan's whole correctness story rests on "the default `preprocess` impl is a true no-op for non-C++ files and for C++ files with empty `macro_strip`." Any unexpected snapshot churn falsifies that claim.

## Acceptance Criteria

- [x] `testdata/ue/MyActor.h` and `testdata/ue/.code-graph.toml` exist with the documented contents
- [x] 2 new UE hierarchy snapshots added and approved (`response_get_class_hierarchy_ue_aactor`, `response_get_class_hierarchy_ue_double_macro`)
- [x] All 4 existing hierarchy snapshots show zero diff
- [x] `CLAUDE.md` documents the `[cpp]` schema, the cache-invalidation nuance, and adds a NEW C++ Limitations entry (without modifying the existing macro-generated-definitions entry)
- [x] Sample `.code-graph.toml` has the commented-out UE macro list with inline cache-invalidation note
- [x] `crates/codegraph-lang-cpp/src/lib.rs` module-level doc, `CppParser` struct doc, and `LanguagePlugin` impl doc updated to mention `preprocess`/`strip_macros` and the `[cpp].macro_strip` config
- [x] **Cache-invalidation note grep verified:** the exact phrase `force=true` appears in both CLAUDE.md AND the sample .code-graph.toml — checked in 3.4's structural verification
- [x] `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace` clean
- [x] `cargo insta pending-snapshots` zero
