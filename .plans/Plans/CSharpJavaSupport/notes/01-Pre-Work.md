---
title: "Phase 1 Debrief: Pre-Work — Workspace Plumbing"
type: debrief
plan: "CSharpJavaSupport"
phase: 1
phase_title: "Pre-Work — Workspace Plumbing"
status: complete
created: 2026-05-08
updated: 2026-05-08
tags: [language-plugin, c-sharp, java, tree-sitter, multi-language]
---

# Phase 1 Debrief: Pre-Work — Workspace Plumbing

Two tasks (1.1, 1.2), four commits, two review-fix cycles. All structural gates green on touched crates. Phase 1 cleanly unblocks Phases 2 and 3.

## Decisions Made

- **Pinned both grammars at `=0.23.5`** — `tree-sitter-c-sharp = "=0.23.5"` and `tree-sitter-java = "=0.23.5"`, matching the strict-pin convention used by the four shipped grammars. Compatibility with `tree-sitter` core 0.26 was verified via a scratch crate at `/tmp/ts-probe` before committing the pins. The probe resolved `tree-sitter v0.26.8` cleanly with no version conflicts.
- **Extended both existing serde tests, not just one.** The brief assumed at most one round-trip test would exist; the codebase had two — `language_serializes_lowercase` (dedicated) and `symbol_round_trip_every_kind_and_language` (Cartesian product). Both extended for symmetry, ensuring CSharp/Java round-trip end-to-end through the full `Symbol` shape.
- **`disabled` precedence test landed in `code-graph-lang`, not `code-graph-core`.** That's where `LanguageRegistry::language_for_path_with_config` lives and where its precedence contract is already pinned; the existing `with_config_disabled_blocks_*` tests are the model. Round-trip + cross-additive collision tests stayed in `code-graph-core::config::tests`.
- **Cross-additive collision test only covers the new csharp+java pairing.** The O(n²) loop in `RootConfig::load` walks all `C(6,2) = 15` pairs by construction; testing one representative new-vs-new pair is sufficient. Existing tests already cover the old-language pairs.
- **CLAUDE.md "init all six (shallow clones)" count language deliberately NOT bumped.** Per the plan's explicit instruction (4.4 batches the count update). The `[extensions]` built-in-defaults comment WAS bumped in 1.2 because that's a per-language enumeration, not a count.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| Strict-pinned `tree-sitter-c-sharp` and `tree-sitter-java` compatible with tree-sitter core 0.26 | Met | Both `=0.23.5`; probe-verified |
| `Language::CSharp` and `Language::Java` exist; round-trip through serde | Met | Both serde tests extended |
| `parse_language` and `SearchSymbolsInput::language` schema description handle the new languages | Met (after fix-cycle) | Initial 1.1 commit missed this; caught by quality-scanner; fixed in commit `8a2cde2` |
| `ExtensionsConfig` accepts `csharp`/`java`; `lookup_additional`/`lists_mut`/`additive_lists` widened | Met | Array sizes 5→7 and 4→6 confirmed by compile-time check |
| `.code-graph.toml.example` and CLAUDE.md `[extensions]` documentation list 6 language defaults | Met | Both files updated |
| Cross-additive collision and disabled-precedence tests pass | Met | csharp + java symmetric (java symmetric test added in fix-cycle commit `c0c6517`) |
| `cargo build --workspace`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` all pass | Partially Met | First three: pass cleanly. `cargo fmt --all --check`: pre-existing drift in 7-8 unrelated files (NOT from this phase); per-crate fmt-check on touched crates clean |

## Deviations

- **`parse_language` and `SearchSymbolsInput::language` schemars description were NOT updated in 1.1's main commit.** The brief said "DO NOT match-arm-update any consumer of `Language` outside of the round-trip test addition" — guidance intended to preserve `match` exhaustiveness across `#[non_exhaustive]` boundaries. The implementer correctly read this strictly. But the wire-format mapping (string → Language) is a different category from match exhaustiveness, and missing it would have left `Language::CSharp` and `Language::Java` unreachable through `search_symbols` filtering. Caught by `planner:quality-scanner` as a Major finding; fixed in commit `8a2cde2`.
- **Brief assumed one serde round-trip test; codebase had two.** The implementer extended both, which is correct, but the brief should have said "find the existing Language tests and extend them all" rather than assuming a count.
- **Stale comment + asymmetric test from 1.2.** Comment "O(n²) over four typically-tiny lists" went stale when `additive_lists` widened to 6; the disabled-precedence test was added for csharp but not java. Both Minor; both fixed in commit `c0c6517`.
- **Pre-existing workspace-wide fmt drift surfaced.** `cargo fmt --all --check` is red on 7-8 unrelated files (`config.rs` `lists_mut` signature, several `tests/watch_*_reindex.rs` files, `discovery.rs`, `code-graph-lang/src/lib.rs`). The drift predates this phase. Both implementers correctly avoided fixing it (out of scope), but it's now a known liability that Phase 4.4's final structural gate will surface again.

## Risks & Issues Encountered

- **Brief-scope-narrowness produced one Major finding.** The "DO NOT match-arm-update consumers" instruction unintentionally excluded the wire-format mapping in `parse_language` and the agent-facing description text in `server.rs`. Resolution: tighter brief instructions for Phases 2 and 3 — when telling an implementer "DO NOT touch X," enumerate the things they SHOULD touch beyond X (especially agent-facing description fields, which are functionally part of the contract per the project-specific lens in CLAUDE.md).
- **Workspace fmt drift not blockable per-task.** No way to enforce workspace-wide fmt cleanliness without scope-expanding into unrelated files. Resolution: a separate `cargo fmt --all` cleanup pass should land before Phase 4.4, OR Phase 4.4 should explicitly include the cleanup subtask. Adding to "Impact on Subsequent Phases" below.
- **No grammar-version compatibility blocker.** The plan README's Risks section flagged that neither grammar might have a 0.26-compatible release; both did (`=0.23.5`). Probe via scratch crate took ~3 minutes.

## Lessons Learned

- **Quality-scanner is the right tool for "we wrote what we said we'd write but missed an obvious adjacent need."** Both findings (`parse_language` gap, asymmetric java test) were genuinely worth fixing. The intent-blind lens caught them precisely because the implementer was correctly intent-aware. Two-cycle review-fix is a real pattern, not theatre.
- **The `#[non_exhaustive]` annotation on `Language` worked exactly as designed.** No consumer's `match` expression broke when CSharp/Java landed because every non-test consumer either has a `_ => ...` arm or uses the `LanguageRegistry`'s string-keyed dispatch. Confirmed by the implementer's grep for `Language::Cpp\|Language::Rust\|...` matches.
- **Fixed-size array literals are a load-bearing safety net for `ExtensionsConfig` widening.** `lists_mut` returning `[(...); 5]` (now `; 7]`) made the size mismatch a compile error if either added entry was forgotten. This pattern is worth preserving in future widening.
- **The CLAUDE.md "Agent-facing tool descriptions" project-specific lens is real.** Quality-scanner explicitly cited it when flagging the missing `csharp`/`java` strings in `SearchSymbolsInput::language`'s description. The lens's value showed up in its first invocation.
- **The CLAUDE.md "Documentation read cold" lens is also real.** Quality-scanner used it to verify the module-level doc comment in `server.rs:19-21` matched the schemars description after the fix. Caught nothing, but the verification was non-trivial.
- **The brief format's "verification field as dense paragraph naming every gate" pattern carries forward cleanly.** Both 1.1 and 1.2 had implementers who clearly used the verification field as the acceptance test. No ambiguity at task boundaries.

## Impact on Subsequent Phases

- **Phase 2 (C# Plugin) and Phase 3 (Java Plugin) can now dispatch in parallel.** Both depend only on Phase 1, which is complete. The plugins' `id()` impls will reference `Language::CSharp` / `Language::Java` cleanly; users can add `[extensions].csharp` / `[extensions].java` to `.code-graph.toml` and have it deserialize cleanly.
- **Phase 4.4 (final structural verification) needs a workspace fmt cleanup subtask.** The pre-existing drift in 7-8 files will fail `cargo fmt --all --check` at the gate. Two options: (a) add a `cargo fmt --all` cleanup pass as a Phase 4.4 subtask before the gate run, OR (b) land a separate fmt-cleanup commit out-of-band before Phase 4 begins. Option (b) is cleaner — drift should be fixed where it lives, not bundled into a feature phase.
- **Phase 2 and Phase 3 task briefs should tighten the "DO NOT touch X" constraints.** When an implementer is told "do not match-arm-update consumers of Language," explicitly call out that this excludes string-to-enum mappers (`parse_language`-equivalent) and agent-facing description fields. Or, more positively: list the consumers that DO need updating per task and explicitly say "no others should be touched in this commit."
- **The carry-forward convention from PLANNER_IMPROVEMENTS.md is validated.** Both task 1.1 and 1.2 produced findings in their own scan; both were absorbed before moving on. Phases 2 and 3 should keep the same per-task scan + carry-forward rhythm.

## Skill Opportunities

### 1. `/planner:fmt-clean` (or `make fmt-clean` Makefile target)

- **What you did repeatedly:** Both 1.1 and 1.2's gate runs surfaced pre-existing workspace fmt drift. The implementers correctly avoided fixing it, but everyone touching the codebase is paying a low-grade cognitive tax of "is this drift mine or pre-existing?" on every fmt-check. The drift will accumulate until a deliberate cleanup pass.
- **Where it belongs:** Most likely a `make fmt-clean` Makefile target — runs `cargo fmt --all` and asserts the diff is non-empty (so it's a real cleanup commit, not a no-op). Alternative: a dedicated debug "fmt the world" pre-merge ritual baked into the docs.
- **Why a skill:** Removes the cognitive tax of "is this my drift?" Makes drift cleanup a one-command ritual instead of a 5-minute discovery pass. Phase 4.4's final structural gate becomes trustworthy.
- **Rough shape:** `make fmt-clean` runs `cargo fmt --all`, then `git diff --stat` reports what changed, then user reviews and commits as `chore(fmt): cargo fmt --all sweep`. No automation of the commit itself — drift fixes deserve human eyeballs in case fmt rewrites something semantically ambiguous.

### 2. Enum-extension checklist (project-level skill OR brief-template snippet)

- **What you did repeatedly:** Adding two new variants to `Language` exposed three categories of consumer that needed updating: (a) match arms — protected by `#[non_exhaustive]` + `_ =>` catch-alls, no action needed; (b) string-to-enum mappers like `parse_language` — easy to miss because they're outside the type system's reach; (c) agent-facing description text — functionally part of the contract per the CLAUDE.md project-specific lens. The brief for 1.1 covered (a) explicitly but missed (b) and (c). The fix-cycle caught both.
- **Where it belongs:** A short checklist in the project-planner brief template OR a project-level Claude skill at `~/.claude/projects/.../skills/extend-enum.md` that the planner references when the task involves adding enum variants. Could also live as a CLAUDE.md "Enum extension checklist" subsection alongside the Code Conventions section.
- **Why a skill:** Brief-vs-shipped-state drift on this exact pattern (what consumers need updating) was the single largest source of review-fix cycles in Phase 1. Adding new languages will recur (Kotlin, Scala, F#, Swift...); the checklist amortizes across every future enum extension.
- **Rough shape:** Three-item checklist baked into the brief template:
  1. **Match arms:** verify `_ => ...` catch-alls or `#[non_exhaustive]` + minimal-update consumers; if any consumer's `match` is exhaustive without a catch-all, list which ones need updating in this commit.
  2. **String-to-enum mappers:** grep for the function (`parse_language`, `parse_<X>`) and update; extend the `_handles_all_<plural>` test.
  3. **Agent-facing description text:** grep for the enum's old enumerated list (`"cpp, rust, go, or python"`) and update wherever it appears; refresh the corresponding insta snapshot.

### 3. `/planner:fix-minor` — autonomous fix loop for sub-Major scanner findings

- **What you did repeatedly:** Quality scanner returned non-Critical findings (one Major escalated to fix-now, two Minors fixed inline) that needed fixing but didn't warrant a full implementer dispatch. I ended up doing the Minor fixes inline manually (one-character comment edit + ~10-line symmetric test). The Major fix went through a full implementer dispatch which felt heavy for a 3-file, ~20-line change.
- **Where it belongs:** A new `/planner:fix-minor <commit-hash>` slash command that takes a commit's quality-scan findings JSON and dispatches a lightweight implementer (or runs them inline against the orchestrator's tools) for each Minor/Question finding, with a single re-scan at the end.
- **Why a skill:** Right-sizes the implementer dispatch to the size of the fix. Saves ~3-5 minutes of orchestration overhead per Minor finding cycle. Validated 2× in this phase (csharp/java parse_language fix was Major-sized but right-sized; comment+symmetric-test fix was Minor-sized and the inline approach was correct).
- **Rough shape:** Inputs: target commit, scanner findings (Critical/Major escalate to user; Minor/Question auto-fix). Outputs: a single follow-up commit per finding cluster, plus a re-scan report. Invoke automatically after every quality-scanner run that returns non-Critical findings AND the user opts into auto-fix mode.

### 4. Brief-template field for "what consumers may need updating beyond the obvious"

- **What you did repeatedly:** The 1.1 brief listed the things-not-to-touch but not the things-the-implementer-should-also-grep-for. Better briefs would have an explicit "downstream consumers to consider" line, even if the answer is "none beyond the in-scope crates."
- **Where it belongs:** A new optional field in `shared/templates/plan-phase.md` and `frontmatter-schema.md` — `affected_consumers: <list>` or a per-task `update_sites: <list>` field. Or a "Consumer surfaces" subsection in each task body's Notes.
- **Why a skill:** Forces the planner to enumerate downstream surfaces upfront instead of trusting the implementer to discover them. Catches the wire-format-mapping class of bug before code touches the diff.
- **Rough shape:** Per-task frontmatter `affected_consumers: ["crates/code-graph-tools/src/handlers/mod.rs::parse_language", "crates/code-graph-tools/src/server.rs::SearchSymbolsInput::language description"]` (or natural-language equivalent in the task body). Even a free-text "Consumer surfaces" subsection like the existing "Notes" subsection would be a step up.
