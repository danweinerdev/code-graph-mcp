---
title: "UE integration tests + docs + .code-graph.toml.example preset"
type: phase
plan: UeMacroSupport
phase: 4
status: complete
created: 2026-05-13
updated: 2026-05-14
deliverable: "A synthetic UE-style fixture (Actor.h / Object.h / ActorComponent.h with realistic `UCLASS(...)` / `UFUNCTION(...)` / `UPROPERTY(...)` / `GENERATED_BODY()` usage) drives end-to-end integration tests: with the recommended preset, `AActor` / `UObject` / `UActorComponent` extract correctly and `get_class_hierarchy` walks the diamond. An anti-regression test pins today's broken behavior (zero symbols without the preset). `.code-graph.toml.example` ships the recommended UE preset with caveats. CLAUDE.md captures the new capability, the cache-invalidation interaction, and the two-pass raw-string corruption nuance."
tasks:
  - id: "4.1"
    title: "Synthetic UE-style fixture under tests/fixtures/"
    status: complete
    verification: "New fixture directory `crates/code-graph-tools/tests/fixtures/ue_minimal/` (path may differ — match the existing fixture convention) contains: (a) `.code-graph.toml` with the UE preset enabled (`[cpp].macro_strip_with_args = [...]` with at least `UCLASS`, `USTRUCT`, `UFUNCTION`, `UPROPERTY`, `GENERATED_BODY`); (b) `Object.h` with `class COREUOBJECT_API UObject { GENERATED_UCLASS_BODY() public: UFUNCTION(BlueprintCallable) virtual void Tick(float DeltaSeconds); };` (or equivalent — realistic UE-style content); (c) `Actor.h` with `UCLASS(BlueprintType, meta=(BlueprintSpawnableComponent)) class ENGINE_API AActor : public UObject { GENERATED_BODY() public: UFUNCTION(BlueprintCallable, Category=\"Tick\") virtual void Tick(float DeltaSeconds) override; UPROPERTY(EditAnywhere) UAnimMontage* MyMontage; };` and similar realistic usage; (d) `ActorComponent.h` with `class ENGINE_API UActorComponent : public UObject {};` to give the diamond a third leaf. The fixture is gitignored-aware — confirm `.code-graph.toml` requires `git add -f` (per CLAUDE.md test conventions on gitignored test fixtures); the test setup uses `git check-ignore` to verify; the fixture files are added with `-f` and committed. `make snapshot-audit ARGS=\"ue_minimal\"` confirms the snapshot tier is discoverable."
  - id: "4.2"
    title: "Integration test: extraction works with preset enabled"
    status: complete
    verification: "New test in `crates/code-graph-tools/tests/ue_macro_support.rs` (or matching existing test convention) — `ue_fixture_extracts_uclass_with_preset`. Indexes the Phase 4.1 fixture. Asserts: (a) `search_symbols(\"^AActor$\")` returns total >= 1 with a `Class` kind result; (b) `search_symbols(\"^UObject$\")` returns total >= 1; (c) `search_symbols(\"^UActorComponent$\")` returns total >= 1; (d) `get_class_hierarchy(\"UObject\")` returns `AActor` AND `UActorComponent` in the `derived` array (the diamond); (e) `search_symbols(\"^AActor::Tick$\")` finds the method with parent `AActor`, line numbers correct (preserved byte offsets per the design invariant). Note: `Graph::search` builds the regex target as `\"{parent}::{name}\"` for parented methods, so a bare `\"^Tick$\"` regex returns zero results; the regex MUST be anchored against the full `parent::name` form; (f) `get_file_symbols(\"<fixture>/Actor.h\")` includes `AActor` and `Tick`. Each assertion has a clear failure message naming the missing symbol."
    depends_on: ["4.1"]
  - id: "4.3"
    title: "Anti-regression test: zero symbols WITHOUT the preset"
    status: complete
    verification: "New test `ue_fixture_no_config_extracts_zero_aactor_symbols` indexes the same fixture but with `[cpp].macro_strip_with_args = []` (empty — opt-in default). Asserts: (a) `search_symbols(\"^AActor$\")` returns total == 0 (today's broken behavior pinned); (b) `search_symbols(\"^UObject$\")` returns total == 0; (c) `get_class_hierarchy(\"UObject\")` returns the 'class not found' fuzzy-match suggestion path (`Err` or the documented error response). The test exists to make removal of the feature a visible regression: if someone proposes \"a different parser approach makes UCLASS work without needing the preset,\" deleting this test is a deliberate signal that the unfixed-state baseline no longer holds. The test docstring explicitly says: 'When `UeMacroSupport` ships, this test PASSES because the fixture-without-preset reproduces the originally-reported UE4 bug. Deletion of this test is the marker that we have a better fix.'"
    depends_on: ["4.1"]
  - id: "4.4"
    title: "Recommended UE preset in .code-graph.toml.example"
    status: complete
    verification: "`.code-graph.toml.example` at the repo root gains a `[cpp].macro_strip_with_args = [...]` block — commented out by default — with the preset enumerated. The preset includes: reflection/metadata (`UCLASS`, `USTRUCT`, `UENUM`, `UFUNCTION`, `UPROPERTY`, `UINTERFACE`, `UDELEGATE`, `UPARAM`, `UMETA`), generated-body markers (`GENERATED_BODY`, `GENERATED_UCLASS_BODY`, `GENERATED_USTRUCT_BODY`, `GENERATED_UINTERFACE_BODY`, `GENERATED_IINTERFACE_BODY`), delegate macro families (`DECLARE_DYNAMIC_MULTICAST_DELEGATE`, `DECLARE_DYNAMIC_MULTICAST_DELEGATE_OneParam` through `_ThreeParams`, `DECLARE_DELEGATE`, `DECLARE_DELEGATE_OneParam`, `DECLARE_DELEGATE_TwoParams`, `DECLARE_DELEGATE_RetVal`, `DECLARE_MULTICAST_DELEGATE`, `DECLARE_EVENT`). Comments inline name additional macros users may want to add manually (`DEPRECATED` / `UE_DEPRECATED` for UE4/UE5) and call out the pitfall — conditional-compilation macros like `WITH_EDITOR` / `WITH_EDITORONLY_DATA` must NOT be added (they appear in `#if` contexts and stripping them breaks the parse). The example file remains TOML-valid (parses cleanly via `toml::from_str` in a smoke test, even with all lines commented out)."
    depends_on: ["4.3"]
  - id: "4.5"
    title: "CLAUDE.md updates"
    status: complete
    verification: "FOUR CLAUDE.md edits land in this task: (a) C++ Supported list (line ~155): add a bullet adjacent to the existing 'Macro-prefixed classes (`class CORE_API MyClass : public Base {}`) iff listed in `[cpp].macro_strip`' line — 'Parameterized API macros (`UCLASS(...)`, `UFUNCTION(...)`, `UPROPERTY(...)`, `GENERATED_BODY()`, etc.) iff listed in `[cpp].macro_strip_with_args`. Default (no `[cpp].macro_strip_with_args`) leaves these broken — zero behavior change for non-UE users.' (b) Cache invalidation section line 137 — the existing sentence 'Changes to `[cpp].macro_strip` or `[extensions]` do NOT retroactively re-parse files with unchanged mtime' is edited to 'Changes to `[cpp].macro_strip`, `[cpp].macro_strip_with_args`, or `[extensions]` do NOT retroactively re-parse files with unchanged mtime'. (c) Cache invalidation section line 138 — the existing sentence 'To apply new `macro_strip` or to evict entries moved to `[extensions].disabled`: re-run `analyze_codebase` with `force=true`' is edited to 'To apply new `macro_strip`, new `macro_strip_with_args`, or to evict entries moved to `[extensions].disabled`: re-run `analyze_codebase` with `force=true`'. Both line 137 and 138 must be updated — a user reading line 138 alone must learn the new field also requires `force=true`. (d) C++ Limitation 7 line 164 (raw-string-delimiter collision): today it names only `macro_strip` as the trigger; edit to mention both `macro_strip` and `macro_strip_with_args` as triggers AND add a note that the two-pass interaction makes the issue worse — pass-1 may corrupt the raw-string tag before pass-2's `skip_lexical` runs, so pass-2 may incorrectly scan into what was previously a raw-string body. Workaround unchanged: rename the offending raw-string tag, or remove the colliding macro from both lists. `grep -c 'macro_strip_with_args' CLAUDE.md` returns >= 4 (one mention per edit). `grep -nE 'macro_strip[^_]' CLAUDE.md` — review each remaining single-field mention; in cache-invalidation and limitations contexts both fields must be named."
    depends_on: ["4.4"]
  - id: "4.6"
    title: "Structural verification"
    status: complete
    verification: "`cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all --check` clean; `cargo test --workspace` green (the Phase 4.2 and 4.3 integration tests pass); existing 49 corpus tests + fmt/curl/abseil-cpp baselines stay within ±10%; `make snapshot-clean` passes; any new fixture-related snapshots regenerated via `cargo insta review` deliberately, not blanket-accepted. CLAUDE.md grep checks: `grep -c 'macro_strip_with_args' CLAUDE.md >= 3` AND no orphan reference to the old single-field name in the cache-invalidation sentence."
    depends_on: ["4.5"]
---

# Phase 4: UE integration tests + docs + .code-graph.toml.example preset

## Overview

End-to-end fixture-driven verification + documentation phase. The fixture (Phase 4.1) is engineered to mirror real UE4/UE5 header conventions; the integration test (4.2) proves the feature works when the user opts in; the anti-regression test (4.3) pins today's broken state so the diff is undeniable. Docs (4.4, 4.5) make the feature discoverable: users grep CLAUDE.md, find the bullet, copy the preset from `.code-graph.toml.example`, and the next `analyze_codebase` extracts UE symbols.

This phase is the user-visible payload of the plan. Phases 1–3 build the machinery; Phase 4 is the proof-and-onboarding.

## 4.1: Synthetic UE-style fixture under tests/fixtures/

### Subtasks
- [ ] Decide fixture path. Existing convention: look for `crates/code-graph-tools/tests/fixtures/` or similar; if no `fixtures/` dir exists, place under a `testdata/ue/` mirror of the existing `testdata/rust/`, `testdata/go/` baselines
- [ ] Create `Object.h` modeling UE 4.27's `UObject` core: `class COREUOBJECT_API UObject { GENERATED_UCLASS_BODY() public: UFUNCTION(BlueprintCallable) virtual void Tick(float DeltaSeconds); };` — uses `GENERATED_UCLASS_BODY`, `UFUNCTION`, and the `COREUOBJECT_API` bare-token export macro
- [ ] Create `Actor.h` modeling `AActor`: `UCLASS(BlueprintType, meta=(BlueprintSpawnableComponent)) class ENGINE_API AActor : public UObject { GENERATED_BODY() public: UFUNCTION(BlueprintCallable, Category=\"Tick\") virtual void Tick(float DeltaSeconds) override; UPROPERTY(EditAnywhere) UAnimMontage* MyMontage; };`
- [ ] Create `ActorComponent.h` modeling `UActorComponent`: `UCLASS() class ENGINE_API UActorComponent : public UObject { GENERATED_BODY() };` — minimal, just enough to be the second leaf of the diamond
- [ ] Create `.code-graph.toml` with the recommended preset enabled AND `[cpp].macro_strip = [\"ENGINE_API\", \"COREUOBJECT_API\"]` (whole-word for the bare export macros, parameterized for the reflection macros)
- [ ] Verify gitignore handling: `git check-ignore <path>/.code-graph.toml` — if ignored, `git add -f` the file (per CLAUDE.md test convention: "Gitignored test fixtures need `git add -f`"). Confirm CI checkout includes the file
- [ ] Smoke: `cargo run -p code-graph-parse-test -- <fixture_dir>` succeeds and outputs expected symbols (manual sanity check before writing the test)

### Notes
The fixture is intentionally small (3 header files + 1 config) so the test runs in <100ms. It is NOT a Real UE source tree — that's Phase 5's submodule baseline. This fixture lives in-tree and exists to be the deterministic regression target.

## 4.2: Integration test: extraction works with preset enabled

### Subtasks
- [ ] Create `crates/code-graph-tools/tests/ue_macro_support.rs` (or extend an existing UE-targeted test file if one exists)
- [ ] Mirror the existing integration-test idiom (look at `tests/watch_cpp_macro_strip.rs` for the closest existing pattern). Use the test-server harness to drive `analyze_codebase` against the fixture from 4.1
- [ ] After indexing, call each of the 6 assertion-bearing operations from the verification field, asserting per the spec
- [ ] For `get_class_hierarchy(\"UObject\")`: convert the result to a set of names in `derived` and assert both `\"AActor\"` and `\"UActorComponent\"` are present. Don't assert order — diamond walk order is implementation-defined
- [ ] For `Tick`: pin both line and column to the source position (the design's byte-offset preservation invariant)
- [ ] Each `assert!` carries a custom message naming the offending symbol so test failures localize fast

### Notes
The line/column preservation assertion is the closest thing to a unit test for the "space substitution preserves offsets" invariant from `CppMacroStrip`. If a future refactor changes `strip_macros` or `strip_macros_with_args` to non-byte-preserving substitution, this assertion fails first.

## 4.3: Anti-regression test: zero symbols WITHOUT the preset

### Subtasks
- [ ] Add `ue_fixture_no_config_extracts_zero_aactor_symbols` to the same test file as 4.2
- [ ] Set up: copy the fixture to a tempdir, but rewrite the `.code-graph.toml` to set `macro_strip_with_args = []` (explicit empty — the opt-in default)
- [ ] Drive analyze + the 3 assertions from the verification field
- [ ] Add the test docstring explaining the test exists to PIN today's broken state, and that deletion of this test is the marker for "we shipped a better fix that makes the preset unnecessary"

### Notes
The anti-regression test is unusual in that its passing is documentation that the world is broken. The test's *failure* (after the design's feature lands) would mean we've discovered a way to fix UE extraction without the preset — at which point the failure is a celebration and the test can be deleted.

## 4.4: Recommended UE preset in .code-graph.toml.example

### Subtasks
- [ ] Open `.code-graph.toml.example` at the repo root
- [ ] The existing `[cpp]` block is FULLY COMMENTED OUT (lines :60-65 — `# [cpp]` / `# macro_strip = [ ... ]`). The new `macro_strip_with_args` example stays within the same comment region — add it immediately after the closing `# ]` of the `# macro_strip = [...]` block as a sibling commented-out key. Do NOT add a second `# [cpp]` header (one is enough; the existing one covers both fields)
- [ ] The example block stays COMMENTED OUT by default (preceded by `#` per line) so a user copying the file as-is gets zero behavior change; uncommenting enables the preset
- [ ] List the macros per the verification field, grouped by purpose (reflection / generated-body / delegate)
- [ ] Add inline comments naming: (a) additional macros to add manually for specific UE versions (`DEPRECATED`, `UE_DEPRECATED`); (b) macros NOT to add (`WITH_EDITOR`, `WITH_EDITORONLY_DATA` — conditional-compilation pitfall)
- [ ] Verify `toml::from_str(include_str!(\"../../../.code-graph.toml.example\"))` parses cleanly in a smoke test (or rely on an existing such test if one exists). The smoke test guarantees the example file remains TOML-valid even when nothing is uncommented

### Notes
The "do not add these" warning in the inline comments is load-bearing. Without it, a UE user reading the preset and seeing it's "macros to strip" will add every macro they see, including `WITH_EDITOR`, which will produce confusing parse failures whose root cause is hard to debug.

## 4.5: CLAUDE.md updates

### Subtasks
- [ ] Open `CLAUDE.md`. Locate the C++ supported-list section (per the existing CppMacroStrip pattern — look near the `Macro-prefixed classes` bullet)
- [ ] Add the new bullet from the verification field, immediately after the existing `macro_strip` bullet so the two are visually adjacent
- [ ] Locate the Cache invalidation section. Edit BOTH adjacent sentences:
  - Line 137: `Changes to [cpp].macro_strip or [extensions] do NOT retroactively re-parse files with unchanged mtime.` → add `[cpp].macro_strip_with_args` to the comma-separated list
  - Line 138: `To apply new macro_strip or to evict entries moved to [extensions].disabled: re-run analyze_codebase with force=true.` → add `new macro_strip_with_args` after `new macro_strip` in the same enumeration. **Both edits are required** — a user reading only line 138 must learn the new field also requires `force=true`
- [ ] Locate C++ Limitation 7 (raw-string-delimiter collision, line 164). Edit the limitation text to mention both `macro_strip` AND `macro_strip_with_args` as triggers, AND add a sentence about the two-pass interaction (pass-1 may corrupt the raw-string tag before pass-2's `skip_lexical` runs)
- [ ] Run `grep -c 'macro_strip_with_args' CLAUDE.md` — confirm >= 4 (one per edit: supported list, line 137, line 138, limitation 7)
- [ ] Run `grep -nE 'macro_strip[^_]' CLAUDE.md` — review each remaining single-field mention to confirm none is in a cache-invalidation or limitation context where it should be widened

### Notes
The cache-invalidation sentence is the most surgical edit. Get the exact wording right (it's a comma-separated list inside a single sentence; adding the new field is a one-token insertion). A new sentence or new paragraph would be wrong.

## 4.6: Structural verification

### Subtasks
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Run `cargo fmt --all --check`
- [ ] Run `cargo test --workspace`
- [ ] Run `make snapshot-clean`
- [ ] If any snapshots regenerated from Phase 4, run `cargo insta review` deliberately
- [ ] Confirm the C++ dogfood baselines stay within ±10%: `cargo test -p code-graph-lang-cpp fmt`, `... curl`, `... abseil-cpp`

### Notes
Phase 4 may regenerate the tools-list snapshot if any handler was touched — verify by running and observing `make snapshot-clean`. Expected: zero regenerations (this phase touches only docs, fixtures, and tests).

## Acceptance Criteria
- [ ] UE-style fixture lives in-tree, `git add -f`'d if necessary
- [ ] Integration test asserts `AActor`/`UObject`/`UActorComponent` extract with preset; diamond walk correct
- [ ] Anti-regression test pins zero-symbols-without-preset as today's behavior
- [ ] `.code-graph.toml.example` ships the UE preset with caveats
- [ ] CLAUDE.md updated in 3 places per the verification field
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
- [ ] `make snapshot-clean` passes
- [ ] fmt/curl/abseil-cpp baselines within ±10%
