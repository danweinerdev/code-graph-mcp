---
title: "Phase 2 Debrief: Wire-through — preprocess hook + call sites + integration"
type: debrief
plan: CppMacroStrip
phase: 2
phase_title: "Wire-through — preprocess hook + call sites + integration"
status: complete
created: 2026-05-07
updated: 2026-05-07
tags: [cpp, tree-sitter, ue, unreal-engine, parser, config]
---

# Phase 2 Debrief: Wire-through — preprocess hook + call sites + integration

## Decisions Made

- **`preprocess` hook with default impl (Decision 4 from the design) was the right call in retrospect.** The plan-reviewer's intervention to flip the design from "extend `parse_file` signature" to "add `preprocess` default-impl method" paid back exactly as predicted: 3 production plugins (Rust, Go, Python) and 2 test stubs (`FakePlugin`, `StubPlugin`) needed zero changes. Compile-error blast radius dropped from "everywhere" to "just the C++ plugin and 2 call sites." The "future-proofing" argument the design originally used to justify the broader change was YAGNI; the smallest-possible-hook approach is what shipped.
- **Watch-mode integration test was required, not optional.** This was a reviewer-flagged change to the plan. Phase 2 confirmed the value: `tests/watch_cpp_macro_strip.rs` is the ONLY test that exercises `preprocess` in `try_reindex_file`. Every existing watch test uses Rust/Go/Python fixtures that inherit the default no-op `preprocess` impl, so a broken wiring (e.g. passing `RootConfig::default()` to `preprocess` in the watch handler) would slip through their assertions silently. The canary test catches this specifically.
- **`cached_cfg` → `cfg_for_blocking` rename collapsed.** Quality scanner flagged it as a needless intermediate binding (`cached_cfg` was assigned at line 259 and never read between 259 and 295, then renamed to `cfg_for_blocking` at 295 right before the `move ||` closure). Coordinator dropped the intermediate binding and renamed at the declaration site. One fewer line, no readability loss.
- **`UObject` sentinel assertion before `AActor` check in the watch test.** Quality scanner pointed out that the original watch test asserted `AActor` presence but had no "did the file even parse?" sentinel. A debounce-timing failure or a missing file would produce "AActor not found" with no diagnostic value. Added an `assert!(names.contains(&"UObject"))` BEFORE the `AActor` check so debounce/IO problems produce a distinguishable error message ("UObject is the file-parsed sentinel — its absence means the debounce window is too short or the file write didn't land"). Test now distinguishes "preprocess wiring is broken" from "the test infrastructure is flaky."

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| `LanguagePlugin::preprocess` exists with a default impl returning `Cow::Borrowed(content)` | Met | Strictly additive; no existing `parse_file` signature changes |
| `CppParser` overrides `preprocess` to call `strip_macros(content, &cfg.cpp.macro_strip)` | Met | Override is 3 lines |
| Indexer and watch handler both call `preprocess` before `parse_file` | Met | One-line insert each, both inside their respective parallel/blocking sections |
| End-to-end integration test (positive case) confirms macro-prefixed classes extract | Met | `cpp_macro_strip_extracts_class_with_api_macro` |
| Control test (using IDENTICAL fixture, differing only in config) confirms zero classes when `macro_strip` is empty | Met | Both tests reference the same `MY_ACTOR_HEADER` constant |
| Watch-mode integration test confirms macro-prefixed classes extract on file-change events | Met | `tests/watch_cpp_macro_strip.rs` with UObject + AActor assertions |
| All 49 existing C++ corpus tests pass byte-identical | Met | Algorithm-first phasing meant Phase 2 was just plumbing |
| All existing snapshots show zero diff | Met | Default `Cow::Borrowed` impl is a true no-op for non-C++ files |
| `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace` clean | Met | Plus 18 symbols / 21 edges parse-test baseline preserved |
| No test stub or out-of-tree implementor needed updates | Met | Decision 4's main payoff |

## Deviations

- **None on the plan path.** Implementer followed tasks 2.1–2.4 verbatim. Two post-implementation polishes (collapse the `cached_cfg` rename, add the `UObject` sentinel) addressed quality-scanner findings, not deviations from the plan.

## Risks & Issues Encountered

- **No real risks materialized in this phase.** The Phase 1 algorithm-first investment paid off — every Phase 2 failure mode would have been a plumbing bug, and the integration tests + zero-snapshot-diff audit catch all of them.
- **The `Cow<'a, [u8]>` lifetime threading through the trait method's named-lifetime parameter** could have been a subtle compile-error trap (associated-function lifetime parameters need to match the input lifetime). Worked correctly the first time. Object-safety check at `crates/codegraph-lang/src/lib.rs:443-445` (`assert_object_safe::<dyn LanguagePlugin>()`) defends against future regressions here at build time.
- **800ms watch-mode debounce sleep could be CI-flaky on heavily loaded runners.** The pattern matches `watch_race.rs` precedent (which has been stable in CI), so the risk is the same as existing watch tests. Not a new flake source.

## Lessons Learned

- **Strictly-additive trait extensions are massively cheaper than signature changes.** This is a now-confirmed pattern: adding a method with a default impl reaches zero existing implementors. Changing an existing method signature reaches every implementor (production + test stub + future out-of-tree). Always prefer the additive path when the new behavior is opt-in. The plan-reviewer's intervention here was load-bearing.
- **A canary test that's the ONLY thing exercising a code path is worth far more than its line count.** `watch_cpp_macro_strip.rs` is ~140 lines including setup, but it's the difference between "shipped feature" and "shipped feature where 50% of the wiring is silently broken." Every existing watch test would have passed with the watch-handler `preprocess` call commented out. Reviewer-required canary tests should be the rule, not the exception, whenever a change touches a code path that existing tests don't exercise.
- **Diagnostic sentinels before discriminator assertions improve test maintainability disproportionately.** "AActor not found, got empty []" is 30 seconds of head-scratching. "UObject not found — debounce window too short or file write failed" tells you exactly what to investigate. The cost is one extra assertion; the payoff is days saved over a test's lifetime.

## Impact on Subsequent Phases

- **Phase 3 inherited a working, fully-wired pipeline.** The fixture + snapshot tests in Phase 3 only had to verify the user-facing payoff — they didn't need to debug pipeline issues. Phase 2's "default `preprocess` is a true no-op for non-C++ files" claim let Phase 3 confidently assert that the 4 existing `class_hierarchy` snapshots show zero diff (which they did).
- **The `cached_cfg` clone strategy in `try_reindex_file`** means watch-mode picks up the config that was active at the most-recent `analyze_codebase` — config edits to `.code-graph.toml` between `analyze_codebase` calls don't take effect until the next index run. This is the correct semantics (consistent with how `max_threads` and other knobs behave) and is documented in Phase 3's CLAUDE.md update.

## Skill Opportunities

- **What you did repeatedly:** Verified the "no other plugin or test stub touched" claim by reading every plugin's `LanguagePlugin` impl and both test stubs. Did this manually as part of Phase 2.1's verification.
  **Where it belongs:** A `scripts/audit-trait-impls.sh <trait_name>` shell script that lists every `impl <Trait> for ...` block in the workspace. Useful for any future trait-extension work.
  **Why a skill:** Confirms the additive-trait-extension promise. When a default-impl method is added, this script lets the implementer or reviewer assert "exactly these N implementors exist; only the new C++ override is materially changed; the others retain default behavior."
  **Rough shape:** `scripts/audit-trait-impls.sh LanguagePlugin` runs `cargo expand` or grep for `impl LanguagePlugin for` and lists the match locations + which methods each block actually defines (vs. inherits).

- **What you did repeatedly:** Inserted "test-infrastructure sentinel" assertions before discriminator assertions. Did this in `tests/watch_cpp_macro_strip.rs` (`UObject` before `AActor`) and would do it any time a test depends on async timing or file IO.
  **Where it belongs:** A note in `CLAUDE.md` test conventions: "When a test depends on async timing or file IO, assert a low-stakes baseline first (e.g., 'a no-macro class extracts') before asserting the discriminator (e.g., 'a macro-prefixed class extracts'). The baseline assertion's failure message names the most likely root cause (timing, IO) so the test failure is self-diagnosing."
  **Why a skill:** Cheap to document, makes flaky-test failures actionable. Pattern is reusable for any timing-dependent test.
  **Rough shape:** Convention note in CLAUDE.md.
