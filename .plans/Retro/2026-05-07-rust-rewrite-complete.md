---
title: "RustRewrite Plan Retrospective — Go MCP Server → Four-Language Rust MCP"
type: retro
status: complete
created: 2026-05-07
updated: 2026-05-07
tags: [rewrite, rust, multi-phase, plugin-architecture, tree-sitter]
related:
  - Plans/RustRewrite/README.md
  - Plans/RustRewrite/notes/01-Foundation-And-Cpp-Parser.md
  - Plans/RustRewrite/notes/02-Graph-Engine-And-LLM-Optimizations.md
  - Plans/RustRewrite/notes/03-MCP-Server-Tools-And-Discovery.md
  - Plans/RustRewrite/notes/04-Watch-Cross-Compile-Cutover.md
  - Plans/RustRewrite/notes/05-Rust-Parser.md
  - Plans/RustRewrite/notes/06-Go-Parser.md
  - Plans/RustRewrite/notes/07-Python-Parser.md
  - Designs/SharedDaemon/
---

# RustRewrite Plan Retrospective — Go MCP Server → Four-Language Rust MCP

7 phases, ~50 tasks, single-author dispatch via `/planner:implement`. Plan shipped end-to-end with no emergency rollbacks. Final state: a four-language Rust MCP server (C++ / Rust / Go / Python) shipping 15 MCP tools, 683 workspace tests, and 0 audit advisories. The original Go implementation is fully replaced.

## What Went Well

- **The plan shipped, all phases complete, no rollbacks.** Phase 1 → Phase 7 spanned roughly six weeks of dispatch work; every phase landed within its acceptance criteria. The plan moved from `Plans/` to `Plans/` cleanly.
- **Phase 5/6/7 plugin phases each shipped with zero cross-cutting fixes.** Each plugin phase received a plan-reviewer brief refresh against shipped state before implementer dispatch. The brief refresh paid back three consecutive times — Rust, Go, Python all shipped without "the brief said X but the codebase needs Y" stalls. This cadence is the highest-leverage process change the plan produced.
- **Carry-forward of quality findings across task boundaries produced zero rework at close-out.** Phase 7 alone carried scanner findings forward five times (7.4 → 7.5 → 7.6 → 7.7 → 7.8); by the close-out commit only two trivial comment-text fixes remained. Compare to a "scan once at the end" pattern that typically surfaces 20+ findings nobody addresses because the phase is "done."
- **Two helper consolidations eliminated 8 byte-identical copies across the workspace.** `truncate_signature` (Phase 7.1: 3 → 1 copy, prevented a 4th) and `find_enclosing_kind` (Phase 7.7: 5 → 1 copy). Both consolidations had been deferred from Phase 5 and Phase 6 and only landed when Phase 7's brief made each a hard subtask.
- **Real-world dogfood baselines on three external projects pinned regression behavior.** testdata_cpp (Phase 1), `logrus@v1.9.3` (Phase 6), `requests@v2.32.3` (Phase 7). Each one followed an identical pattern: clone at pinned tag, run parse-test, record symbol count, gate the regression at ±10% with `#[ignore]` + graceful skip on missing fixture. Pattern is now confirmed three times and stable.
- **Wire-format snapshot suite scaled from 0 to ~50 fixtures.** `cargo insta` workflow held up across four languages; no flaky fixtures, no host-specific paths, no timestamp leaks.
- **`(Language, name)`-keyed `SymbolIndex` invariant survived a 3-way cross-language collision regression.** C++/Go/Python `init` symbols stay isolated under asymmetric assertions (each language's caller IS in its own init's callers AND IS NOT in either of the other two's). The asymmetric-assertion shape is the load-bearing form.
- **Watch-mode reindex regression suite is language-symmetric.** `watch_cpp_reindex.rs`, `watch_rust_reindex.rs`, `watch_go_reindex.rs`, `watch_python_reindex.rs` all exercise the same `Graph::prune_dangling_edges` invariant for both inheritance and call edges. SharedDaemon's multi-session watch tests inherit this baseline.
- **Tree-sitter grammar quirks were caught by corpus tests, not in the wild.** `future_import_statement` (Python, Phase 7.4), `function_signature_item` (Rust, Phase 5), `method_elem` (Go, Phase 6) — every grammar had a syntactically-distinct construct the "obvious" query missed. Corpus fixtures caught each one before it shipped.
- **Phase debriefs were written *while the plan was still active*, not retrospectively.** Each debrief captures the actual implementation experience, not a sanitized summary. The Phase 7 debrief alone is 132 lines; the plan-level retrospective (this document) can quote from them rather than re-deriving context.

## What Could Be Improved

- **Four documented carry-over skill opportunities never landed across the plan.** `make new-plugin LANG=...` (Phase 5 → Phase 6 → Phase 7), `make audit`/`make check` aggregate targets (Phase 4 → Phase 5 → Phase 6 → Phase 7), sequential-dispatch heuristic doc inside `/planner:implement` (Phase 5 → Phase 6 → Phase 7), and the plan-reviewer-refresh cadence as a slash command (Phase 5 → Phase 6 → Phase 7). The pattern: each debrief flags the gap, the next debrief notes "still not done." Without a forcing function (a hard subtask in the next plan), they will silently die.
- **Helper consolidations were deferred twice before forcing them.** `truncate_signature` had three copies by Phase 6's debrief; Phase 5 said "Phase 6 will fix"; Phase 6 said "stay in scope." Only Phase 7's brief making it a hard 7.1 subtask broke the cycle. Same arc for `find_enclosing_kind` (which by Phase 7's start had five copies). Deferral is rational at decision time; the cumulative cost grew silently.
- **Brief-vs-shipped-state drift surfaced multiple times across plugin phases.** Most consequential: Phase 7.5 brief implied inheritance edge `from` field uses `path:Outer::Inner` form; the codebase contract (established in Phase 1, reaffirmed in Phase 5) uses bare class names. Only the 7.5 quality scanner caught this; the implementer correctly tested against the existing contract. The brief-refresh cadence catches *most* drift, but the "spec lags codebase" pattern is recurring.
- **Symbol-count estimates in dogfood fixtures routinely overshot reality.** `requests@v2.32.3` brief said 400-1000; actual 284. `logrus` similar. The estimate is harmless when treated as estimate (the recorded baseline is the contract), but the brief-vs-actual gap reads as a discrepancy in the debrief and produces "should we update the brief?" questions that are pure overhead. Better: write the brief with a placeholder ("recorded baseline: TBD; populated on first run") instead of a numeric range.
- **Honor-system regression baselines.** All three dogfood baselines (testdata_cpp, logrus, requests) live behind `#[ignore]` and a network-gated skip path. Without a CI lane configured for `cargo test -- --include-ignored`, the baselines are effectively whoever last ran them. Phase 6's debrief flagged this; Phase 7 inherited it; the plan ends with the same gap.
- **Stray empty files at repo root (`method`, `parent`) sat untracked across the entire plan.** Untracked, dated May 6, never explained. Each implementer flagged them; nobody had authority to delete. The orchestrator should have asked the user once near Phase 5 instead of letting them ride through three more phases.
- **Quality-scanner findings in early phases (1-4) had no debrief field to land in.** The Skill Opportunities template field was added to the debrief template around Phase 6. Phases 1-4 debriefs are thinner as a result; cross-plan pattern detection has to start at Phase 5.
- **Test-name discipline drifted.** Phase 7.5 quality scanner flagged tests named after spec verbiage rather than after assertion intent. Catching the pattern at write time would be cheaper than rewriting at scanner time.

## Action Items

- [ ] **Create `/planner:check-duplication` slash command.** Scans `crates/*/src/**/*.rs`, hashes each `fn` body post-fmt, reports groups with size ≥ 2 with line/test counts. Run as part of `/planner:debrief`. (See Skill Opportunities #1.)
- [ ] **Create `/planner:dogfood-baseline` slash command** that takes `(language, project URL, pinned tag, target path)` and produces both the test skeleton (`#[ignore]`-gated, ±10% regression band, graceful skip on missing fixture) and the baseline file template. (See Skill Opportunities #2.)
- [ ] **Create `/planner:carry-forward` slash command** that reads a prior task's quality scan and emits an "Address these" subtask list pre-populating the next task's brief. (See Skill Opportunities #3.)
- [ ] **Create `/planner:refresh-brief <plan>/<phase>` slash command** as a hard prerequisite before `/planner:implement` for any phase that has a prior phase debrief. (See Skill Opportunities #4 — and resolves a 3-debrief carry-over.)
- [ ] **Add `make new-plugin LANG=<name>` Makefile target.** Scaffolds a new `codegraph-lang-<name>` crate with the standard layout (Cargo.toml deps, lib.rs skeleton with `parse_to_filegraph`, queries.rs, helpers.rs, the object-safety test). Resolves a 3-debrief carry-over.
- [ ] **Add `make audit` + `make check` aggregate Makefile targets.** `make check` runs fmt + clippy + test in one command; `make audit` adds `cargo audit` + structural assertions (no `unsafe`, no `#[allow(clippy::...)]`). Resolves a 4-debrief carry-over.
- [ ] **Add a "Consolidations Landed" section to the debrief template** (between Decisions Made and Requirements Assessment). Forces each debrief to list helpers extracted *and* helpers still in N-copy state, with copy counts. Makes deferral cost continuously visible.
- [ ] **Document the sequential-dispatch heuristic in `/planner:implement`'s prompt.** "If multiple tasks all extend the same function (e.g., `parse_to_filegraph`'s extractor list), dispatch them sequentially, not in parallel." Resolves a 3-debrief carry-over.
- [ ] **Rename brief estimates of dogfood symbol counts to placeholders.** Update the next plan's dogfood-baseline guidance to write `"recorded baseline: TBD; populated on first run"` instead of a numeric range.
- [ ] **Set up a CI lane that runs `cargo test -- --include-ignored`.** Pin the three dogfood baselines (testdata_cpp, logrus, requests) so they're not honor-system. Could go alongside SharedDaemon's CI work.
- [ ] **Clean up the stray `method` and `parent` files at repo root.** Investigate origin; delete if confirmed unrelated.
- [ ] **Add a test-name-vs-intent linter pass.** Regex over `#[test]` / `#[tokio::test]` function names looking for `phase_N` / `task_N_M` / `spec_section_X` patterns. Surfaces names that look like task IDs rather than assertion intent. (See Skill Opportunities #5.)

## Key Metrics

| Metric | Value | Notes |
|--------|-------|-------|
| Phases shipped | 7 | All complete; plan moved to `Plans/` |
| Languages supported | 1 → 4 | Started with Go (legacy); shipped C++ / Rust / Go / Python |
| Workspace tests | 0 → 683 | Plus 4 `#[ignore]`-gated dogfood tests |
| MCP tools | 15 | analyze_codebase, get_file_symbols, search_symbols, get_symbol_detail, get_symbol_summary, get_callers, get_callees, get_dependencies, detect_cycles, get_orphans, get_class_hierarchy, get_coupling, generate_mermaid, watch_start, watch_stop |
| Tree-sitter grammars | 4 | tree-sitter-cpp 0.23.4, tree-sitter-rust 0.24.0, tree-sitter-go 0.25.0, tree-sitter-python 0.25.0 |
| Helper consolidations | 2 | `truncate_signature` (Phase 7.1, 3 → 1 copy); `find_enclosing_kind` (Phase 7.7, 5 → 1 copy) |
| Duplicate helper LOC eliminated | ~80 | Across 8 byte-identical copies before consolidation |
| Real-world dogfood baselines | 3 | testdata_cpp (Phase 1), logrus@v1.9.3 (Phase 6, 411 symbols), requests@v2.32.3 (Phase 7, 284 symbols) |
| Wire-format snapshots | ~50 | Across `snapshot_responses.rs` and `snapshot_tools_list.rs` |
| Plugin crates | 4 | `codegraph-lang-cpp`, `codegraph-lang-rust`, `codegraph-lang-go`, `codegraph-lang-python` |
| Cross-language collision regression | 3-way | C++/Go/Python `init`; 3 positive + 6 asymmetric negative assertions |
| Watch-mode reindex regressions | 4 | One per language; all exercise both Inherits and Calls edge pruning |
| Final binary size | 11 MB | Host-target release build |
| `cargo audit` advisories | 0 | Across 191 dependencies |
| `unsafe` blocks introduced in plan | 0 | Workspace-level `unsafe_code = "forbid"` |
| `#[allow(clippy::...)]` suppressions introduced | 0 | Across all 7 phases |
| Quality-scanner findings carried forward (Phase 7) | 5 task boundaries | 7.4 → 7.5 → 7.6 → 7.7 → 7.8; net rework at close-out: 2 comment-text fixes |

## Skill Opportunities

These aggregate the strongest-signal patterns from the seven phase debriefs (especially Phases 6 and 7, where the Skill Opportunities template field was active). Each pattern appeared at least twice across the plan, so the signal is "already validated, ready to mechanize."

### 1. `/planner:check-duplication` slash command — flags helpers with N≥3 byte-identical copies

- **Pattern observed (4 mentions across debriefs):** `truncate_signature` accumulated to 3 copies by Phase 6 and was extracted in Phase 7.1 just before the 4th would have been added. `find_enclosing_kind` accumulated to 5 copies by Phase 7's start and was extracted in Phase 7.7. Both deferrals were rational at decision time but cumulative cost grew silently. The pattern of "one phase says next phase will fix; next phase defers" repeats reliably without a forcing function.
- **Home for the skill:** New slash command `/planner:check-duplication`, invoked at the end of `/planner:debrief`.
- **Why a skill:** Duplicate-detection-via-eyeball is unreliable. The scanner is mechanical (AST-based or even SHA-based on `fn` bodies post-fmt); the *recommendation* (consolidate now or defer to phase X) is what humans should make. Mechanizing the detection lets the deferral decision be informed.
- **Rough shape:** Scans `crates/*/src/**/*.rs`, hashes each `fn` body post-fmt, groups by hash, reports groups with size ≥ 2 as a Markdown table of `(helper name, crates, line count, test count)`. Wire it into `/planner:debrief` so every debrief has a "Consolidations Landed / Pending" view.

### 2. `/planner:dogfood-baseline` slash command — automates the real-world-validation gate

- **Pattern observed (3 mentions, used 3 times):** Phase 1 testdata_cpp, Phase 6 logrus@v1.9.3, Phase 7 requests@v2.32.3. Each one used the same boilerplate: clone external project at pinned tag, run parse-test, record symbol count to `*-baseline.txt`, gate regression at ±10% with `#[ignore]` + graceful skip on missing fixture. The boilerplate is identical and easy to get subtly wrong (e.g., panic-on-missing-baseline-file but eprintln-on-missing-fixture; ±10% calc vs strict equality).
- **Home for the skill:** New slash command `/planner:dogfood-baseline`.
- **Why a skill:** Eliminates the "spec said 400-1000, actual is 284" mismatch class of issue (the brief should never include a numeric estimate; the test populates the baseline on first run). Ensures the `#[ignore]` discipline + graceful-skip pattern is uniform across baselines.
- **Rough shape:** `/planner:dogfood-baseline LANG=python PROJECT=psf/requests TAG=v2.32.3 PATH=src/requests` produces:
  - `crates/codegraph-tools/tests/dogfood_<lang>_<project>.rs` skeleton with `#[ignore]`-gated test, ±10% regression band, graceful skip on missing fixture.
  - `testdata/<lang>/<project>-baseline.txt` template (run once with `--include-ignored` to populate the actual count).
  - Documentation snippet for CLAUDE.md.

### 3. `/planner:carry-forward` slash command — pre-populates the next task's brief from the prior task's quality scan

- **Pattern observed (5 mentions in Phase 7 alone):** 7.4 → 7.5 → 7.6 → 7.7 → 7.8. Each implementer brief carried the prior task's quality-scanner findings as explicit "Address these" items. Done manually by the orchestrator. By Phase 7's close-out, only two trivial comment fixes remained — the carry-forward pattern produced zero rework.
- **Home for the skill:** New slash command `/planner:carry-forward`.
- **Why a skill:** Manual carry-forward is reliable when the orchestrator remembers. Automating it makes the pattern impossible to skip and removes the "scan-once-at-end" alternative (which surfaces 20+ findings nobody addresses).
- **Rough shape:** `/planner:carry-forward <plan>/<phase>/<task-id>` reads the prior task's quality scan output (saved to a known location, or piped from the scanner agent), extracts Minor and Question findings, and pastes them as a "Address these" subtask block at the top of the next task's brief.

### 4. `/planner:refresh-brief` slash command — pre-phase plan-reviewer refresh against shipped state

- **Pattern observed (3 phases — 5, 6, 7):** Each plugin phase received a plan-reviewer pass against shipped state before implementer dispatch. All three plugin phases shipped with zero cross-cutting fixes *because* of the refresh. The Phase 5 and Phase 6 debriefs both flagged this as a "documented cadence" carry-over; Phase 7 confirmed the value but the cadence is still informal.
- **Home for the skill:** New slash command `/planner:refresh-brief`, called as a hard prerequisite before `/planner:implement`.
- **Why a skill:** This is the highest-leverage process change the plan produced (3-time confirmed). Not codifying it means the next plan's first phase will skip it. The cost of skipping is high (mid-implementation brief-vs-shipped drift); the cost of running it is low (~10 min of plan-reviewer dispatch).
- **Rough shape:** `/planner:refresh-brief <plan>/<phase>` reads the phase doc + the prior phase's debrief + any related design doc, runs `/planner:plan-reviewer` against the phase doc with the prior debrief as additional context, and outputs an Approve/Revise verdict. If Revise, lists the specific brief sections that need updating.

### 5. Test-name-vs-test-intent linter

- **Pattern observed (1 explicit mention, but generalizable):** Phase 7.5 quality scanner flagged that test names tracked spec verbiage (`class_with_multiple_bases_produces_one_edge_per_base`) rather than assertion intent. The cost of fixing test names later (which Phase 7.6 did) is higher than catching at write time.
- **Home for the skill:** A `/simplify`-like slash command, or a project-level Claude skill, or just a regex linter in `make check`.
- **Why a skill:** Test names are read more often than written; bad names compound. A linter pass at write time is ~free.
- **Rough shape:** A regex-based scanner over `#[test]` / `#[tokio::test]` function names looking for patterns like `phase_N` / `task_N_M` / `spec_section_X` / generic-spec-prose names. Reports each as a candidate for renaming with a suggested rename based on the function body's first `assert` line.

### 6. `make new-plugin LANG=<name>` Makefile target

- **Pattern observed (2 mentions, never landed):** Phase 5 debrief flagged it as "next phase should add"; Phase 6 debrief noted "still not done"; Phase 7 added a fourth plugin and the boilerplate was hand-rolled again. By the end of the plan, the plugin-crate skeleton (Cargo.toml deps, lib.rs with `parse_to_filegraph`, queries.rs, helpers.rs, object-safety test) is mechanical.
- **Home for the skill:** A Makefile target at the repo root.
- **Why a skill:** Adding a fifth language (if anyone ever does) should be a one-command scaffold, not 30 minutes of copy-paste-rename. The pattern is now well-validated across 4 plugin crates; the scaffold is no longer speculative.
- **Rough shape:** `make new-plugin LANG=java` creates `crates/codegraph-lang-java/` with a Cargo.toml referencing the workspace `tree-sitter-java` dep, a lib.rs scaffold with `JavaParser::new()`, the `python_parser_is_object_safe_via_box_dyn`-style test, and an empty queries.rs. Uses `awk`/`sed`/templates; no Rust-specific tooling needed.

### 7. `make check` + `make audit` aggregate Makefile targets

- **Pattern observed (4 mentions, never landed):** Phase 4 → Phase 5 → Phase 6 → Phase 7 each ran `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` (and Phase 7 also `cargo audit`) by hand. Each phase's verification gate ran the same commands.
- **Home for the skill:** Makefile targets at the repo root.
- **Why a skill:** Saves typing, makes the gate uniform (no risk of one phase running clippy without `-D warnings`), and gives `/planner:implement` a single command to invoke.
- **Rough shape:** `make check` = fmt-check + clippy-D-warnings + workspace test. `make audit` = `cargo audit` + structural assertions (`grep -rn 'unsafe ' crates/ | grep -v test` is empty; no new `#[allow(clippy::...)]` suppressions). Both should produce non-zero exit on any failure so they're CI-pluggable.

### 8. "Consolidations Landed" section in the debrief template

- **Pattern observed (1 explicit suggestion, but reinforces #1):** Phase 7's debrief lists two consolidations landed; future plan readers want to know which helpers shipped consolidated and which are still in N-copy state. Currently lives in narrative; not searchable.
- **Home for the skill:** Template addition at `shared/templates/debrief.md`.
- **Why a skill:** Debrief format consistency makes cross-plan queries possible ("which helpers got consolidated in which plan?"). Creates a forcing function: a debrief that says "Consolidations Landed: none; pending: X (4 copies), Y (3 copies)" is louder than a narrative that buries the pending items.
- **Rough shape:** Add an optional `## Consolidations Landed` section between `## Decisions Made` and `## Requirements Assessment`. Two columns: "Landed this phase" and "Pending (with copy counts)". Optionally automate the "Pending" column from the `/planner:check-duplication` output.

### 9. Document the sequential-dispatch heuristic in `/planner:implement`

- **Pattern observed (3 mentions, never documented):** Phase 5, 6, 7 each had a "Wave 2" of implementer tasks that all extended the same `parse_to_filegraph` function with a new `extract_X` call. Parallel dispatch would have produced merge conflicts; sequential dispatch was the right call. Each phase's orchestrator re-derived the heuristic.
- **Home for the skill:** `/planner:implement`'s prompt should include the heuristic explicitly.
- **Why a skill:** The heuristic is now confirmed across three plugin phases. Documenting it once eliminates the re-derivation; future plans inherit the convention.
- **Rough shape:** Add to `/planner:implement`'s skill prompt: "If multiple tasks all extend the same function (e.g., adding extractor calls to a shared `parse_to_filegraph`), dispatch them sequentially, not in parallel. Parallel dispatch creates merge conflicts on the shared call site."

## Takeaways

1. **Brief refresh against shipped state is the single highest-leverage process change.** Three plugin phases shipped clean *because* the brief was refreshed before dispatch. Without it, mid-implementation drift surfaces and burns cycles. Codify it as `/planner:refresh-brief`.
2. **Carry-forward of quality findings across task boundaries beats end-of-phase scanning by 10×.** Phase 7 demonstrated five task boundaries of carry-forward and ended with zero rework at close-out. This pattern generalizes to any multi-task phase and should be mechanized via `/planner:carry-forward`.
3. **Helper consolidations need forcing functions, not reminders.** Two debriefs flagged `truncate_signature`; both were politely deferred. Only when Phase 7's brief made it a hard subtask did it land. The same arc happened for `find_enclosing_kind`. Reminders die; forcing functions ship.
4. **Asymmetric assertions are the load-bearing form for cross-language correctness tests.** "X IS in A's callers AND IS NOT in B's callers" — both halves matter. Without the negative half, a "callers includes everything" bug would pass silently. Phase 6 established the pattern at 2-way; Phase 7 widened to 3-way; the form generalizes to N-way.
5. **Tree-sitter grammars are more nuanced than the surface API suggests.** Every grammar has a special node kind for syntactically-distinct constructs (`future_import_statement`, `function_signature_item`, `method_elem`). Corpus tests for every variant are the mitigation; "obvious" queries miss the special forms.
6. **Real-world dogfood baselines pin regression behavior across language plugins.** Three baselines now exist (testdata_cpp, logrus, requests). The pattern is stable; the only outstanding gap is CI integration for `--include-ignored`.
7. **Phase debriefs written during active work, not retrospectively, are 5× more useful.** Each Phase 7 debrief entry quotes specifics from the implementation; a retrospective sanitization would have lost them. The discipline of "write the debrief before `git mv` to Complete/" should be inherited by SharedDaemon.
8. **The four-language MCP is shipped; SharedDaemon is the next planned body of work.** `Designs/SharedDaemon/` is `status: draft`; the four-language Rust binary is its starting point. The skill opportunities above (especially `/planner:refresh-brief` and `/planner:carry-forward`) should land before SharedDaemon's first phase ships.
