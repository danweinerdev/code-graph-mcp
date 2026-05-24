---
title: "Phase 3 Debrief: Fixture, snapshot, docs, cutover"
type: debrief
plan: CppMacroStrip
phase: 3
phase_title: "Fixture, snapshot, docs, cutover"
status: complete
created: 2026-05-07
updated: 2026-05-07
tags: [cpp, tree-sitter, ue, unreal-engine, parser, config]
---

# Phase 3 Debrief: Fixture, snapshot, docs, cutover

## Decisions Made

- **`testdata/ue/.code-graph.toml` force-added past `.gitignore`.** The repo's `.gitignore` excludes `.code-graph.toml` because that filename is reserved for the user's per-root config — they shouldn't commit their personal indexing settings. But the plan needed to commit a `.code-graph.toml` *as a test fixture* under `testdata/ue/`. Discovered when `git status` showed the file as untracked even after the implementer reported the fixture complete. Resolved with `git add -f testdata/ue/.code-graph.toml`. Without this, the snapshot test would have silently failed in CI on a fresh checkout — `analyze_codebase` would have run with default `RootConfig` (empty `macro_strip`), the macro-prefixed classes would have failed to extract, and the snapshot assertion would have produced a confusing "expected AActor, got empty hierarchy" error.
- **Documentation contradiction caught by quality scanner; consolidated to a single Limitations entry.** Initial Phase 3 implementation documented macro-prefixed class support in BOTH the "Supported C++ Patterns" bullet list AND the "Known Limitations" entry 7. Both descriptions were technically accurate but the framing conflicted ("supported" vs. "requires opt-in"). Coordinator consolidated by reframing entry 7 to be specifically about the raw-string-delimiter caveat (the actual remaining limitation; macro-prefix support itself is now a feature) and rewriting the Supported Patterns bullet to point at it. Single source of truth per concern.
- **Pre-existing stale `.code-graph.toml` reference fixed.** CLAUDE.md said "A sample `.code-graph.toml` ships at the repo root" — but the actual file is `.code-graph.toml.example`. The error pre-dated this plan but became more prominent because the new Configuration content placed it adjacent to the cache-invalidation callout. Coordinator updated the wording to "A sample `.code-graph.toml.example` ships at the repo root; copy it to `.code-graph.toml`..." Stale-doc cleanup as a side effect of doing the work nearby.
- **Cache-invalidation note enforced via `grep` check.** The user explicitly required that the `force=true` cache-invalidation guidance appear in both CLAUDE.md and the sample TOML. Phase 3.4's verification field included a load-bearing `grep -l 'force=true'` assertion. Both files passed (CLAUDE.md line 114; `.code-graph.toml.example` lines 46-47). The grep check survived the documentation-consolidation pass — both files still contain the phrase verbatim.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| `testdata/ue/MyActor.h` and `testdata/ue/.code-graph.toml` exist with the documented contents | Met | `.code-graph.toml` force-added past gitignore |
| 2 new UE hierarchy snapshots added and approved | Met | `response_get_class_hierarchy_ue_aactor` (6 unique nodes) + `response_get_class_hierarchy_ue_double_macro` (multi-macro) |
| All 4 existing hierarchy snapshots show zero diff | Met | Engine, Rust trait, Go interface, Python dog — all byte-identical |
| `CLAUDE.md` documents the `[cpp]` schema, the cache-invalidation nuance, and adds a NEW C++ Limitations entry | Met | After consolidation pass; existing macro-generated-definitions entry unchanged |
| Sample `.code-graph.toml.example` has the commented-out UE macro list with inline cache-invalidation note | Met | UE macros + `force=true` callout |
| `crates/codegraph-lang-cpp/src/lib.rs` doc comments updated | Met | Module + `CppParser` + `LanguagePlugin` impl all reference `preprocess` |
| Cache-invalidation note grep verified | Met | `force=true` appears in both CLAUDE.md (1 hit) and sample TOML (2 hits) |
| `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace` clean | Met | Workspace fully green |
| `cargo insta pending-snapshots` zero | Met | All snapshots accepted |

## Deviations

- **`.code-graph.toml.example`, not `.code-graph.toml`, is the actual filename in the repo root.** The plan and design referred to it as "the sample `.code-graph.toml` at the repo root." This was a plan-vs-reality mismatch. The implementer correctly used the `.example` file when locating "the sample TOML" and the grep gate was updated to point at the correct path. No real deviation in intent — just a naming gap.
- **Documentation consolidation was a quality-scanner-driven post-implementation pass**, not in the original Phase 3.3 task list. The implementer correctly added the Supported Patterns bullet AND the Limitations entry per the plan's literal instructions; the contradiction only emerged when both were read together. Coordinator's consolidation removed the contradiction without changing the documented behavior.

## Risks & Issues Encountered

- **The gitignore-vs-fixture-config interaction is the highest-impact risk in this plan and would have escaped to CI.** A fresh `git clone` followed by `cargo test --workspace` would have failed the new UE snapshot tests because `testdata/ue/.code-graph.toml` would have been absent — `RootConfig::load` would resolve to empty `macro_strip`, the macro-prefixed classes would not extract, and the snapshots would mismatch. Discovered locally only because the implementer's own `git status` after the implementation included the file as untracked, and the coordinator ran `git status` again before committing. Without that double-check, the silently-broken state would have shipped. Documented in Phase 2 → Phase 3 lesson and as a cross-cutting skill opportunity.
- **Documentation drift between plan/design and reality.** The plan referred to "the sample `.code-graph.toml`" without acknowledging that the file is named `.example` in the repo. Minor friction during implementation; corrected during the implementer's file-locate step.

## Lessons Learned

- **`.gitignore` rules designed for user-generated artifacts can silently exclude test fixtures with the same name.** This is a real and recurring trap. The convention "`.code-graph.toml` is gitignored because it's a per-user artifact" makes sense at the user-root level but doesn't anticipate "test fixtures sometimes need to be that exact filename for the loader to find them." The fix (`git add -f`) is mechanical but invisible from `cargo test` output alone. Convention worth documenting: any test fixture using a filename matching a project-wide `.gitignore` rule must be `git add -f`'d, ideally with an inline `.gitkeep` or fixture-builder convention to make the expectation explicit.
- **Documentation contradictions emerge most easily when two sections are written from different mental models in the same pass.** The "Supported Patterns" list and the "Known Limitations" list are both bullet-list shapes; both were updated by the same implementer in the same session for the same feature. The contradiction was invisible to the implementer who held both bullets as accurate descriptions of different aspects (support exists; limitation exists). Quality scanner — reading the two sections cold without context — caught it immediately. Reading docs cold (without the writer's model) is a high-leverage activity.
- **Pre-existing stale references compound when new content sits adjacent.** The "ships at the repo root" stale reference for `.code-graph.toml` had been in CLAUDE.md unchanged for some time. Adding new Configuration content next to it didn't trigger an immediate need to fix the stale line — but it did make the contradiction more visible to readers, who would naturally look at the example file when reading the new schema. Worth a habit: when editing near a stale reference, fix it.

## Impact on Subsequent Phases

- **None — this was the final phase.** Plan moved Active → Complete; status `complete`.
- **Plan-level retrospective candidate:** the gitignore-vs-fixture interaction is a cross-cutting issue worth surfacing in a `/retro CppMacroStrip` doc as an action item (e.g., a CLAUDE.md convention or a `tests/fixtures/` README that calls this out for future fixture authors).

## Skill Opportunities

- **What you did repeatedly:** `git add -f` on a test fixture file matching a project-wide `.gitignore` rule. Did this once in Phase 3 but the trap is recurring — every future test fixture using a "user-artifact" filename will hit it.
  **Where it belongs:** A note in `CLAUDE.md` Test Conventions section: "Test fixtures under `testdata/` may use filenames that the project's `.gitignore` excludes (e.g. `.code-graph.toml` is per-user-root config and gitignored, but test fixtures sometimes need that exact name). When adding such a fixture, force-add with `git add -f` and verify with `git status` that the file is staged. The `cargo test` command does NOT catch a silently-excluded fixture — only a fresh-checkout CI run does."
  **Why a skill:** Prevents a class of bugs where local development "works" but CI silently fails. The bug is invisible from the test runner; only `git status` reveals it.
  **Rough shape:** Convention note. Could be enforced with a CI step that runs `git status --porcelain testdata/` and fails if any file is untracked, but doc-only is acceptable.

- **What you did repeatedly:** Read documentation sections cold (without the writer's mental model) to catch contradictions. Did this via quality-scanner dispatch in Phase 3.3.
  **Where it belongs:** Make this an explicit lens in the standard `planner:quality-scanner` prompt: "When reviewing documentation changes, read the modified sections AND the surrounding sections cold — without context from the implementer's commit message or task description. Flag any framing contradictions (same feature described as 'supported' in one section and 'limitation' in another) or stale references (file paths that don't exist, version numbers that have moved on)."
  **Why a skill:** Documentation contradictions are easy to miss for the writer; trivially visible to a cold reader. Quality scanner already does general code review; adding this lens is essentially free.
  **Rough shape:** One paragraph added to the quality-scanner agent prompt or shared/orchestration.md.

- **What you did repeatedly:** Used `grep -l <phrase>` as a load-bearing documentation verification ("the cache-invalidation note must appear in both CLAUDE.md and the sample TOML"). Worked perfectly — the grep gate is scriptable, automatable, and survived the consolidation pass.
  **Where it belongs:** Generalize as a `scripts/doc-audit.sh <phrase> <files...>` shell script that asserts a phrase appears in N files. Could be invoked as part of CI for any documented-must-have requirement.
  **Why a skill:** Already proved effective in this plan. The cost of accidental documentation regression is high (a silently-removed `force=true` note would mislead users); the cost of running a grep gate is near-zero.
  **Rough shape:** `scripts/doc-audit.sh 'force=true' CLAUDE.md .code-graph.toml.example` — exits non-zero if the phrase is missing from any listed file. Could be wired into `make doc-audit` and called from CI.
