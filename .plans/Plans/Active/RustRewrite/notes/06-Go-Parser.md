---
title: "Phase 6 Debrief: Go Language Parser"
type: debrief
plan: "RustRewrite"
phase: 6
phase_title: "Go Language Parser"
status: complete
created: 2026-05-05
---

# Phase 6 Debrief: Go Language Parser

## Decisions Made

- **`GoParser` accepts the default `resolve_call` and `resolve_include`** — does not override either. The default scope-aware heuristic is sufficient for Go's flat package structure (`(Language, name)` keying eliminates the cross-language collision risk that motivates per-language overrides). Default `resolve_include` is correctly a no-op for Go because Go imports are module paths (e.g. `"github.com/sirupsen/logrus"`), not filesystem paths — leaving them unresolved is the intended wire-format behavior. Same call as the C++ and Rust plugins.
- **Generic receivers (`*Server[T]` and `Server[T]`) implemented, not deferred.** The plan-reviewer flagged this as a Question on the 6.2 brief; implementer chose to handle it during 6.2 by descending through `pointer_type → generic_type → type_identifier`. Two helper tests + two lib tests pin the behavior. Symbol parents drop the generic args (parent = `"Server"`, not `"Server[T]"`) so symbol IDs and call-resolution lookups stay textual and stable.
- **`@call.expr` and `@import.spec` outer captures removed from queries.** Sibling C++ and Rust plugins ship with these dead captures; the Go queries removed them after the 6.3/6.4 quality-scanner findings. The line is re-anchored via `find_enclosing_kind`, which is the actual mechanism. Removing the dead capture saves a per-match dispatch and makes the doc-comments accurate. Sibling plugins were intentionally NOT modified — the brief was scoped to Go.
- **`truncate_signature` consolidation deferred (still).** The Phase 5 debrief said Phase 6 would be the moment to extract this helper to a shared module since three byte-identical copies (cpp/rust/go) is the consolidation threshold. The 6.1 implementer was told to duplicate rather than refactor mid-phase to avoid scope creep. Decision held: three copies remain, all byte-identical. Phase 7 will create a fourth (Python). Recommend a dedicated `/simplify`-shaped task to extract before Phase 7 starts. The Go copy *did* gain the C++'s 200-byte fallback test (Minor finding from 6.3 review), so test coverage is now C++/Go = 8 each, Rust = 4 — Rust's coverage is the laggard.
- **Sequential dispatch for Wave 2 (6.2 → 6.3 → 6.4)**. All three tasks extend `parse_to_filegraph` with a new `extract_X` call; parallel work would have produced merge conflicts. This is the same pattern Phase 5 used and is now established as the convention for plugin-extraction phases. Wall-time cost was ~25 minutes total; the diff history reads naturally with each commit cleanly building on the prior.
- **Plan-doc status flips committed alongside implementation commits, not separately.** The 6.5 implementer included the orchestrator's `in-progress`/`complete` status flips in the implementation commit because the working tree had them pending; subsequent tasks followed suit. The phase-finalization commit (`07cbcd4`) is the only pure orchestrator-state-sync commit. This is a slight deviation from the Phase 5 convention but more pragmatic — it keeps the plan tracker actually reflecting what shipped, instead of lagging by one commit.
- **`logrus@v1.9.3` baseline = 407 symbols, 1983 edges, 0 warnings.** Within the 200–500 brief range. Recorded in `testdata/go/logrus-baseline.txt` with a ±10% regression gate (`#[ignore]`-gated, requires `/tmp/logrus`). Test panics on missing baseline file (real bug) but `eprintln! + return`s on missing `/tmp/logrus` (setup gap) — asymmetric on purpose, addressed during 6.6 cleanup.
- **Cross-language collision regression test is the load-bearing assertion of the phase.** `cross_language_init_callers_stay_isolated` builds C++ `void init()` + `void caller_cpp() { init(); }` and Go `func init()` + `func caller_go() { init() }`, then asserts each language's `init` has only its own caller. The asymmetric assertion (`caller_cpp` IS in C++ init's callers AND IS NOT in Go init's callers) only holds under `(Language, name)` SymbolIndex keying — bare-name keying would fail the second half. Phase 3's invariant at `crates/codegraph-lang/src/lib.rs:116` now has a runtime regression test.
- **`>= 1` → `== 1` tightening on `search_init_returns_both_languages`.** Found by 6.6 quality scan: comment said "exactly two" but assertion said "at least one each". Tightened during 6.7 cleanup. Cheap fix, eliminates a false-positive gap (a double-indexing bug would have passed silently).
- **Go interface fixture extracted to `tests/common/mod.rs::GO_INTERFACE_FIXTURE`.** Two test binaries (mixed_language.rs + snapshot_responses.rs) had byte-identical fixture strings. Centralized to a `pub const` during 6.7 cleanup so a future change requires editing only one place.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| GoParser implements LanguagePlugin (object-safety check passes) | Met | `go_parser_is_object_safe_via_box_dyn` test mirrors `crates/codegraph-lang-cpp/src/lib.rs:542-545`; passes in 6.1 |
| All extraction patterns: definitions with method-receiver parent, all import forms, direct + selector_expression calls | Met | 6.2/6.3/6.4 each landed with 11–21 net new tests; total 57 unit + 14 corpus by end of 6.5 |
| Generic receivers (`*Server[T]`, `Server[T]`) handled without crash | Met | Implemented in 6.2, not deferred; 4 tests covering pointer/value × bare/generic forms |
| `macro_rules!`-style anti-regression — embedded struct fields produce zero `Inherits` edges | Met | `embedded_struct_field_produces_no_inherits_edge` (6.2); also asserted at corpus level (testdata/go has 0 Inherits edges across 42 symbols) |
| testdata/go passes | Met | 8 files, 42 symbols (14 Function / 12 Method / 7 Struct / 6 Interface / 3 Typedef), 41 edges (30 Calls / 11 Includes / 0 Inherits), 14 corpus tests pass |
| Real-world dogfood (logrus@v1.9.3) parses cleanly within recorded baseline | Met | 46 files, 407 symbols, 1983 edges, 0 warnings; baseline = 407 with ±10% regression gate |
| Mixed C++ + Rust + Go indexing works | Met | `mixed_cpp_rust_go_indexes_all_three` + 4 sibling tests; `helper` anchor in all three languages discriminated by file extension |
| Cross-language collision regression passes (`init` in C++ vs Go stays isolated) | Met | `cross_language_init_callers_stay_isolated` asserts asymmetric callers; pin for `(Language, name)` SymbolIndex keying |
| Watch-mode reindex regression passes | Met | `watch_go_reindex.rs` (2 tests) exercises symbol delete + add + dangling-edge prune via `Graph::prune_dangling_edges` |
| Go interface `get_class_hierarchy` works | Met | Snapshot fixture `response_get_class_hierarchy_go_interface_reader.snap`; lookup succeeds; bases + derived empty (no structural inheritance) |
| All Phase 1-6 tests pass; lint, format, audit gates clean | Met | 573 workspace tests passing (491 baseline from Phase 5 + 82 net new in Phase 6); `cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo audit` 0 advisories; `make release` produces 10 MB binary |

## Deviations

- **Plan-doc status flips bundled into implementation commits** rather than land-as-separate-orchestrator-commit. Phase 5 kept these separate; Phase 6 mostly bundled them after the 6.5 implementer set the precedent. End result: plan tracker stayed in sync with what shipped, no lag commits. Net positive but a convention drift worth noting for Phase 7.
- **Generic receiver support was a 6.2 addition beyond the original brief.** The brief named pointer + value receivers only; the plan-reviewer Question on 6.1 escalated to "implement or document as limitation" by 6.2. Implementer chose implement; cost was ~30 lines + 4 tests. Worth it — now 6.6 docs don't have to carry an awkward limitation.
- **`truncate_signature` consolidation NOT done.** Phase 5 debrief flagged Phase 6 as the consolidation moment. Brief explicitly told implementer to duplicate, not consolidate. Net: three byte-identical copies persist; Phase 7 will be a 4-copy state. The decision was deliberate scope-discipline; the consolidation now lives as a Skill Opportunity below.
- **Phase doc subtask checkboxes ticked across multiple commits.** The 6.1 → 6.4 implementers each ticked their own checkboxes in the phase doc as they went. The orchestrator's role is to flip the higher-level `tasks[i].status` field. End state is consistent, but two layers of editing the same file is fragile. Phase 7 should pick one: either the implementer ticks checkboxes (and the orchestrator only flips status fields), or the orchestrator does both.
- **`@call.expr` / `@import.spec` cleanup was scoped to Go only.** Sibling C++/Rust plugins still carry the dead captures. The brief explicitly said "leave the siblings alone." Net: a one-liner cleanup that should propagate to siblings is now a tracked follow-up.
- **No commit landed for Task 6.7's verification subtasks themselves.** All gates were green on the as-shipped state from 6.6; the 6.7 commit (`2fca1c1`) only carries the two Minor cleanup fixes from the 6.6 review. The phase-doc verification field is satisfied by the `cargo` invocations themselves, not by a code change.
- **Quality-scanner found a false positive in 6.4 (`>= 1` test in 6.6 review).** This was a real test laxity, not a false positive. The scanner caught a gap that an intent-aware reviewer would have forgiven ("close enough"). Tightened during 6.7. Validates the intent-blind quality-scanner pattern.

## Risks & Issues Encountered

- **`@call.expr` / `@import.spec` dead captures in sibling plugins.** Quality-scanner caught these in Go and they were removed; the same pattern lives in the C++ and Rust queries and was deliberately left alone per the brief. Now a tracked follow-up. Real risk: a future maintainer may see `@call.expr` in a sibling plugin and assume it's load-bearing, then "fix" the Go plugin by adding it back. Mitigation: doc-comments in the Go queries explicitly explain why the capture isn't there.
- **`truncate_signature` test coverage drift between three copies.** C++ has 8 tests, Go now has 8 (gained the 200-byte fallback test in 6.3), Rust still has 4. The Rust copy's coverage is now the laggard — UTF-8 boundary fixes that landed in Rust may not be in C++/Go (or vice versa); the byte-identical claim could silently break. Mitigation: consolidate (Skill Opportunity) before Phase 7 lands.
- **`logrus@v1.9.3` requires network access to clone.** Phase 5's `requests@v2.32.3` (Phase 7) and Phase 6's `logrus@v1.9.3` (this phase) both need network. CI without internet access can't run the dogfood test. Test is `#[ignore]`-gated so default CI is unaffected, but the regression gate is silent when not run. Phase 5 had the same shape; not new. Worth confirming explicitly: did anyone ever wire the dogfood test into a network-allowed CI lane? If not, the regression baseline is honor-system.
- **`cargo test -- --include-ignored` previously panicked on missing `/tmp/logrus`.** Initial 6.5 implementation panicked with a setup-instruction message; 6.6 cleanup changed it to `eprintln! + return`. Cost: now CI can't tell skipped from passed (Question raised by 6.6 scanner). Trade-off accepted — clean output for new contributors beats CI fidelity for a feature gate that nobody's wired up anyway.
- **Phase doc subtask-checkbox / status-field two-layer editing model is fragile.** When two layers of automation edit the same file (implementer ticks subtask checkboxes; orchestrator updates `tasks[i].status`), conflicts are statistically likely. Didn't happen this phase but it's a latent risk. Phase 5 had the same pattern. Mitigation: pick one layer.
- **Sequential dispatch for Wave 2 was the right call but was decided at orchestrator-time, not codified.** Phase 5 debrief said "document the sequential-dispatch heuristic." Didn't happen between Phase 5 and Phase 6; orchestrator rediscovered the heuristic. Worth documenting as a `/planner:implement` convention.

## Lessons Learned

- **The intent-blind quality-scanner pass-after-each-task pattern works.** Six scans across 6.1–6.6 surfaced 14 Minor findings + 7 Questions. Critical/Major: zero. Most findings (12 of 14 Minor) were addressed in the next task as part of the natural editing flow; two (truncate_signature consolidation, sibling-plugin dead captures) became tracked follow-ups. Per-task scanner cost: ~3-5 minutes; benefit: the shipped state has no known correctness or maintainability defects, and the developer who looks at this code in 18 months will not find a `// TODO: clean up later` comment.
- **The "fold prior task's findings into next task's brief" pattern compounds.** Each implementer brief carried the prior task's quality-scanner findings as explicit "address these" items. By the end of the phase, the cleanup was free — no separate cleanup commit needed beyond Task 6.7's two trailing items. Phase 5 used this informally; making it explicit in the orchestrator's task brief made the implementer agents more reliable.
- **Strict assertions catch mistakes that loose assertions hide.** The 6.6 scanner caught `>= 1` where the comment said "exactly two." That's not a real bug today (the test passes), but a future double-index regression would pass silently. Cheap to fix; high value as forward defense. **Pattern: after writing a `>= N` or `!is_empty()` assertion, ask whether the actual intent is `== N`.** If yes, write `== N`.
- **Real-world dogfood validation is high-leverage but the regression gate is honor-system without CI wiring.** logrus@v1.9.3 = 407 symbols is a great regression baseline IF anyone runs the test. `#[ignore]`-gating + `--include-ignored` discipline is the only mechanism today. Phase 7's `requests@v2.32.3` will inherit the same shape. Worth a 30-minute CI lane configured with network access just to lock the baseline checks.
- **Cross-language collision regression is the load-bearing test of polyglot graph correctness.** It's the test that catches the kind of bug that would silently pollute every multi-language user's graph: collision in the `(Language, name)` index. The asymmetric assertion (X is in A's callers AND X is not in B's callers) is the structural form that makes such tests load-bearing rather than tautological. Phase 7 should add a 3-way collision test (`init` in C++, Go, Python) following the same pattern.
- **Generic receivers were a "bonus" implementation that paid back immediately** in documentation simplicity. Without it, CLAUDE.md would carry an awkward limitation note "generic receivers fall through to the empty fallback"; with it, generic receivers are a supported pattern. Cost: 30 lines + 4 tests. Worth it for the doc clarity alone.
- **Phase 5 → Phase 6 brief refresh paid off (again).** The 2026-04-30 plan-reviewer pass that pre-applied Phase 5 lessons to the Phase 6 brief meant zero cross-cutting fixes during implementation: object-safety test, strict version pin, default trait methods, watch-mode regression, Box+context registration — all already correct in the 6.x phase doc. Phase 7's brief should get a similar refresh after Phase 6 ships, before the implementer dispatch.
- **`truncate_signature` is now textbook over-due for consolidation.** Phase 5 said "Phase 6 is the moment." Phase 6 said "stay in scope, defer." Phase 7 will create a fourth copy. The pattern is: tactical scope-discipline beats strategic refactor-when-it's-time discipline. Mitigation has to be a forcing function — a dedicated task in Phase 7 that lands the consolidation BEFORE Phase 7's plugin code is written.
- **The "register parser in parse-test during corpus task, register in MCP server during integration task" split is solid.** Phase 5 established it; Phase 6 followed it; the rationale is that the corpus task needs the parser registered to validate parse output, while the MCP server registration is a separate gate that depends on the corpus passing first. Phase 7 should follow the same split.

## Impact on Subsequent Phases

- **Phase 7 (Python) inherits a 3-language working binary, 4-language registry shape.** All plumbing is proven; Python is the last plugin to wire. Same brief structure as Phase 6 should apply with grammar-specific deltas.
- **`truncate_signature` consolidation should land BEFORE Phase 7's parser body** — see Skill Opportunity #1 below. Three copies → four copies is the breaking point. Pre-Phase-7 simplify task is the right shape.
- **Sibling-plugin `@call.*` / `@import.*` dead capture cleanup should land before or during Phase 7.** The Go plugin removed these; the C++ and Rust plugins still carry them. Phase 7 will create a fourth plugin that has to decide whether to inherit the dead pattern (matching siblings) or skip it (matching Go). Ambiguity gets resolved by either consolidating now or by Phase 7 explicitly choosing.
- **Sequential dispatch for plugin-extraction phases is now confirmed convention.** Phase 7 wave 2 (definitions + calls + imports + decorators/inheritance — 4 tasks) all extend `parse_file`. Sequential dispatch will avoid merge conflicts. Document this in `/planner:implement` skill instructions to prevent rediscovery.
- **Cross-language collision test pattern extends to 3-way for Phase 7.** Add Python `def init(): pass` to the C++ + Go fixture; assert each of three `init` symbols stays isolated under `get_callers`. The same `(Language, name)`-keyed `SymbolIndex` invariant holds; the test grows from 2-way to 3-way along the same lines.
- **`testdata/mixed/` is now `foo.cpp` + `foo.rs` + `foo.go`.** Phase 7 will add `foo.py`. The `helper` anchor pattern works for any number of languages.
- **Wire-format snapshots: 30 in `snapshot_responses.rs` + 15 in `snapshot_tools_list.rs` after Phase 6.** Phase 7 will add 4 more (per the Phase 6 pattern: search by language, file_symbols, class_hierarchy, plus one extra). Total trajectory: ~50 snapshot fixtures by phase end.
- **`SharedDaemon` plan (deferred draft in `Designs/SharedDaemon/`) inherits a 3-language working binary now**, will inherit a 4-language one after Phase 7. The wire-format snapshot suite is a known constraint that will need to rebaseline for SharedDaemon's two new tools and a new `cache_status` field on `analyze_codebase`.
- **`logrus-baseline.txt` is the second `*-baseline.txt` regression-gate fixture in the repo** (the first was Phase 1's testdata_cpp). Phase 7's `requests@v2.32.3` will create a third. The baseline-file pattern is now established; consider promoting it to a shared test helper (Skill Opportunity below).
- **No design changes propagate to `Designs/RustRewrite/README.md`.** The design doc's status (`review`) is unchanged; Phase 6 implementation matched the design without forcing revisions.

## Skill Opportunities

### 1. `/simplify` (or dedicated task) for `truncate_signature` consolidation BEFORE Phase 7

- **What I did repeatedly**: Three byte-identical copies of `truncate_signature` now exist in `codegraph-lang-cpp/src/helpers.rs`, `codegraph-lang-rust/src/helpers.rs`, and `codegraph-lang-go/src/helpers.rs`. Each copy has a near-byte-identical unit-test suite (8/4/8 tests respectively). Phase 5 explicitly noted Phase 6 was the consolidation moment; both Phase 5 and Phase 6 deferred it as scope creep.
- **Where it belongs**: A dedicated pre-Phase-7 task (1 hour) that extracts the helper to a new module — most likely `codegraph-lang::helpers::truncate_signature` (since `codegraph-lang` is already a dep of every plugin), with all 8 edge-case tests promoted to the shared module.
- **Why a skill**: The pattern of "one phase says next phase will fix; next phase defers" is the textbook way refactors silently die. Phase 7 will create a fourth copy; the consolidation cost grows linearly. A dedicated task with a hard "land before Phase 7's parser body" deadline forces the action.
- **Rough shape**: New task `RustRewrite/Active/Phase7-Prep`: extract `truncate_signature` to `crates/codegraph-lang/src/helpers.rs`, port all unit tests (highest-coverage set, currently C++ or Go), update three plugin crates to import the shared helper, delete duplicate copies. Verify with `cargo test --workspace`.

### 2. `make new-plugin LANG=...` Makefile target (carry-over from Phase 5 debrief; still not done)

- **What I did repeatedly**: Same scaffold as Phase 5 — workspace dep + Cargo.toml + lib.rs skeleton + queries.rs + helpers.rs + object-safety test. Phase 5 wrote one (Rust); Phase 6 wrote one (Go); Phase 7 will write one (Python). Phase 5 debrief flagged this as a Skill Opportunity; nothing happened. Now confirmed by Phase 6's identical work pattern.
- **Where it belongs**: `Makefile` at repo root, or `scripts/new-plugin.sh`.
- **Why a skill**: Phase 6 work was 90% identical to Phase 5 in structure (different grammar). Phase 7 will be the same. The implementer agent had to read 4 sibling files to assemble the scaffold; a generator would emit it.
- **Rough shape**: `make new-plugin LANG=python` produces:
  - `crates/codegraph-lang-python/Cargo.toml` with stubbed workspace deps
  - `crates/codegraph-lang-python/src/lib.rs` with `PythonParser` skeleton, cached queries, `LanguagePlugin` impl, `#[test] fn python_parser_is_object_safe_via_box_dyn` test
  - `crates/codegraph-lang-python/src/queries.rs` with empty `DEFINITION_QUERIES` / `CALL_QUERIES` / `IMPORT_QUERIES` constants
  - `crates/codegraph-lang-python/src/helpers.rs` with `extract_*` stubs
  - `Cargo.toml` workspace `[workspace.dependencies]` updated with `tree-sitter-python` placeholder
  - **Compile gate**: `cargo build -p codegraph-lang-python` succeeds immediately with empty queries.

### 3. Document the sequential-dispatch heuristic in `/planner:implement` (carry-over from Phase 5 debrief; still not done)

- **What I did repeatedly**: Phase 5 wave 2 (5.2/5.3/5.4) and Phase 6 wave 2 (6.2/6.3/6.4) both required sequential dispatch because all tasks extended `parse_file`. Phase 5 debrief flagged this as a documentation opportunity. Phase 6 rediscovered the heuristic during planning ("Wave 2 — sequential dispatch per Phase 5 lesson"). Phase 7 will need it too.
- **Where it belongs**: `/planner:implement` skill instructions (or in `shared/orchestration.md`).
- **Why a skill**: Three phases of plugin work all hit the same "parallel dispatch would conflict" wall. Heuristic is currently in human-knowledge-only form.
- **Rough shape**: Add a paragraph to the `/implement` flow's "Build Dependency Graph & Execute in Waves" section: "Heuristic: if multiple tasks in a wave modify the same file (e.g., all extending the same `extract_X`-style entry function), dispatch sequentially. Parallel dispatch with worktree isolation is also valid but requires merging at wave end. Sequential is faster end-to-end for ≤3 tasks."

### 4. `make audit` + `make check` aggregate Makefile targets (carry-over from Phase 4/5 debrief; still not done)

- **What I did repeatedly**: Every implementer agent ran `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo audit`, in that order. Phase 4/5 debriefs flagged this. Phase 6 ran it ~7 times across 7 tasks.
- **Where it belongs**: `Makefile`.
- **Why a skill**: Aggregating the canonical pre-commit gate into `make check` replaces 4 commands with one and documents the gate.
- **Rough shape**:
  ```makefile
  check: fmt-check lint test audit
  audit:
      command -v cargo-audit >/dev/null || cargo install --locked cargo-audit
      cargo audit
  ```

### 5. Real-world-dogfood baseline-fixture pattern as a shared helper

- **What I did**: `testdata/cpp/` (Phase 1), `testdata/go/logrus-baseline.txt` (Phase 6), and the eventual `testdata/python/requests-baseline.txt` (Phase 7) all follow the same pattern: clone an external fixture at a pinned tag, run parse-test, record the symbol count, gate the regression at ±10% with `#[ignore]` + `eprintln!+return` on missing fixture.
- **Where it belongs**: Either a shared test helper in `crates/codegraph-tools/tests/common/mod.rs` (e.g., `fn assert_dogfood_baseline_within_ten_percent(fixture_path: &Path, baseline_path: &Path, parser: impl LanguagePlugin)`) OR a Makefile recipe `make dogfood-LANG=go` that clones, parses, and compares. The Makefile shape is more honest about the fact that this is a one-off CI gate.
- **Why a skill**: The pattern is now used twice (Phase 1 C++ baseline, Phase 6 Go baseline) and Phase 7 will use it a third time. The boilerplate (panic-on-missing-baseline-file but eprintln-on-missing-fixture; ±10% calc; `#[ignore]` discipline) is identical and easy to get wrong.
- **Rough shape**: `tests/common/dogfood.rs::dogfood_baseline_assertion(fixture_dir: &Path, baseline_file: &Path, expected_range: RangeInclusive<usize>) -> ()`. Returns silently if `fixture_dir` is missing (eprintln! + early return); panics if `baseline_file` is missing (real bug); panics if the actual symbol count is outside ±10% of `baseline_file`'s recorded value.

### 6. Sibling-plugin dead-capture sweep before Phase 7

- **What I did**: 6.3/6.4 quality-scanner caught `@call.expr` / `@import.spec` outer captures that were never read by `extract_calls` / `extract_imports`. The Go plugin removed them; the C++ and Rust plugins still carry the dead captures. The brief explicitly scoped the cleanup to Go.
- **Where it belongs**: A 30-minute pre-Phase-7 task that removes the dead captures from `codegraph-lang-cpp/src/queries.rs` and `codegraph-lang-rust/src/queries.rs`, updates the queries' doc-comments, and confirms tests still pass. Combined with the `truncate_signature` consolidation, this is a natural pre-Phase-7 simplify task.
- **Why a skill**: Without it, Phase 7's Python plugin will face the same decision (inherit the dead captures or skip them). Choosing now eliminates ambiguity and keeps all plugins consistent.
- **Rough shape**: Same as Skill #1 — a dedicated `RustRewrite/Active/Phase7-Prep` simplify task.

### 7. Pre-Phase plan-reviewer refresh as a documented cadence (carry-over from Phase 5 debrief; still partially done)

- **What I did**: Phase 5 debrief recommended a plan-reviewer refresh against shipped state before each plugin phase. Phase 6 inherited a refreshed brief from the 2026-04-30 reviewer pass and shipped with zero cross-cutting fixes. Phase 7's brief should get the same treatment but no formal step exists.
- **Where it belongs**: `/planner:implement` skill instructions OR a dedicated `/planner:refresh-brief` slash command.
- **Why a skill**: Phase 5 → Phase 6 brief refresh saved at least 5 fixup commits. Phase 6 → Phase 7 should benefit from the same. The current state is "human remembers to do it"; a slash command would make it discoverable.
- **Rough shape**: `/planner:refresh-brief <plan>/<phase>` reads the phase doc + the prior phase's debrief + the related design doc, and runs `/planner:plan-reviewer` against the phase doc with the prior debrief as additional context. Outputs a Revise-or-Approve verdict and a list of cross-cutting fixes.
