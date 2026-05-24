---
title: "Config field + validation (no behavior change)"
type: phase
plan: UeMacroSupport
phase: 1
status: complete
created: 2026-05-13
updated: 2026-05-14
deliverable: "`[cpp].macro_strip_with_args` field exists in `CppConfig`, is loaded from `.code-graph.toml`, and validates correctly (empty entries dropped with `eprintln!`, within-list dedup silent, cross-list intersection with `macro_strip` rejected with `MacroStripConflict { token }`). The C++ parser does NOT consume the field yet — pure config-layer addition with zero behavioral impact on parsing."
tasks:
  - id: "1.1"
    title: "Add macro_strip_with_args field to CppConfig"
    status: complete
    verification: "`crates/code-graph-core/src/config.rs` `CppConfig` struct gains `pub macro_strip_with_args: Vec<String>` with `#[serde(default)]` so absent section/key yields an empty Vec. The existing `macro_strip` field is unchanged. Round-trips through TOML: `[cpp]\\nmacro_strip_with_args = [\"UCLASS\", \"UFUNCTION\"]\\n` deserializes to `vec![\"UCLASS\".to_string(), \"UFUNCTION\".to_string()]`. Absent `[cpp]` section yields an empty Vec (preserves the existing zero-config default behavior). Unit test asserts both round-trip and default-empty behavior."
  - id: "1.2"
    title: "Add MacroStripConflict variant to ConfigError"
    status: complete
    verification: "`crates/code-graph-core/src/config.rs` `ConfigError` enum gains `MacroStripConflict { token: String }` with `#[error(\"[cpp] macro '{token}' may not appear in both `macro_strip` and `macro_strip_with_args` (ambiguous strip target — see CLAUDE.md and the UeMacroSupport design)\")]`. The variant is constructible in tests; the error message names the offending token verbatim. The error renders cleanly via `Display` (no escape artifacts). `RootConfig::load` returns this variant (not a `Toml` or `Io` parse error) when the conflict is detected."
    depends_on: ["1.1"]
  - id: "1.3"
    title: "Validation in RootConfig::load"
    status: complete
    verification: "`RootConfig::load` in `crates/code-graph-core/src/config.rs` validates `macro_strip_with_args` immediately after deserialization (parallel to the existing `macro_strip` validation at :345–353): (a) drain empty-string entries with a SINGLE `eprintln!(\"code-graph-mcp: dropping empty entry from .code-graph.toml [cpp].macro_strip_with_args\")` per occurrence (matches the existing `macro_strip` pattern); (b) silently deduplicate within-list duplicates; (c) detect cross-list intersection: any token present in BOTH `macro_strip` and `macro_strip_with_args` produces `Err(ConfigError::MacroStripConflict { token })` with the FIRST offending token (don't enumerate; one is enough to block the load); (d) case-sensitive matching throughout — NO lowercasing (C++ macro names are case-sensitive; `UCLASS` ≠ `uclass`)."
    depends_on: ["1.2"]
  - id: "1.4"
    title: "Config-layer unit tests"
    status: complete
    verification: "`crates/code-graph-core/src/config.rs` test module gains tests parallel to the existing `cpp_macro_strip_*` tests: (a) `cpp_macro_strip_with_args_default_empty` — absent section yields empty Vec; (b) `cpp_macro_strip_with_args_round_trips` — explicit values parse correctly; (c) `cpp_macro_strip_with_args_filters_empty_strings` — empties dropped, `eprintln!` confirmed (or just shape asserted, matching existing test); (d) `cpp_macro_strip_with_args_dedups_within_list` — `[\"UCLASS\", \"UCLASS\"]` deserializes to single entry; (e) `cpp_macro_strip_conflict_rejected` — `macro_strip = [\"X\"], macro_strip_with_args = [\"X\"]` returns `Err(ConfigError::MacroStripConflict { token: \"X\".into() })`; (f) `cpp_macro_strip_disjoint_lists_pass` — `macro_strip = [\"ENGINE_API\"], macro_strip_with_args = [\"UCLASS\"]` loads cleanly; (g) `cpp_macro_strip_case_sensitivity_preserved` — `[\"UCLASS\"]` and `[\"uclass\"]` are treated as distinct tokens (no false-positive conflict)."
    depends_on: ["1.3"]
  - id: "1.5"
    title: "Structural verification"
    status: complete
    verification: "`cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all --check` clean; `cargo test -p code-graph-core config` green (the 7 new tests pass); existing `cpp_macro_strip_*` tests stay green (no regression); `make snapshot-clean` passes (Phase 1 touches no handler so no snapshots regenerate)."
    depends_on: ["1.4"]
---

# Phase 1: Config field + validation (no behavior change)

## Overview

Pure config-layer phase. Lands the `macro_strip_with_args` field, the `MacroStripConflict` error variant, and validation in `RootConfig::load`. The C++ parser does NOT consume the field yet — that wiring is Phase 3. By itself this phase is invisible to end users: the field exists, is parsed, and validated, but no extraction behavior changes.

This phase is parallel-safe with Phase 2 (which writes `skip_lexical` in a different crate, `crates/code-graph-lang-cpp/`). Phase 3 fans in to consume both.

## 1.1: Add macro_strip_with_args field to CppConfig

### Subtasks
- [ ] Edit `crates/code-graph-core/src/config.rs:138` (`pub struct CppConfig`); add `pub macro_strip_with_args: Vec<String>,` after the existing `macro_strip` field
- [ ] Confirm `#[serde(default)]` is in place at the struct level (it already is at :136) so absent section/key yields the field's `Default::default()` — `Vec::new()`
- [ ] Update the struct-level doc comment (currently at :123) to mention both fields: "C++-specific knobs. `macro_strip` is whole-word identifier replacement; `macro_strip_with_args` is parameterized-macro replacement (identifier + balanced `(args)`). See `Designs/UeMacroSupport`."
- [ ] No change to `CppConfig::default()` (derives `Default`; new field gets `Vec::new()` automatically)

### Notes
The existing `macro_strip` validation pattern (drop empties, no warn on `[]`) is the model — Tasks 1.3 and 1.4 mirror it field-by-field.

## 1.2: Add MacroStripConflict variant to ConfigError

### Subtasks
- [ ] Locate `ConfigError` in `crates/code-graph-core/src/config.rs`; identify the existing variants (`Io`, `Toml`, `ExtensionMissingDot`, `ExtensionConflict`)
- [ ] Add a new variant: `#[error("[cpp] macro '{token}' may not appear in both `macro_strip` and `macro_strip_with_args` (ambiguous strip target — see CLAUDE.md and the UeMacroSupport design)")] MacroStripConflict { token: String },`
- [ ] Confirm the variant is constructible from `RootConfig::load` (no extra impls needed; `thiserror::Error` derive provides `Display` and `Error` automatically)
- [ ] Verify the `analyze_codebase` handler's error mapping in `crates/code-graph-tools/src/handlers/analyze.rs:77-88` correctly routes this variant — the existing `Err(e @ ConfigError::ExtensionMissingDot { .. }) | Err(e @ ConfigError::ExtensionConflict { .. }) => tool_error(format!("invalid .code-graph.toml: {e}"))` pattern needs `MacroStripConflict { .. }` added to the same arm so the user sees the formatted message
- [ ] Add a comment in the handler arm noting that any new `ConfigError` variant must be mapped here

### Notes
The handler-side error mapping is easy to miss and produces a less helpful error if missed (the variant would surface through some default arm, possibly with a worse message). Verifying the mapping here, in Phase 1, prevents an awkward Phase 3 surprise where the error fires for the first time and the test catches a generic wording.

## 1.3: Validation in RootConfig::load

### Subtasks
- [ ] In `RootConfig::load` (`crates/code-graph-core/src/config.rs:345-353` region — find the existing `macro_strip` validation), add a parallel block for `macro_strip_with_args`:
  - Drain empty-string entries with `eprintln!("code-graph-mcp: dropping empty entry from .code-graph.toml [cpp].macro_strip_with_args")` per drop (same per-drop pattern as the existing block; do NOT batch with a single `eprintln!`)
  - Silently dedup within-list duplicates using `HashSet<&str>` walk-and-retain or equivalent
- [ ] AFTER both fields are individually validated, compute cross-list intersection: `parsed.cpp.macro_strip.iter().find(|t| parsed.cpp.macro_strip_with_args.contains(t))` — on hit, return `Err(ConfigError::MacroStripConflict { token: token.clone() })`
- [ ] Place the conflict check AFTER both empty-drop and dedup steps so paste-mistakes (extra empties, duplicates) don't accidentally trigger conflicts
- [ ] Document the order in a code comment: "Drain → dedup → cross-check. Drain-then-cross-check is the canonical order: empty strings could superficially appear to be in both lists (a deserialization quirk) before they're dropped"
- [ ] Document case sensitivity in a code comment: "Case-sensitive throughout. C++ macro names are case-sensitive; lowercasing would corrupt user config."

### Notes
The existing `macro_strip` validation does NOT lowercase or trim entries beyond the empty-drop. Preserve that asymmetry by not adding trim/lowercase to `macro_strip_with_args` either — case-sensitive comparisons throughout.

## 1.4: Config-layer unit tests

### Subtasks
- [ ] Add the 7 tests listed in this task's `verification` field to `crates/code-graph-core/src/config.rs` test module (search for the existing `cpp_macro_strip_*` tests and place the new ones adjacent)
- [ ] Each test constructs a minimal `.code-graph.toml` fixture in a tempdir, calls `RootConfig::load(&dir)`, and asserts on the result
- [ ] Test (e) (`cpp_macro_strip_conflict_rejected`) MUST assert the error is specifically `ConfigError::MacroStripConflict { token: "X".into() }` (not just `is_err()` — the variant matters for handler error mapping)
- [ ] Test (g) (case sensitivity) MUST assert `[\"UCLASS\"]` and `[\"uclass\"]` produce a successful load (NOT a conflict) — pinpointing the case-sensitive-comparison invariant against future "helpful" lowercasing

### Notes
Test parity with the existing `cpp_macro_strip_*` tests is the simplest path. Aim for the new tests to read as direct analogues of the existing ones a reviewer can scan side-by-side.

## 1.5: Structural verification

### Subtasks
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Run `cargo fmt --all --check`
- [ ] Run `cargo test -p code-graph-core config`
- [ ] Run `cargo test --workspace`
- [ ] Run `make snapshot-clean` — confirm no `*.snap.new` files

### Notes
Phase 1 produces zero behavioral impact on parsing; existing 49 corpus tests + fmt/curl/abseil-cpp baselines must remain byte-identical.

## Acceptance Criteria
- [ ] `CppConfig.macro_strip_with_args` field added with `#[serde(default)]`
- [ ] `ConfigError::MacroStripConflict { token }` variant added with proper `#[error(...)]` text
- [ ] `analyze_codebase` handler error mapping covers the new variant
- [ ] `RootConfig::load` validates: empty drop, dedup, cross-list conflict; case-sensitive throughout
- [ ] 7 new unit tests pass
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
- [ ] `make snapshot-clean` passes
