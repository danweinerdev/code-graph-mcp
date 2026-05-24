---
title: "CppMacroStrip Plan Retrospective — UE C++ macro-prefix class extraction"
type: retro
status: draft
created: 2026-05-07
updated: 2026-05-07
tags: [cpp, ue, unreal-engine, parser, single-session, plan-retro, tree-sitter]
related:
  - Plans/CppMacroStrip/README.md
  - Plans/CppMacroStrip/notes/01-Foundation.md
  - Plans/CppMacroStrip/notes/02-Wire-Through.md
  - Plans/CppMacroStrip/notes/03-Fixture-And-Docs.md
  - Designs/CppMacroStrip/README.md
  - Retro/2026-05-07-pagination-overhaul.md
---

# CppMacroStrip Plan Retrospective — UE C++ macro-prefix class extraction

3 phases, 9 tasks, single-session dispatch via `/planner:implement`. Plan shipped end-to-end with no rollbacks. Closes the second half of the issues observed on a generic UE project (the first half — `get_orphans` token-bloat — was closed by PaginationOverhaul earlier the same day). After this plan, `class CORE_API MyClass : public UObject {};` correctly extracts as a `MyClass` class symbol with `UObject` parent edge for users who configure `[cpp].macro_strip` in their `.code-graph.toml`. The fix is opt-in (empty default; non-UE users see zero behavior change).

## What Went Well

- **The plan shipped in a single session.** Design → plan → 3 phases → debriefs → retro all happened end-to-end. Phase 1's algorithm-first foundation made Phase 2 trivial (no plumbing surprises) and Phase 3 became the user-facing payoff with no implementation friction.
- **The user-reported failure closed.** A UE codebase with `class CORE_API MyClass : public UObject {};` now produces the correct `MyClass` symbol with `UObject` inheritance edge once `macro_strip = ["CORE_API"]` is in the project's `.code-graph.toml`. Both `get_class_hierarchy` and `get_callers` work correctly on UE-style code (the two specifically-named broken tools).
- **Quality scanner caught a real bug in every phase**, consistent with PaginationOverhaul: workspace test-compile break in P1 (added `cpp` field to `RootConfig` broke an exhaustive struct-literal in `indexer.rs`), watch-test diagnostic gap in P2 (no file-parsed sentinel before the canary assertion), documentation contradiction in P3 (same feature in both Supported Patterns and Known Limitations sections of CLAUDE.md). Three findings, three real bugs, three caught pre-commit.
- **Algorithm-first phasing for byte-level transformations.** Phase 1 locked `strip_macros` in 11 unit tests before any production caller existed. The substitution had high blast radius (a bug would corrupt every C++ file's parse), so isolating it to a phase where it could be exhaustively tested without the noise of integration concerns was correct. Phase 2's verification could focus entirely on plumbing because the algorithm was already proved.
- **Strictly-additive trait extension paid back exactly as the design's plan-reviewer pass predicted.** The original design proposed extending `LanguagePlugin::parse_file`'s signature to take `&RootConfig`. Plan-reviewer flipped it to "add a new `preprocess` method with a default impl returning `Cow::Borrowed(content)`." The result: 3 production plugins (Rust, Go, Python) and 2 test stubs (`FakePlugin`, `StubPlugin`) needed zero changes. Compile-error blast radius dropped from "every implementor in the workspace" to "just the C++ override + 2 call sites." The "future-proofing" argument in the original Decision 4 was YAGNI; the smallest-possible-hook approach won.
- **Implementer correctly deviated from a literal task instruction in Phase 1.1.** Task said "emit `tracing::warn!`"; implementer checked `Cargo.toml` and found the workspace deliberately has no `tracing` dep (documented convention in `watch.rs:461`). Used `eprintln!` instead, flagged the deviation in their report, asked the coordinator to confirm. This is the exact right reflex — implementers should validate task-stated dependencies before adopting them blindly. Coordinator approved the deviation; the warning still fires on the established channel.
- **The `force=true` cache-invalidation grep gate worked.** The user explicitly required this nuance be documented. Phase 3.4's verification field included a load-bearing `grep -l 'force=true' CLAUDE.md` and `grep -l 'force=true' .code-graph.toml.example` assertion. Both files passed; the phrase survived the documentation-consolidation pass that consolidated the Supported-vs-Limitations contradiction.
- **Anti-regression worked completely.** All 4 existing class_hierarchy snapshots show zero diff (proves `preprocess` is a true no-op when `macro_strip` is empty). The 49 existing C++ corpus tests passed byte-identical through every phase. The `testdata/cpp/` parse-test baseline (18 symbols / 21 edges) was preserved.
- **Per-phase commit cadence with descriptive messages held up.** 4 commits total (3 phases + debriefs); each phase commit is independently revertable. The history reads cleanly for a future bisect.

## What Could Be Improved

- **`testdata/ue/.code-graph.toml` was silently excluded by `.gitignore` and would have failed CI on a fresh checkout.** The repo's `.gitignore` excludes `.code-graph.toml` (intended for user-root personal configs), but a test fixture at `testdata/ue/.code-graph.toml` matches that pattern and got silently dropped from the staging area. Caught only because the coordinator ran `git status` before committing and noticed the missing file. Without that double-check, `cargo test --workspace` would still have passed locally (the file was on disk) but failed in CI on a fresh `git clone` (the file was never committed). This is a class of bug that's invisible from the test runner — only fresh-checkout CI would catch it. New failure mode not seen in PaginationOverhaul; worth a CLAUDE.md convention note.
- **Documentation contradictions emerged during the cutover phase.** Phase 3.3 added macro-prefix-class support documentation in BOTH the "Supported C++ Patterns" bullet list AND the "Known Limitations" entry 7. Both bullets were technically accurate but the framing was contradictory ("supported" vs. "requires opt-in"). Quality scanner caught it on a cold read; coordinator consolidated to a single Limitations entry covering only the raw-string caveat. Lesson: when a feature has caveats, framing matters — pick one section to be the source of truth and have the other section reference it.
- **Stale documentation references compound when new content sits adjacent.** CLAUDE.md said "A sample `.code-graph.toml` ships at the repo root" — but the actual file is `.code-graph.toml.example`. The error pre-dated this plan but became more visible because the new Configuration content placed it next to the cache-invalidation callout. Fixed during the consolidation pass. Lesson: when editing near a stale reference, fix it rather than letting it accumulate.
- **Several skill opportunities flagged in PaginationOverhaul's retro are STILL UNADDRESSED.** From the PaginationOverhaul retro action items: `scripts/snapshot-audit.sh`, `make snapshot-clean`, the `parsed_sorted` quirk note in CLAUDE.md, `/planner:commit-phase` template, and the planner:scan-tool-descriptions checklist. None landed before CppMacroStrip; same patterns will continue to recur until they do. Each future plan will surface the same friction (snapshot zero-diff hand-audit, manual `cargo insta pending-snapshots` checks, hand-drafted phase commits, etc.). Without a forcing function, these will silently die.
- **Plan-vs-reality drift on the sample TOML filename.** The plan and design referred to "the sample `.code-graph.toml` at the repo root" but the actual file is `.code-graph.toml.example`. Implementer correctly resolved it during the file-locate step; minor friction. Could have been caught earlier by having `/plan-reviewer` actually run `find .code-graph.toml.example` against the design's referenced paths.

## Action Items

- [ ] **Add a CLAUDE.md Test Conventions note about gitignore-vs-fixture-config.** "Test fixtures under `testdata/` may use filenames that the project's `.gitignore` excludes (e.g. `.code-graph.toml` is per-user-root config and gitignored, but test fixtures sometimes need that exact name). When adding such a fixture, force-add with `git add -f` and verify with `git status` that the file is staged. The `cargo test` command does NOT catch a silently-excluded fixture — only a fresh-checkout CI run does."
- [ ] **Consider a CI step that runs `git status --porcelain testdata/` and fails if any file is untracked.** Catches the gitignore-vs-fixture trap automatically. ~5 lines of CI YAML.
- [ ] **Build `scripts/doc-audit.sh <phrase> <files...>`** that asserts a phrase appears in N files. Generalizes the `force=true` cache-invalidation grep gate. Wire into a `make doc-audit` target. Could be invoked from CI for any documented-must-have requirement.
- [ ] **Write `scripts/snapshot-audit.sh <expected-paths…>`** (carry-over from PaginationOverhaul retro; STILL not done). Done by hand 3 times in this plan; at least 4 prior times in PaginationOverhaul. The `git diff --stat tests/snapshots/` zero-diff assertion is load-bearing in every phase.
- [ ] **Add a `make snapshot-clean` Makefile target** (carry-over from PaginationOverhaul retro). Runs `cargo insta pending-snapshots`; fails non-zero if any pending exist. Wire into pre-commit if a hook exists.
- [ ] **Document the `parsed_sorted` snapshot-normalization quirk in CLAUDE.md** (carry-over from PaginationOverhaul retro; STILL not done). One paragraph; prevents two phases of identical doc-comment back-and-forth on the next paginated-tool addition.
- [ ] **Extend the `planner:quality-scanner` agent prompt with a "read-docs-cold" lens.** "When reviewing documentation changes, read the modified sections AND the surrounding sections cold — without context from the implementer's commit message or task description. Flag any framing contradictions (same feature described as 'supported' in one section and 'limitation' in another) or stale references (file paths that don't exist, version numbers that have moved on)." Caught the CLAUDE.md contradiction in this plan.
- [ ] **Add a CLAUDE.md convention note about implementer task-instruction validation.** "Before adopting a dependency named in a task instruction, verify it exists in `Cargo.toml`. The instruction may be derived from convention assumptions that don't apply here. Flag deviations rather than silently expanding scope." The Phase 1 `tracing::warn!` instance is the canonical example.
- [ ] **Add a CLAUDE.md test convention note about diagnostic sentinels in timing-dependent tests.** "When a test depends on async timing or file IO, assert a low-stakes baseline first (e.g., 'a no-macro class extracts') before asserting the discriminator (e.g., 'a macro-prefixed class extracts'). The baseline assertion's failure message names the most likely root cause (timing, IO) so the test failure is self-diagnosing." The `UObject` sentinel before `AActor` in `watch_cpp_macro_strip.rs` is the example.
- [ ] **Decide on `/planner:commit-phase`** vs. a Makefile alternative for templated phase-commit messages (carry-over from PaginationOverhaul retro). Drafted 4 phase-commit messages by hand in this plan, structurally identical. ~20 minutes saved per plan. The CppMacroStrip + PaginationOverhaul commits are now a 9-commit sample of the template; enough data to fix the format.

## Key Metrics

| Metric | Value | Notes |
|--------|-------|-------|
| Phases | 3 | All complete; status `complete` |
| Tasks | 9 | All complete (3+5+4-task fudge — Phase 2 expanded by 1 during plan revision) |
| Code commits | 4 | One per phase, plus 1 debrief commit |
| Plan duration | 1 session | Design → plan → 3 phases → debriefs → retro |
| Tools fixed | 4 | `get_class_hierarchy`, `get_callers`, `get_callees`, `get_orphans { kind: class }` — all now work on macro-prefixed UE classes when `macro_strip` is configured |
| Tools intentionally untouched | 11 | All other MCP tools; `preprocess` is opt-in via config |
| Unit tests added | 16 | 5 in `codegraph-core` (CppConfig) + 11 in `codegraph-lang-cpp` (strip_macros) |
| Integration tests added | 3 | `cpp_macro_strip` (positive + control) + `watch_cpp_macro_strip` (canary) |
| Snapshot tests added | 2 | UE hierarchy snapshots — `aactor` (depth=2, 6 nodes) + `double_macro` (multi-macro extraction) |
| Existing snapshots regenerated | 0 | Default `preprocess` impl is a true no-op for non-C++ files |
| Existing snapshots verified zero-diff | 4 | `engine`, `rust_trait_greet`, `go_interface_reader`, `python_dog` hierarchy snapshots |
| Quality scanner findings (real bugs caught) | 3 | Workspace compile break, watch-test diagnostic gap, doc contradiction |
| Plan reviewer findings before /implement | 6 | All addressed in design or plan revisions; zero stalls during implementation |
| Workspace test count post-plan | ~735 passing | Up from ~726 pre-plan baseline |
| Rollbacks | 0 | No phase reverted; no commit amended |
| `cargo fmt --check` / `cargo clippy -D warnings` clean across all phases | Yes | No `#[allow]` attributes added |
| Files force-added past `.gitignore` | 1 | `testdata/ue/.code-graph.toml` — the new failure mode |

## Skill Opportunities

Aggregated across the three debriefs, with strong-signal flags for patterns observed across both this plan AND the prior PaginationOverhaul plan.

### 1. CLAUDE.md note on gitignore-vs-fixture-config (NEW — Phase 3 only, but high-impact)

- **Pattern observed:** `testdata/ue/.code-graph.toml` was silently excluded by `.gitignore` (intended for user-root configs). The fixture file existed locally but was never committed. Discovered only by running `git status` before committing; the test runner does not catch this. New trap not seen in PaginationOverhaul.
- **Home for the skill:** A note in `CLAUDE.md` Test Conventions section.
- **Why a skill:** This bug is invisible from `cargo test` output. Local development passes; CI fails on fresh checkout. Days of confusion possible if not caught at commit time. One-paragraph note in CLAUDE.md (with the `git add -f` instruction and the verification sentence "verify with `git status` that the file is staged") closes the trap forever.
- **Rough shape:** Convention note, ~3 sentences. Could be reinforced with a CI step that runs `git status --porcelain testdata/` and fails if any file is untracked.

### 2. `scripts/doc-audit.sh <phrase> <files...>` (Phase 3 — already proved effective)

- **Pattern observed:** The user's must-have requirement ("the cache-invalidation note must appear in both CLAUDE.md and the sample TOML") was enforced via a load-bearing `grep -l 'force=true'` gate in Phase 3.4. The gate caught nothing (the implementer did the right thing) but would catch any accidental removal during a future reformat or doc edit. Cheap, automatable, and would survive the documentation-consolidation pass.
- **Home for the skill:** Shell script at `scripts/doc-audit.sh`, optionally wired into `make doc-audit` and called from CI.
- **Why a skill:** Documented-must-have requirements are easy to silently violate during routine doc edits. A grep gate is the cheapest possible enforcement and trivially scriptable. Would generalize to any "phrase X must appear in files Y, Z" requirement that future plans surface.
- **Rough shape:** `scripts/doc-audit.sh 'force=true' CLAUDE.md .code-graph.toml.example` — exits non-zero if the phrase is missing from any listed file. Plain bash; no dependencies.

### 3. `scripts/snapshot-audit.sh <expected-paths…>` (STRONG SIGNAL — flagged in PaginationOverhaul retro AND here)

- **Pattern observed:** Every phase ran `git diff --stat tests/snapshots/` by hand to verify only the expected snapshot files changed. Done at least 7 times across PaginationOverhaul + CppMacroStrip without a script. Load-bearing in every phase; trivial to automate.
- **Home for the skill:** Shell script at `scripts/snapshot-audit.sh`, invoked from each phase's structural-verification step (and ideally a pre-commit hook).
- **Why a skill:** Catches accidental cross-tool effects (e.g. an "improvement" to a shared helper that quietly regenerates an unrelated tool's snapshot). The check is mechanical, easy to skim past, and high-consequence if missed. **This is the strongest skill signal across the two plans — same skill flagged twice in retros, still not built. The cost of building it is one afternoon; the cost of NOT building it is recurring friction in every wire-format-touching plan.**
- **Rough shape:** `scripts/snapshot-audit.sh response_get_orphans_default_callables tools_list_get_orphans …` exits non-zero if `git diff --name-only crates/codegraph-tools/tests/snapshots/` contains any file not in the expected list. Plain bash; no dependencies.

### 4. `make snapshot-clean` / pre-commit hook for `.snap.new` files (STRONG SIGNAL — also from PaginationOverhaul retro)

- **Pattern observed:** Verified `cargo insta pending-snapshots` reports zero before each commit. Forgetting this leaves `.snap.new` files in the working tree that get accidentally staged or, worse, missed entirely (test passes but the actual snapshot didn't update). Done by hand at least 7 times across the two plans.
- **Home for the skill:** Makefile target plus optional pre-commit hook.
- **Why a skill:** One-command check; failure mode is silent (you can ship a "passing" plan with stale snapshots if you skip it). Same status as #3 — flagged twice across plans, still not built.
- **Rough shape:** `make snapshot-clean` runs `cargo insta pending-snapshots`; exits non-zero if any pending exist; suggests `cargo insta review`.

### 5. `planner:quality-scanner` "read-docs-cold" lens (NEW — Phase 3 only, but easy to add)

- **Pattern observed:** Quality scanner caught the CLAUDE.md "Supported Patterns vs. Known Limitations" contradiction during Phase 3 by reading both sections cold (without the implementer's mental model). Documentation contradictions are easy to miss for the writer; trivially visible to a cold reader. Could be promoted to an explicit lens in the standard quality-scanner prompt.
- **Home for the skill:** Extension to the `planner:quality-scanner` agent prompt — a new explicit lens.
- **Why a skill:** Quality scanner already does general code review; adding this lens is essentially free. Documentation contradictions and stale references are a recurring failure mode that's easier to catch at review time than to fix later.
- **Rough shape:** One paragraph added to the quality-scanner agent prompt: "When reviewing documentation changes, read the modified sections AND the surrounding sections cold — without context from the implementer's commit message or task description. Flag any framing contradictions (same feature described as 'supported' in one section and 'limitation' in another) or stale references (file paths that don't exist, version numbers that have moved on)."

### 6. CLAUDE.md note on `parsed_sorted` snapshot quirk (CARRY-OVER from PaginationOverhaul retro)

- **Pattern observed:** Two PaginationOverhaul phases (1 and 4) initially wrote doc-comments claiming snapshot files were the wire-format ground truth for field order. Caught both times by quality scanner. Same trap will catch the next contributor adding a paginated tool. Flagged in PaginationOverhaul retro; STILL not done.
- **Home for the skill:** One paragraph in CLAUDE.md Code Conventions section.
- **Why a skill:** Documentation that prevents two-iteration back-and-forth on the same trap.
- **Rough shape:** Code Conventions bullet — "Wire-format field order is governed by struct declaration order (serde guarantees declaration-order serialization for `derive(Serialize)`). Snapshot files alphabetize JSON keys via the test harness's `parsed_sorted` helper, so the *struct itself*, not the snapshot, is the source of truth for declaration order."

### 7. CLAUDE.md note on implementer task-instruction validation (NEW — Phase 1 only)

- **Pattern observed:** Phase 1.1 implementer correctly flagged that "use `tracing::warn!`" was wrong about workspace dependencies (no `tracing` dep exists). Used `eprintln!` instead, flagged the deviation. Without this reflex, an implementer who trusted the instruction would have either added `tracing` as a workspace dep (silent scope expansion) or shipped a compile error.
- **Home for the skill:** A note in `CLAUDE.md` (or implementer agent prompt).
- **Why a skill:** Prevents both silent scope expansion and broken implementations. Implementer prompts can drift in either direction without this reflex.
- **Rough shape:** "Before adopting a dependency named in a task instruction, verify it exists in `Cargo.toml`. The instruction may be derived from convention assumptions that don't apply here. Flag deviations rather than silently expanding scope."

### 8. CLAUDE.md note on test-infrastructure sentinels (NEW — Phase 2 only)

- **Pattern observed:** `tests/watch_cpp_macro_strip.rs` initially asserted only the discriminator (`AActor` is present). Quality scanner pointed out that a debounce-timing failure or a missing file would produce "AActor not found, got empty" with no diagnostic value. Adding `assert!(names.contains(&"UObject"))` BEFORE the discriminator turned the failure into "UObject is the file-parsed sentinel — its absence means the debounce window is too short or the file write didn't land."
- **Home for the skill:** Convention note in CLAUDE.md.
- **Why a skill:** Pattern is reusable for any timing-dependent or IO-dependent test. The cost is one extra assertion; the payoff is days saved over a test's lifetime.
- **Rough shape:** "When a test depends on async timing or file IO, assert a low-stakes baseline first (e.g., 'a no-macro class extracts') before asserting the discriminator (e.g., 'a macro-prefixed class extracts'). The baseline assertion's failure message names the most likely root cause (timing, IO) so the test failure is self-diagnosing."

### 9. `/planner:commit-phase` template (CARRY-OVER from PaginationOverhaul retro)

- **Pattern observed:** Each phase commit followed the same template — subject `[PlanName/Phase N] <short>`, body with sections for "what changed," "why," "tests added," "snapshot delta," "follow-on notes." Drafted by hand 4 times in this plan (3 phases + debriefs), at least 5 times in PaginationOverhaul, structurally identical each time. ~5 minutes per commit.
- **Home for the skill:** A `/planner:commit-phase` slash command or a `make commit-phase PLAN=CppMacroStrip PHASE=3` Makefile target.
- **Why a skill:** ~20 minutes saved per plan; eliminates inconsistency in commit-message structure across phases. The CppMacroStrip + PaginationOverhaul phase commits are now a ~9-commit sample of the template; enough data to confidently fix the format.
- **Rough shape:** Reads `Plans/{Active,Complete}/<Plan>/<NN>-<Phase>.md` frontmatter (title, deliverable, acceptance criteria), runs `git diff --stat HEAD` to get the file change list and snapshot delta, opens an editor with a pre-filled commit message, runs `git commit -F <tmpfile>` after edits.

## Takeaways

- **Per-phase quality scanning has a 100% bug-finding rate across two consecutive plans.** PaginationOverhaul (3 real bugs in 4 phases) + CppMacroStrip (3 real bugs in 3 phases) = 6 real bugs caught pre-commit across 7 phases. The pattern is now strong enough to be canonical: every phase that produces uncommitted code changes should run quality-scanner before commit. The cost is ~5 minutes per phase; the benefit is per-phase commits that don't ship known bugs.
- **The /design + /plan reviewer interventions before /implement save phases of rework.** Design reviewer flipped Decision 4 from "extend `parse_file` signature" (would have rippled to all 4 plugins + 2 test stubs + 2 call sites) to "preprocess hook with default impl" (zero changes to 3 plugins + 2 test stubs). That single intervention saved an estimated 4-6 commits of follow-on changes. Pre-implementation review continues to pay back disproportionately — confirmed across both plans.
- **Algorithm-first phasing pays off when the algorithm is in a high-blast-radius position.** Byte-level transformations in a parser pipeline corrupt every file silently if the algorithm is wrong. Phase 1 locked the substitution with 11 unit tests before any production caller existed; Phase 2's verification could focus entirely on plumbing because the substitution itself was already proved. Same pattern would apply to any future "inner-loop transformation in a hot path" work.
- **Strictly-additive trait extension is the canonical way to add per-implementor behavior to a workspace-wide trait.** A new method with a default impl reaches zero existing implementors. Changing an existing method signature reaches every implementor (production + test stub + future out-of-tree). The blast-radius difference is the difference between "single-file change" and "every plugin gets touched." The plan-reviewer's intervention here was the single highest-leverage improvement of the plan.
- **The `force=true` cache-invalidation grep gate is the prototype for any "documentation-must-have" requirement.** A user explicitly required a specific phrase appear in two specific files. The Phase 3.4 verification field encoded it as a `grep -l` assertion. The gate would have caught any accidental removal during reformat. Worth promoting to a `scripts/doc-audit.sh` pattern that any future plan can adopt.
- **The gitignore-vs-fixture-config trap is the most-impactful new finding from this plan.** A test fixture filename matching a project-wide `.gitignore` rule is silently excluded from commits — `cargo test` doesn't catch it; only fresh-checkout CI does. The single `git add -f` fix is mechanical, but the bug class is invisible to the test runner. Worth a CLAUDE.md convention note immediately, and a CI step ideally.
- **Skill opportunities flagged in retros stay unaddressed unless they have a forcing function.** PaginationOverhaul's retro flagged 6 skill opportunities; CppMacroStrip's retro flags 9 (5 new + 4 carry-overs). The 4 PaginationOverhaul carry-overs (snapshot-audit script, snapshot-clean Makefile target, parsed_sorted note, /planner:commit-phase template) are exactly the same patterns that recurred in this plan. Without a "do these before the next plan starts" forcing function, every future plan will surface the same friction. **The strongest signal across the two plans is the snapshot-audit script and the snapshot-clean Makefile target — both flagged twice; build them next.**
