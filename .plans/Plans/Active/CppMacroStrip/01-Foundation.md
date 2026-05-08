---
title: "Foundation — CppConfig + strip_macros algorithm"
type: phase
plan: CppMacroStrip
phase: 1
status: complete
created: 2026-05-07
updated: 2026-05-07
deliverable: "`CppConfig { macro_strip: Vec<String> }` lives in `RootConfig` with `#[serde(default)]` and an empty-string filter at config-load. `strip_macros(&[u8], &[String]) -> Cow<[u8]>` ships with the full algorithm-correctness unit test suite. Nothing in the parse pipeline calls strip_macros yet — this phase locks the algorithm before Phase 2 wires it through."
tasks:
  - id: "1.1"
    title: "Add CppConfig to RootConfig with empty-string filter"
    status: complete
    verification: "`crates/codegraph-core/src/config.rs` defines `pub struct CppConfig { #[serde(default)] pub macro_strip: Vec<String> }` with `#[derive(Debug, Default, Deserialize, Clone)]`. `RootConfig` gains `#[serde(default)] pub cpp: CppConfig`. **Empty-string filter:** either a custom `Deserialize` impl or a post-load normalization in `RootConfig::load` drains entries where `s.is_empty()` and emits a `tracing::warn!` per dropped entry. Tests cover: (a) absent `[cpp]` section resolves to default empty `macro_strip` (backward-compat); (b) `macro_strip = [\"\", \"CORE_API\", \"\"]` deserializes to `[\"CORE_API\"]`; (c) `macro_strip = []` deserializes cleanly and produces no warnings; (d) the existing tests in `config.rs` continue to pass byte-identical (the new field is additive)."
  - id: "1.2"
    title: "Implement strip_macros + algorithm-correctness unit tests"
    status: complete
    verification: "`strip_macros(content: &[u8], macros: &[String]) -> Cow<'_, [u8]>` lives in `crates/codegraph-lang-cpp/src/` (free function or `CppParser` associated function). Algorithm: empty list → `Cow::Borrowed`; otherwise mutable copy + per-macro multi-pass with whole-word boundary check (`is_ident_byte = b.is_ascii_alphanumeric() || b == b'_'`) + replace matched bytes with same-count spaces. Unit tests cover: (a) `class CORE_API MyClass : public UObject {};` with `[\"CORE_API\"]` → produces `MyClass` with `UObject` inherits edge; (b) `class FOO_API BAR_EXTRA MyClass : public Base {};` with both macros → `MyClass` extracts; (c) `class CORE_API MyClass {};` with empty `macro_strip` → zero symbols (preserves opt-in semantics); (d) `UCLASS()\\nclass CORE_API MyClass : public UObject {};` with `[\"CORE_API\"]` → both `UCLASS()` call edge AND `MyClass` symbol; (e) `void CORE_API DoThing();` with `[\"CORE_API\"]` → `DoThing` function symbol (free side effect); (f) ordinary string literal `\"CORE_API is great\"` → unaffected; (g) raw string literal `R\"(CORE_API in raw)\"` → unaffected (positive case; the unsafe raw-string-tag-matches-macro case is documented as a limitation, not a passing test); (h) `void CORE_API_helper() {}` with `[\"CORE_API\"]` → `CORE_API_helper` symbol unchanged (whole-word boundary check); (i) **prefix-overlap order safety** — `class FOO MyClass {}; class FOO_BAR OtherClass {};` with `[\"FOO\", \"FOO_BAR\"]` AND with `[\"FOO_BAR\", \"FOO\"]` → both orderings produce both `MyClass` and `OtherClass` (locks the worked-example claim from the design); (j) **empty list short-circuit** — assert `Cow::Borrowed` discriminant for empty `macros`; (k) **byte-offset preservation** — parse a class with a macro prefix, assert the resulting symbol's `line` and `column` match the position of `MyClass` in the *original* (pre-substitution) source bytes."
  - id: "1.3"
    title: "Structural verification"
    status: complete
    depends_on: ["1.1", "1.2"]
    verification: "`cargo fmt --all --check` clean. `cargo clippy --workspace --all-targets -- -D warnings` clean (no `#[allow]` to suppress findings on the new `CppConfig`, the empty-string filter, or `strip_macros`). `cargo test --workspace` passes — the new tests pass; the existing 49 C++ corpus tests remain unchanged (they don't exercise macro-prefixed classes, so adding `strip_macros` as a callable-but-uncalled function should not affect them). No snapshot files regenerate in this phase (nothing wired through to the parse pipeline yet)."
---

# Phase 1: Foundation — CppConfig + strip_macros algorithm

## Overview

Lock the algorithm in isolation before wiring it through the pipeline. `CppConfig` lands in `RootConfig` with an empty-string filter; `strip_macros` ships as a free function with the full correctness suite. After this phase, the substitution algorithm is fully specified and tested but does not affect any production parse — Phase 2 connects it.

The algorithm-only-then-wire-it-through pattern is deliberate: it isolates the highest-risk piece (byte-level substitution that must not corrupt any C++ file) into a phase where it can be exhaustively unit-tested without the noise of integration concerns. Phase 2's verification can then assume the substitution itself is correct and focus on plumbing.

## 1.1: Add CppConfig to RootConfig with empty-string filter

### Subtasks
- [x] Define `pub struct CppConfig { #[serde(default)] pub macro_strip: Vec<String> }` in `crates/codegraph-core/src/config.rs`
- [x] Add `#[derive(Debug, Default, Deserialize, Clone)]` to `CppConfig`
- [x] Add `#[serde(default)] pub cpp: CppConfig` field to `RootConfig`
- [x] Implement empty-string filtering — either a custom `Deserialize` impl on `CppConfig` or a normalization in `RootConfig::load` after deserializing. Recommend post-load normalization (simpler, easier to test): `cfg.cpp.macro_strip.retain(|s| { let keep = !s.is_empty(); if !keep { tracing::warn!("dropping empty macro_strip entry"); } keep });`
- [x] Add unit tests: absent section → defaults; `["" , "CORE_API", ""]` → `["CORE_API"]`; `[]` → `[]` no warnings; existing tests still pass

### Notes
The empty-string filter is non-negotiable per design Decision 7 — an empty pattern in the substitution loop would match every byte position with `pat.len() == 0`, advancing 0 bytes per iteration → infinite loop in release builds. The filter at config-load time is the only safe place; the substitution loop is allowed to assume every pattern has length > 0.

## 1.2: Implement strip_macros + algorithm-correctness unit tests

### Subtasks
- [x] Add `pub(crate) fn strip_macros(content: &[u8], macros: &[String]) -> Cow<'_, [u8]>` in `crates/codegraph-lang-cpp/src/` (recommend a new `preprocess.rs` module so the function is discoverable; alternatively add it as an associated function on `CppParser` in `lib.rs`)
- [x] Implement: empty list → `Cow::Borrowed(content)`; otherwise allocate `Vec<u8>` from `content`, then for each macro pattern do a byte-equality scan with whole-word boundary check (`is_ident_byte = b.is_ascii_alphanumeric() || b == b'_'`); on match, overwrite those byte positions with `b' '`
- [x] Implement `is_ident_byte` as a tight inline helper. Document that `$` is excluded (GCC/Clang extension; not used in UE)
- [x] Add unit tests for cases (a)–(k) listed in the verification field. Tests that need actual class extraction (cases a, b, c, d, e) should drive the existing `CppParser::parse_to_filegraph` with the cleaned bytes returned by `strip_macros`, asserting the resulting `FileGraph` contains the expected symbols/edges. Tests for substitution properties only (cases f, g, h, i, j, k) can call `strip_macros` directly and assert on the returned `Cow`
- [x] For case (j) (empty-list short-circuit), assert `matches!(strip_macros(b"...", &[]), Cow::Borrowed(_))`
- [x] For case (k) (byte-offset preservation), construct a fixture where `MyClass` is at a known byte offset / line / column in the original source; after extraction, assert the symbol's `line` and `column` match those positions exactly

### Notes
The substitution does not need to be aware of C++ syntax. It is a pure byte-level operation. The whole-word check is the only "knowledge" of the underlying language and is correct for ASCII-only C++ identifiers (the standard's "core spelling"). Per the design, raw-string-literal delimiters that match a macro name will corrupt the raw string — that case is documented as a limitation, not a test target. Case (g) tests that *content* of a raw string is unaffected, which it is.

## 1.3: Structural verification

### Subtasks
- [x] `cargo fmt --all --check` clean
- [x] `cargo clippy --workspace --all-targets -- -D warnings` clean — no `#[allow]` attrs added
- [x] `cargo test --workspace` passes; existing 49 C++ corpus tests untouched
- [x] Confirm `git diff --stat tests/snapshots/` shows zero churn — nothing in this phase touches the parse pipeline, so no snapshot should regenerate

### Notes
The clippy gate is important for the new `CppConfig` derives and the substitution function — `Cow` return types and byte-level loops sometimes attract `needless_collect` or `needless_pass_by_value` lints; if any fire, fix them rather than allow them.

## Acceptance Criteria

- [x] `CppConfig` exists in `RootConfig` with `#[serde(default)]` and the empty-string filter applied at config-load
- [x] `strip_macros` exists in `codegraph-lang-cpp` with the full correctness suite passing
- [x] All 49 existing C++ corpus tests pass byte-identical
- [x] No snapshot files regenerate (nothing in the parse pipeline calls `strip_macros` yet)
- [x] `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace` all clean
- [x] Phase 2 has a fully-specified substitution function and config struct to consume
