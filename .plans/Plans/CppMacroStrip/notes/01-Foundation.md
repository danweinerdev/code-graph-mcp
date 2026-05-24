---
title: "Phase 1 Debrief: Foundation — CppConfig + strip_macros algorithm"
type: debrief
plan: CppMacroStrip
phase: 1
phase_title: "Foundation — CppConfig + strip_macros algorithm"
status: complete
created: 2026-05-07
updated: 2026-05-07
tags: [cpp, tree-sitter, ue, unreal-engine, parser, config]
---

# Phase 1 Debrief: Foundation — CppConfig + strip_macros algorithm

## Decisions Made

- **Algorithm-first phasing held up.** The substitution algorithm got 11 unit tests in isolation before any production code path called it. When Phase 2 wired it through, every existing test continued to pass byte-identical — the default-no-op claim was already proved at the algorithm boundary, not just at the integration boundary. Pattern worth keeping for any future "byte-level transformation in a parsing path" work.
- **Implementer correctly deviated from a literal task instruction.** Task 1.1 said "emit `tracing::warn!` per dropped entry." The implementer checked the workspace's dependencies and found `tracing` is NOT a workspace dep — there's a deliberate "no tracing dep" convention documented in `watch.rs:461` and consistently followed by `indexer.rs`, `discovery.rs`, and `watch.rs` (all use `eprintln!`). The implementer used `eprintln!`, flagged the deviation in their report, and asked the coordinator to confirm. This is exactly the right judgment call — implementers should validate task-stated dependencies before adopting them blindly. Coordinator accepted the deviation; the warning still fires, just on the established channel.
- **Belt-and-suspenders on the empty-pattern guard.** The design specified the empty-string filter at config-load time as the primary defense. The implementer additionally added a runtime guard in `strip_macros` itself (`if pat.is_empty() { continue; }`) on top of the existing `debug_assert!`. This was a quality-scanner suggestion — the public `pub fn strip_macros` and `pub macro_strip` field together create a future-misuse surface that the config-load filter doesn't cover (a benchmark, integration test, or future caller could construct a `CppConfig` directly and pass empty strings). The runtime no-op is cheap and prevents the only correctness failure that matters (infinite loop on empty pattern).
- **Test-name-vs-assertion alignment after quality scan.** `case_k_byte_offset_preservation` originally computed `original_my_class_col = 15` but never asserted on it — `Symbol.column` reports the start of `class_specifier` (always column 0), not the column of `MyClass`. Strengthened the test with two new assertions: (a) negative assertion that the symbol's `signature` does NOT contain `"CORE_API"` (proves bytes were actually substituted, not the pre-substitution source); (b) byte-position scan over the cleaned bytes asserting `MyClass` survives at the same byte offset (15) as in the original. The test now actually discriminates the contract it claims.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| `CppConfig` exists in `RootConfig` with `#[serde(default)]` and the empty-string filter applied at config-load | Met | Plus belt-and-suspenders runtime guard |
| `strip_macros` exists in `codegraph-lang-cpp` with the full correctness suite passing | Met | 11 cases (a)–(k); case (k) strengthened post-scan |
| All 49 existing C++ corpus tests pass byte-identical | Met | No corpus test exercises macro-prefixed classes |
| No snapshot files regenerate (nothing in the parse pipeline calls `strip_macros` yet) | Met | `git diff --stat tests/snapshots/` empty |
| `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace` all clean | Met | After fixing the test-helper compile break (see Risks below) |
| Phase 2 has a fully-specified substitution function and config struct to consume | Met | Phase 2's verification confirmed it |

## Deviations

- **`tracing::warn!` → `eprintln!`** for the empty-string drain warning. The task instruction was wrong about an existing dependency; the workspace convention won. Implementer's call, coordinator-approved.
- **Strengthened `case_k`** beyond the original verification text. Quality scanner flagged that the original assertions wouldn't fail under a buggy implementation; the test was upgraded to actually discriminate. Net positive.
- **Defensive runtime guard in `strip_macros`.** Not in the verification text; added based on quality scanner finding. The scanner's reasoning (public fields + public function = future-misuse surface) was sound.

## Risks & Issues Encountered

- **Workspace test compile broke as a side effect of adding `cpp` to `RootConfig`.** `cfg_with_threads` test helper at `crates/codegraph-tools/src/indexer.rs:374` had an exhaustive `RootConfig { discovery, parsing }` struct literal without `..Default::default()`. Adding the `cpp` field made it a compile error. Quality scanner ran `cargo test --workspace` and surfaced the failure; coordinator added the trailing `..Default::default()` (one-line fix). **The lesson:** when adding a field to a widely-shared struct, `git grep` for direct struct-literal initializers across the workspace before declaring victory. The compiler catches this on workspace test build, but the iteration cost is annoying.
- **`.code-graph.toml` baseline file exists in repo root but the actual file at the root is `.code-graph.toml.example` — not `.code-graph.toml`.** Discovered in Phase 3, not this phase, but the foundation for that confusion was laid here (the `RootConfig::load` doc still references `<root>/.code-graph.toml`, which is correct as the *runtime* file path; the *example* file in the repo is named differently).

## Lessons Learned

- **Implementers should validate task-stated dependencies, not adopt them blindly.** The `tracing::warn!` instruction was technically correct as a pattern but wrong about whether the dep existed. An implementer who trusted the instruction would have added `tracing` to `Cargo.toml` (silent scope expansion) or shipped broken code. The implementer's "I checked, this isn't actually a workspace dep" is the right reflex.
- **Algorithm-first phasing pays back when the algorithm is in a bytes-in/bytes-out path.** Byte-level transformations in a parser pipeline are exactly the kind of code where a subtle off-by-one or boundary error corrupts every file silently. Locking the algorithm with 11 tests before any production caller exists makes the integration phase trivial — Phase 2's verification could focus entirely on plumbing because the substitution itself was already proved.
- **Test names that promise more than they deliver are documentation lies.** `case_k_byte_offset_preservation` originally promised byte-offset preservation but only asserted line==1 and column==0 — both of which would survive any byte-shifting bug because `class_specifier` always starts at column 0 of `class`. The strengthened version (signature negative assertion + byte-position scan over cleaned bytes) makes the test actually fail under the bug class it claims to catch.

## Impact on Subsequent Phases

- **Phase 2 inherited a clean substitution algorithm + config struct.** No surprises in the wire-through. The only Phase 2 failure mode was missing call sites, not algorithm correctness — which is exactly what algorithm-first phasing is supposed to deliver.
- **The `..Default::default()` fix in `cfg_with_threads`** prevents the same compile break from recurring as future fields are added to `RootConfig` (any future `[python]` or `[rust]` config sections will inherit defaults cleanly).
- **The strengthened `case_k` is a regression baseline** — any future change to the substitution algorithm that breaks byte-offset preservation will fail this test specifically. Worth keeping intact across refactors.

## Skill Opportunities

- **What you did repeatedly:** Verified that workspace dependencies stated in task instructions actually exist before using them. Did this in Phase 1.1 (`tracing::warn!` → discovered no `tracing` dep) and would do it again any time an instruction names a dep.
  **Where it belongs:** A note in `CLAUDE.md` (or implementer agent prompt) saying: "Before adopting a dependency named in a task instruction, verify it exists in `Cargo.toml`. The instruction may be derived from convention assumptions that don't apply here. Flag deviations rather than silently expanding scope."
  **Why a skill:** Prevents both silent scope expansion (adding deps) and broken implementations (using imports that don't exist). Cheap to document.
  **Rough shape:** One paragraph in CLAUDE.md `Code Conventions` section.

- **What you did repeatedly:** Ran `cargo test --workspace` after every struct-field addition to `RootConfig` (or any other widely-shared struct) to catch exhaustive struct-literal initializers that don't use `..Default::default()`.
  **Where it belongs:** A clippy lint configuration or a project-level convention note: "When adding a field to a struct used as `T { field1, field2 }` (exhaustive literal) anywhere in the codebase, the field must either be required of all initializers, or the struct's existing initializers must use `..Default::default()`. Prefer the latter for backward-compat."
  **Why a skill:** This is one of the few Rust mistakes the compiler doesn't catch until full workspace build. Cheap to document; saves an iteration.
  **Rough shape:** Convention note. Could be enforced with a `clippy::struct_excessive_bools`-adjacent lint if one exists, but doc-only is fine.
