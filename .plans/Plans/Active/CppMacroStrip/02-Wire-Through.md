---
title: "Wire-through — preprocess hook + call sites + integration"
type: phase
plan: CppMacroStrip
phase: 2
status: planned
created: 2026-05-07
updated: 2026-05-07
deliverable: "`LanguagePlugin::preprocess(&[u8], &RootConfig) -> Cow<[u8]>` is the new trait method with a default impl returning `Cow::Borrowed`. `CppParser` overrides it to call `strip_macros`. Indexer and watch handler both call `preprocess` then `parse_file`. End-to-end integration test confirms config flows from `.code-graph.toml` through `analyze_codebase` into the C++ plugin's substitution. Existing 49 corpus tests + every existing snapshot remain byte-identical."
tasks:
  - id: "2.1"
    title: "Add preprocess hook to LanguagePlugin trait + CppParser override"
    status: planned
    verification: "`crates/codegraph-lang/src/lib.rs::LanguagePlugin` gains `fn preprocess<'a>(&self, content: &'a [u8], _cfg: &RootConfig) -> Cow<'a, [u8]> { Cow::Borrowed(content) }` as a default-impl trait method. The trait method takes `&RootConfig` from `codegraph-core` (which is already a transitive dep). `CppParser` in `crates/codegraph-lang-cpp/src/lib.rs` overrides `preprocess` to return `strip_macros(content, &cfg.cpp.macro_strip)`. **No other plugin (Rust, Go, Python) is touched** — they inherit the default impl. **No test stub is touched** — `FakePlugin` (`crates/codegraph-lang/src/lib.rs:441-463`) and `StubPlugin` (`crates/codegraph-tools/src/indexer.rs:319-353`) inherit the default impl. `cargo build --workspace` succeeds. `cargo test --workspace` passes (the override has no effect yet because nobody calls `preprocess`)."
  - id: "2.2"
    title: "Wire preprocess into the indexer call site"
    status: planned
    depends_on: ["2.1"]
    verification: "`crates/codegraph-tools/src/indexer.rs` (around line 158-161) updates the `fs::read` → `parse_file` sequence to insert `let cleaned = plugin.preprocess(&content, cfg);` and pass `&cleaned` to `parse_file`. The `cfg: &RootConfig` was already in scope at this call site (already passed into `index_directory` per the existing plumbing). For non-C++ files, `preprocess` returns `Cow::Borrowed(content)` so there is zero allocation cost on the hot path. Verified by: (a) `cargo build --workspace` succeeds; (b) `cargo test --workspace` passes; (c) the `fmtlib/fmt` parse-test baseline (32 symbols, 244 edges per Phase 1 of RustRewrite) is unchanged when the manual smoke test is run — fmt has no UE-style macros so no behavior change is expected."
  - id: "2.3"
    title: "Wire preprocess into the watch-handler call site"
    status: planned
    depends_on: ["2.1"]
    verification: "`crates/codegraph-tools/src/handlers/watch.rs::try_reindex_file` (around line 310, the `plugin.parse_file(&path_owned, &content)` call) is updated to insert `let cleaned = plugin.preprocess(&content, &cached_cfg);` first, where `cached_cfg` comes from the existing `inner.config.read().clone()` access (the file already reads `inner.config` at ~line 260 — extend that to keep the value rather than discarding it). Watch-mode reindex picks up the same `macro_strip` list that the most-recent `analyze_codebase` cached. Verified by: (a) `cargo test --workspace` passes; (b) the watch-mode reindex tests in `crates/codegraph-tools/tests/watch_*.rs` pass unchanged — they use Rust/Go/Python fixtures and inherit the default `preprocess` impl, so their behavior is unaffected."
  - id: "2.4"
    title: "End-to-end integration test (config flows through pipeline)"
    status: planned
    depends_on: ["2.2", "2.3"]
    verification: "New test in `crates/codegraph-tools/tests/` (recommend `tests/cpp_macro_strip.rs` as a dedicated file). **Test 1 (positive case):** sets up a `TempDir` containing: (a) a `.code-graph.toml` with `[cpp]\\nmacro_strip = [\\\"CORE_API\\\", \\\"ENGINE_API\\\"]`; (b) a `MyActor.h` with `class CORE_API AActor : public UObject {};` and `class ENGINE_API APawn : public AActor {};`. Invokes `index_directory` (or `analyze_codebase`) and asserts the resulting `Graph` contains: `AActor` symbol (Class kind), `APawn` symbol (Class kind), `Inherits` edge from `AActor` to `UObject`, `Inherits` edge from `APawn` to `AActor`. **Test 2 (control case): uses the IDENTICAL `MyActor.h` content as Test 1, with only the `.code-graph.toml` differing** (no `[cpp]` section). Asserts the resulting `Graph` does NOT contain `AActor` or `APawn`. The fixture-identical-config-different structure guarantees that any difference between Test 1 and Test 2 is attributable solely to `macro_strip` being set — not to fixture syntax errors or unrelated pipeline behavior. **Test 3 (watch-mode REQUIRED, not optional):** in `tests/watch_cpp_macro_strip.rs`, `analyze_codebase` against a TempDir with `[cpp].macro_strip = [\\\"CORE_API\\\"]`, start a watcher, write `MyActor.h` with `class CORE_API AActor : public UObject {};` to the watched dir, wait for the debounce + reindex, assert `AActor` symbol appears. **This test is the only thing that catches a broken `preprocess` wiring in `try_reindex_file` (task 2.3) — without it, an implementer can pass `RootConfig::default()` to `preprocess` in the watch handler and 2.3's verification would still appear to pass because the existing watch tests use Rust/Go/Python fixtures that inherit the default no-op preprocess.**"
  - id: "2.5"
    title: "Anti-regression sweep + structural verification"
    status: planned
    depends_on: ["2.4"]
    verification: "`cargo fmt --all --check` clean. `cargo clippy --workspace --all-targets -- -D warnings` clean. `cargo test --workspace` passes — full suite. **Existing 49 C++ corpus tests pass byte-identical.** **`cargo insta pending-snapshots` reports zero AND `git diff --stat tests/snapshots/` shows zero churn** — no existing snapshot regenerates because all current C++ fixtures use empty `macro_strip` (no `[cpp]` section in any current `.code-graph.toml`). If the engine.cpp hierarchy snapshot or any other existing snapshot regenerates, that's a sign the default `preprocess` impl isn't actually no-op for non-C++ files, which is a bug — investigate before approving any snapshot change. **Manual smoke**: run `codegraph-parse-test` against `testdata/cpp/` (no `.code-graph.toml` there, so default empty `macro_strip` applies); confirm output matches the historical **18 symbols / 21 edges** baseline (corrected from the original Phase 1 plan's typo of 17, per the RustRewrite Phase 1 retro)."
---

# Phase 2: Wire-through — preprocess hook + call sites + integration

## Overview

Connect Phase 1's substitution algorithm to the live parse pipeline. The `preprocess` hook on `LanguagePlugin` is the load-bearing abstraction (per design Decision 4): a default impl that costs nothing for plugins that don't need it, an override on `CppParser` that calls `strip_macros`, and exactly two call-site updates (indexer + watch handler). The end-to-end integration test in 2.4 is the safety rail against the most likely failure mode — a future refactor that accidentally passes `RootConfig::default()` everywhere — by asserting the macro-prefixed class symbols actually appear when `macro_strip` is configured AND assert they don't appear (control test) when it isn't.

**Task ordering within Phase 2:** Tasks 2.2 (indexer call site) and 2.3 (watch handler call site) are independent edits to two different files; either can land first or both can land together. Both depend only on 2.1 (the trait extension). The frontmatter encodes 2.4 as depending on both because the integration test requires both wirings to be in place — it does not imply 2.2 must precede 2.3 or vice versa.

The anti-regression sweep in 2.5 is non-negotiable: if any existing snapshot or corpus test changes, something is wrong with the default-impl no-op claim.

## 2.1: Add preprocess hook to LanguagePlugin trait + CppParser override

### Subtasks
- [ ] In `crates/codegraph-lang/src/lib.rs`, add to the `LanguagePlugin` trait:
  ```rust
  /// Pre-parse hook for byte-level transformations (macro stripping,
  /// preprocessor shims, etc.). Default impl borrows the input
  /// unchanged — zero-cost for plugins that don't need it.
  fn preprocess<'a>(&self, content: &'a [u8], _cfg: &RootConfig) -> Cow<'a, [u8]> {
      Cow::Borrowed(content)
  }
  ```
- [ ] Add `use std::borrow::Cow;` and `use codegraph_core::RootConfig;` to the trait module if not already imported
- [ ] In `crates/codegraph-lang-cpp/src/lib.rs`, add the override on `CppParser`'s `LanguagePlugin` impl:
  ```rust
  fn preprocess<'a>(&self, content: &'a [u8], cfg: &RootConfig) -> Cow<'a, [u8]> {
      strip_macros(content, &cfg.cpp.macro_strip)
  }
  ```
- [ ] Confirm `cargo build -p codegraph-lang -p codegraph-lang-cpp` succeeds
- [ ] Confirm `cargo build --workspace` succeeds — no out-of-tree implementor or test stub breaks because the trait method has a default impl

### Notes
This task is the entire trait extension. It's strictly additive — no signature on any existing method changes. Test stubs and the three other production plugins inherit the default no-op impl. The reviewer specifically called out (Major finding) that the original design's `parse_file` signature change would have rippled to 4 plugins + 2 test stubs + 2 call sites; this approach reduces it to 1 trait change + 1 override + 2 call sites.

## 2.2: Wire preprocess into the indexer call site

### Subtasks
- [ ] Read `crates/codegraph-tools/src/indexer.rs` lines 158-161 to see the current `fs::read` → `parse_file` sequence
- [ ] Insert `let cleaned = plugin.preprocess(&content, cfg);` between the `fs::read` result and the `parse_file` call
- [ ] Update the `parse_file` call to pass `&cleaned` (which is `&[u8]` via `Cow::deref`)
- [ ] Confirm `cfg: &RootConfig` is already in scope at this call site (per the design's research note, the indexer already threads `cfg` through `index_directory`)
- [ ] `cargo build --workspace` succeeds; `cargo test --workspace` passes

### Notes
For non-C++ files, `plugin.preprocess` returns `Cow::Borrowed(content)` (the default impl); the resulting `&[u8]` is borrowed from the original content with no allocation. Only C++ files allocate, and only when `macro_strip` is non-empty.

## 2.3: Wire preprocess into the watch-handler call site

### Subtasks
- [ ] Read `crates/codegraph-tools/src/handlers/watch.rs::try_reindex_file` around lines 250-310
- [ ] The function already accesses `inner.config.read()` (the cached `RootConfig` from the most-recent `analyze_codebase`) — the existing access at ~line 260 reads it and discards it. Refactor to bind it: `let cached_cfg = inner.config.read().clone();`
- [ ] At the `plugin.parse_file(&path_owned, &content)` call (~line 310), insert `let cleaned = plugin.preprocess(&content, &cached_cfg);` and pass `&cleaned` to `parse_file`
- [ ] Confirm the watch tests (`crates/codegraph-tools/tests/watch_*.rs`) still pass — they use Rust/Go/Python fixtures and the default `preprocess` impl, so their behavior is unaffected

### Notes
The cached `RootConfig` strategy means watch-mode reindex picks up the same `macro_strip` list that the most-recent `analyze_codebase` saw. If a user edits `.code-graph.toml` after starting watch mode, the change does NOT take effect on subsequent file-change events until the next `analyze_codebase` call — this is consistent with how `max_threads` and other config knobs behave. The Phase 3 documentation will call this out.

## 2.4: End-to-end integration test (config flows through pipeline)

### Subtasks
- [ ] Create `crates/codegraph-tools/tests/cpp_macro_strip.rs`
- [ ] Test 1 — `cpp_macro_strip_extracts_class_with_api_macro`:
  - Build a `TempDir` with `.code-graph.toml` containing `[cpp]\nmacro_strip = ["CORE_API", "ENGINE_API"]`
  - Write `MyActor.h` with two macro-prefixed classes (one chained inheritance)
  - Call `index_directory` (or whatever the in-process indexer entry point is)
  - Assert the resulting `Graph` contains `AActor` (Class), `APawn` (Class), `AActor → UObject` Inherits, `APawn → AActor` Inherits
- [ ] Test 2 — `cpp_macro_strip_control_empty_list_does_not_extract`:
  - Same fixture as Test 1 but `.code-graph.toml` has no `[cpp]` section (or `macro_strip = []`)
  - Assert the resulting `Graph` does NOT contain `AActor` or `APawn` (the bug is preserved when opt-in is empty)
  - This control test is what makes Test 1 meaningful — proves the difference is attributable to `macro_strip`, not to other changes
- [ ] **Required (not optional)** — `tests/watch_cpp_macro_strip.rs`:
  - `analyze_codebase` against an empty TempDir with `[cpp].macro_strip = ["CORE_API"]`
  - Start a watcher
  - Write `MyActor.h` with `class CORE_API AActor : public UObject {};` to the watched dir
  - Wait for the watch debouncer + reindex
  - Assert `AActor` symbol appears in the graph
  - This is the only test that catches a broken wiring in `try_reindex_file` (task 2.3); without it, the watch path is effectively unverified

### Notes
Test 2 (the control) is reviewer-flagged: without it, a future refactor that accidentally short-circuits `preprocess` to a no-op (or passes `RootConfig::default()` to `preprocess`) would still pass Test 1 if the bug is "doesn't matter, the symbols are in the graph for some other reason." Asserting both the positive AND negative case — using the IDENTICAL fixture, differing only in config — is the discriminator. Test 3 (watch-mode) extends the same discipline to the watch handler call site.

## 2.5: Anti-regression sweep + structural verification

### Subtasks
- [ ] `cargo fmt --all --check` clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` passes
- [ ] `git diff --stat tests/snapshots/` shows ZERO churn — no existing snapshot regenerates. If any does, investigate before proceeding (likely a sign the default `preprocess` is altering bytes when it shouldn't)
- [ ] Optional manual smoke: `cargo run -p codegraph-parse-test -- testdata/cpp/` produces **18 symbols / 21 edges** (the corrected Phase 1 RustRewrite baseline; the original plan's "17" was a typo, fixed in the Phase 1 retro)
- [ ] Optional manual smoke: clone fmtlib/fmt, run parse-test, confirm 32 symbols / 244 edges baseline (the historical Phase 1 dogfood baseline)

### Notes
The zero-snapshot-diff assertion is the strongest claim of "this change is invisible to existing users." If it doesn't hold, we have a real bug somewhere — the default `Cow::Borrowed(content)` should produce byte-identical bytes to the original content, and any non-C++ plugin should hit that path.

## Acceptance Criteria

- [ ] `LanguagePlugin::preprocess` exists with a default impl returning `Cow::Borrowed(content)`
- [ ] `CppParser` overrides `preprocess` to call `strip_macros(content, &cfg.cpp.macro_strip)`
- [ ] Indexer and watch handler both call `preprocess` before `parse_file`
- [ ] End-to-end integration test (positive case) confirms macro-prefixed classes extract when `macro_strip` is configured
- [ ] Control test (using IDENTICAL fixture, differing only in config) confirms zero classes when `macro_strip` is empty — proves `preprocess` is actually wired in the indexer
- [ ] Watch-mode integration test confirms macro-prefixed classes extract on file-change events — proves `preprocess` is wired in `try_reindex_file`
- [ ] All 49 existing C++ corpus tests pass byte-identical
- [ ] All existing snapshots show zero diff
- [ ] `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace` clean
- [ ] No test stub or out-of-tree implementor needed updates
