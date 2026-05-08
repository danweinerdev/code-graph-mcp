---
title: "C++ Macro-Strip — recover class extraction from API-export macros"
type: plan
status: complete
created: 2026-05-07
updated: 2026-05-07
tags: [cpp, tree-sitter, ue, unreal-engine, parser, config]
related:
  - Designs/CppMacroStrip
  - Plans/Complete/RustRewrite/01-Foundation-And-Cpp-Parser.md
phases:
  - id: 1
    title: "Foundation — CppConfig + strip_macros algorithm"
    status: complete
    doc: "01-Foundation.md"
  - id: 2
    title: "Wire-through — preprocess hook + call sites + integration"
    status: complete
    doc: "02-Wire-Through.md"
    depends_on: [1]
  - id: 3
    title: "Fixture, snapshot, docs, cutover"
    status: complete
    doc: "03-Fixture-And-Docs.md"
    depends_on: [2]
---

# C++ Macro-Strip — recover class extraction from API-export macros

## Overview

Restores C++ class extraction for declarations like `class CORE_API MyClass : public UObject {};`. Tree-sitter-cpp's grammar (v0.23.4, final release) parses the API-export macro into the `name: (type_identifier)` slot, leaves the rest as an `ERROR` node, and the existing `has_error()` guard correctly drops the broken capture — so the class is invisible to every downstream tool. This plan implements the design at `Designs/CppMacroStrip`: a `[cpp].macro_strip` config field listing identifier tokens to remove from C++ source bytes (replaced with same-length spaces, preserving offsets) before tree-sitter parses.

The user-reported failure is the second half of the issues observed on a generic UE project (the first half was the `get_orphans` token-bloat, closed by PaginationOverhaul). After this plan ships, `get_class_hierarchy`, `get_callers`, `get_callees`, `get_orphans { kind: class }`, and `generate_diagram` all work correctly on UE codebases for users who opt in via `macro_strip`.

Single-PR delivery (commit-per-phase) following the PaginationOverhaul cadence.

## Architecture

```mermaid
graph TD
    P1[Phase 1: CppConfig + strip_macros<br/>algorithm-only, unit-tested in isolation] --> P2[Phase 2: preprocess hook<br/>+ indexer + watch handler<br/>+ end-to-end integration test]
    P2 --> P3[Phase 3: UE fixture<br/>+ snapshot test<br/>+ documentation cutover]
```

The work touches three crates:

- `crates/codegraph-core` — Phase 1: new `CppConfig` struct + `RootConfig::cpp` field + empty-string filter at config-load.
- `crates/codegraph-lang` — Phase 2: `preprocess` hook added to `LanguagePlugin` trait with default impl.
- `crates/codegraph-lang-cpp` — Phase 1 (substitution algorithm) + Phase 2 (override `preprocess`).
- `crates/codegraph-tools` — Phase 2: indexer + watch handler call-site updates + end-to-end test.

Plus a new fixture (`testdata/ue/MyActor.h`) and documentation surfaces (`CLAUDE.md`, sample `.code-graph.toml`, `lib.rs` doc comments) in Phase 3.

## Key Decisions

Decisions are owned by the design (`Designs/CppMacroStrip`); the plan inherits them verbatim:

1. **Pre-parse byte substitution, not query-level recovery.** The broken AST has no well-formed sibling structure to query against.
2. **Literal token list, not regex.** Explicit user control; no false positives on real identifiers like `OPENAL_API`.
3. **Replace with spaces (same byte count).** Preserves all line/column reporting in extracted symbols.
4. **`preprocess` hook on `LanguagePlugin` with default impl, NOT a `parse_file` signature change.** Strictly additive trait extension; only the C++ plugin overrides; three production plugins and two test stubs need zero changes.
5. **Empty default; opt-in only.** A non-empty default would silently strip identifiers in non-UE codebases that happen to use the same names.
6. **Single flat `[cpp]` section.** YAGNI on pre-emptive nesting.
7. **Empty-string macro entries filtered at config-load** (not `debug_assert`) — empty pattern would infinite-loop the substitution loop in production.

## Dependencies

- **Existing `RootConfig` + `LanguagePlugin` infrastructure** — pagination-style additive extension, no new external dependencies.
- **`insta` snapshot suite** for the new UE-fixture snapshot in Phase 3.
- **No external blockers.** Design `Designs/CppMacroStrip` is in `review`; this plan starts only after design status flips to `approved`.
