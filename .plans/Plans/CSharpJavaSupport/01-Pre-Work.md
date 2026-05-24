---
title: "Pre-Work — Workspace Plumbing"
type: phase
plan: CSharpJavaSupport
phase: 1
status: complete
created: 2026-05-08
updated: 2026-05-08
deliverable: "Workspace pinned to tree-sitter-c-sharp and tree-sitter-java; Language enum has CSharp + Java variants; ExtensionsConfig has csharp + java fields and the corresponding documentation/example references are in sync. After this phase, code in Phases 2 and 3 compiles when it references Language::CSharp / Language::Java and when users add [extensions].csharp / [extensions].java to .code-graph.toml."
tasks:
  - id: "1.1"
    title: "Pin tree-sitter grammars + add Language enum variants"
    status: complete
    verification: "Cargo.toml has `tree-sitter-c-sharp = \"=X.Y.Z\"` and `tree-sitter-java = \"=X.Y.Z\"` strict-pinned in [workspace.dependencies]; both versions confirmed compatible with the workspace's tree-sitter core (currently 0.26) by `cargo tree -p code-graph-core` or by attempting `cargo build -p code-graph-core` after a probe lib references them. `Language::CSharp` and `Language::Java` variants exist in `crates/code-graph-core/src/lib.rs` (the enum is already `#[non_exhaustive]`, so no breaking-change handling needed). `cargo build --workspace` succeeds. `cargo test -p code-graph-core` passes including any existing serde round-trip tests for Language (verify the `#[serde(rename_all = \"lowercase\")]` produces `\"csharp\"` and `\"java\"` — add a serde round-trip assertion for each new variant if one doesn't already exist for the language enum)."
  - id: "1.2"
    title: "Extend ExtensionsConfig with csharp + java fields"
    status: complete
    depends_on: ["1.1"]
    verification: "`ExtensionsConfig` in `crates/code-graph-core/src/config.rs` has `csharp: Vec<String>` and `java: Vec<String>` fields with `#[serde(default)]`. `lookup_additional` (currently 4 language arms) returns `Some(Language::CSharp)` for matches in `self.csharp` and `Some(Language::Java)` for matches in `self.java`. `lists_mut` returns a `[(&'static str, &mut Vec<String>); 7]` (was `; 5`) including `(\"csharp\", &mut self.csharp)` and `(\"java\", &mut self.java)`. `additive_lists` returns `[(&'static str, &Vec<String>); 6]` (was `; 4`) with the corresponding entries. `RootConfig::load` normalization (the loop at config.rs:281 that drains empties and validates leading-dot) compiles unchanged because it iterates `lists_mut()` uniformly. `.code-graph.toml.example` documents the new lists alongside cpp/rust/go/python with the same example-comment style. `CLAUDE.md`'s `[extensions]` table comment grows from 4 → 6 language defaults (`csharp = [.cs]`, `java = [.java]` added). New tests in `crates/code-graph-core/src/config.rs`: a `[extensions].csharp = [\".aspx\"]` round-trip lookup test, and a cross-additive collision test asserting `[extensions].csharp = [\".x\"]` and `[extensions].java = [\".x\"]` returns `Err(ExtensionConflict)`. `cargo build --workspace` and `cargo test -p code-graph-core` pass."
---

# Phase 1: Pre-Work — Workspace Plumbing

## Overview

Two prep tasks that unblock Phases 2 and 3. Neither task involves tree-sitter queries, parser bodies, or test fixtures — both are purely shared-crate plumbing. Phase 1 is **strict prerequisite** to both parallel phases: the plugin crates' `id()` impls reference `Language::CSharp` and `Language::Java`, and the per-root `[extensions].csharp` / `[extensions].java` config entries must deserialize cleanly the moment a user adds them to a `.code-graph.toml`.

Tasks within this phase are sequential (1.1 → 1.2) because 1.2 references the new enum variants from 1.1.

## 1.1: Pin tree-sitter grammars + add Language enum variants

### Subtasks

- [x] Probe latest stable releases of `tree-sitter-c-sharp` and `tree-sitter-java` on crates.io
- [x] Confirm both grammars build against `tree-sitter` core 0.26 — try a quick `cargo new` probe that adds both grammars and runs `cargo build`. If neither has a 0.26-compatible release, **stop and flag**: this plan is blocked until upstream catches up.
- [x] Add to `Cargo.toml` `[workspace.dependencies]` (strict-pinned with `=`, matching the workspace convention) — pinned at `=0.23.5` for both
- [x] Add `CSharp` and `Java` variants to `Language` enum in `crates/code-graph-core/src/lib.rs`. The enum is already `#[non_exhaustive]` and already has `#[serde(rename_all = "lowercase")]` — no derive or attribute changes needed. The new variants serialize as `"csharp"` and `"java"`.
- [x] If a serde round-trip test exists for `Language`, extend it to cover the two new variants. Two existing tests covered Language; both extended (`language_serializes_lowercase` and `symbol_round_trip_every_kind_and_language`).
- [x] Run `cargo build --workspace`, `cargo test -p code-graph-core`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check` — first three pass cleanly; fmt-check has pre-existing drift in 7 unrelated files (NOT introduced by this commit; flagged for separate cleanup).

### Notes

The grammar version pins are TBD until probed against crates.io and against `tree-sitter` core 0.26. Do not commit `<exact>` placeholders — fail loud if either grammar lacks a 0.26-compatible release rather than papering over with an older grammar version.

## 1.2: Extend ExtensionsConfig with csharp + java fields

### Subtasks

- [x] Add fields to `ExtensionsConfig` in `crates/code-graph-core/src/config.rs`
- [x] Widen `lookup_additional` (4 → 6 arms)
- [x] Update `lists_mut`'s array size (5 → 7)
- [x] Update `additive_lists`'s array size (4 → 6)
- [x] Update `.code-graph.toml.example` (`csharp = []`, `java = []` + built-in defaults comment)
- [x] Update `CLAUDE.md`'s `[extensions]` block to include csharp/java built-in defaults
- [x] Add tests: csharp round-trip, java round-trip, cross-additive csharp+java conflict, disabled-precedence for both csharp and java (the symmetric Java test was added in the follow-up commit `c0c6517` after a quality-scan finding)
- [x] Run gates: build/test/clippy pass cleanly; fmt-check on touched crates clean (workspace-wide fmt has pre-existing drift in 7-8 unrelated files, NOT introduced by this commit)

### Notes

The fixed-size array literals in `lists_mut` and `additive_lists` are compile-time-checked — getting the size wrong is a build error, not a runtime surprise. That's the safety net for the `5 → 7` and `4 → 6` widening.

## Acceptance Criteria

- [x] `Cargo.toml` has strict-pinned `tree-sitter-c-sharp = "=0.23.5"` and `tree-sitter-java = "=0.23.5"` dependencies, both confirmed compatible with `tree-sitter` core 0.26
- [x] `Language::CSharp` and `Language::Java` exist in `crates/code-graph-core/src/lib.rs` and round-trip through serde (verified by `language_serializes_lowercase` and `symbol_round_trip_every_kind_and_language`)
- [x] `parse_language` and `SearchSymbolsInput::language` schemars description updated to handle the new languages (added in follow-up commit `8a2cde2` after a quality-scan finding)
- [x] `ExtensionsConfig` accepts `[extensions].csharp` and `[extensions].java`; `lookup_additional`, `lists_mut`, `additive_lists` all updated
- [x] `.code-graph.toml.example` and `CLAUDE.md` `[extensions]` documentation list 6 language defaults
- [x] Cross-additive collision and disabled-precedence tests pass (both csharp and java symmetric variants)
- [x] `cargo build --workspace`, `cargo test -p code-graph-core`, `cargo test -p code-graph-lang`, `cargo clippy --workspace --all-targets -- -D warnings` all pass; per-crate `cargo fmt --check` clean on touched crates (workspace-wide fmt drift on 7-8 pre-existing unrelated files documented for separate cleanup)
