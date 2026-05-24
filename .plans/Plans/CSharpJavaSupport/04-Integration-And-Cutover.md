---
title: "Integration and Cutover"
type: phase
plan: CSharpJavaSupport
phase: 4
status: complete
created: 2026-05-08
updated: 2026-05-11
deliverable: "Both plugins are registered in the binary and the parse-test harness; the cross-language collision regression is widened from 3-way to 5-way with asymmetric assertions for all 5×4 = 20 cross-language pairs; new wire-format snapshots are accepted; documentation reflects 6 supported languages; final structural verification passes; plan moves from Active → Complete."
tasks:
  - id: "4.1"
    title: "Register CSharpParser and JavaParser in main.rs and parse-test"
    status: complete
    verification: "`crates/code-graph-mcp/src/main.rs` has `.register(Box::new(CSharpParser::new().context(\"new csharp parser\")?)).context(\"register csharp plugin\")?` and the equivalent Java line, both following the existing four-language registration block exactly. The module-level doc comment (currently \"all four shipped language plugins — C++, Rust, Go, and Python\") updates to \"all six shipped language plugins — C++, Rust, Go, Python, C#, and Java\". `crates/code-graph-parse-test/src/main.rs` has the equivalent registration block following the explicit-match-on-error pattern used for Python (lines ~83–96 today). `cargo build --release -p code-graph-mcp` succeeds. `cargo run -p code-graph-parse-test -- testdata/csharp` produces a non-empty symbol dump. `cargo run -p code-graph-parse-test -- testdata/java` produces a non-empty symbol dump."
  - id: "4.2"
    title: "Widen mixed-language collision regression to 5-way"
    status: complete
    depends_on: ["4.1"]
    verification: "`crates/code-graph-tools/tests/mixed_language.rs::cross_language_init_callers_stay_isolated` (currently 3-way: C++/Go/Python) widens to 5-way (adds C# and Java). All five fixtures use the bare lowercase name `init` (NOT PascalCase `Init` for C#) — this is load-bearing per the design's Cross-Language Collision Regression Widening section. The renamed test `search_init_returns_all_five_languages` (was `_all_three_languages`) asserts `search_symbols(\"init\")` returns exactly 5 results. `build_init_collision_fixture()` writes 5 source files (was 3): `init_cpp.cpp`, `init_go.go`, `init_py.py`, `init_cs.cs`, `init_java.java`, each with a unique `caller_<lang>` function. `language_from_file` extends with `.cs` → \"csharp\" and `.java` → \"java\". `server_with_all_parsers()` registers all six plugins. **Asymmetric assertions** required: for every cross-language pair (5 languages × 4 other-language callers = 20 assertions), assert BOTH the positive (`caller_<lang>` IS in the get_callers result for `<lang>::init`) AND the negative (`caller_<other>` IS NOT in `<lang>::init`'s callers). The negative half is what makes the test load-bearing per the Phase 6 debrief. `cargo test -p code-graph-tools --test mixed_language` passes."
  - id: "4.3"
    title: "Wire-format snapshots accepted; snapshot-clean gate"
    status: complete
    depends_on: ["4.2"]
    verification: "Five new `.snap` files per language in `crates/code-graph-tools/tests/snapshots/` (10 total): analyze_codebase, get_file_symbols, search_symbols, get_class_hierarchy, get_dependencies — same five Phase 7 added for Python. Workflow: run `cargo test --workspace` to generate `.snap.new`, run `cargo insta review` to accept each one (visual diff), then run `make snapshot-clean` and confirm zero pending. The pre-commit hook (`scripts/hooks/pre-commit`, installed via `make install-hooks`) enforces this gate — committing while `.snap.new` files are present is refused. **Diagnostic discipline:** snapshot diffs against the existing four-language snapshots (cpp/rust/go/python) MUST be zero except for index/sort-order differences caused by adding new languages — if a cpp/rust/go/python snapshot changes outside that scope, investigate before accepting (the `make snapshot-audit ARGS=...` mechanism is the documented way to catch unintended cross-tool effects). `cargo test --workspace` passes; `make snapshot-clean` passes."
  - id: "4.4"
    title: "Documentation + final structural verification + plan close-out"
    status: complete
    depends_on: ["4.3"]
    verification: "Documentation updates: README.md's supported-languages table grows from 4 → 6 (add C# and Java rows). CLAUDE.md updates: the architecture table grows by 2 rows (add `code-graph-lang-csharp` and `code-graph-lang-java` with their responsibilities); the \"As of the Phase 7 cutover, all four languages\" sentence updates to \"all six languages — C++, Rust, Go, Python, C#, and Java\"; the `[extensions]` config block grows from 4 → 6 language defaults; the dogfood-baseline submodules table grows from 6 → 8 rows; new sections `## C# Parser Limitations` and `## Java Parser Limitations` land after the Python Parser Limitations section, mirroring the existing four sections' shape (one paragraph of grammar version validation, a Supported list, and a Known Limitations numbered list with the heuristic-resolution disclaimer plus any language-specific limitations surfaced during 2.x and 3.x — e.g., method references in Java if 3.3 documented a partial-handling case). The architecture diagram in CLAUDE.md updates to show 6 plugins. **Final structural verification across the workspace:** `cargo build --release -p code-graph-mcp` succeeds; `cargo test --workspace` passes (full suite); `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all --check` clean; `cargo audit` clean (or any new advisories acknowledged); `make snapshot-clean` clean; `make submodules` initializes all 8 submodules. **Plan close-out:** write `notes/04-Integration-And-Cutover.md` debrief covering the integration phase. Plan README.md frontmatter status flips from `active` → `complete`. `git mv .plans/Plans/CSharpJavaSupport .plans/Plans/CSharpJavaSupport`. If `dashboard: true` in `planning-config.json`, run `make dashboard` from the planning root."
tags: [language-plugin, c-sharp, java, tree-sitter, multi-language]
---

# Phase 4: Integration and Cutover

## Overview

Bring both plugins live in the binary, widen the cross-language regression to 5-way, accept the new wire-format snapshots, finalize documentation, and run the workspace-wide structural verification gate before flipping the plan status to complete. This phase is **strictly sequential** — every task touches shared integration files (main.rs, parse-test/main.rs, mixed_language.rs, CLAUDE.md, snapshots/) that don't tolerate parallel edits.

**Prerequisite:** Run `/planner:refresh-brief CSharpJavaSupport/04-Integration-And-Cutover.md` before dispatching `/planner:implement` for any task in this phase. Phase 4's brief is most exposed to drift because it depends on the actual shapes shipped by Phases 2 and 3 (file paths, exact registration code, snapshot file names).

**Carry-forward:** any quality-scanner findings from Phase 2 and Phase 3 debriefs that didn't get addressed in those phases should be carried into 4.1's brief at the top.

## 4.1: Register CSharpParser and JavaParser in main.rs and parse-test

### Subtasks

- [ ] In `crates/code-graph-mcp/src/main.rs`, add registration for `CSharpParser` and `JavaParser` after the four existing `.register()` calls. Use the same pattern (`Box::new(<Parser>::new().context("...")?)).context("register ... plugin")?`).
- [ ] Update the module-level doc comment from "all four shipped language plugins — C++, Rust, Go, and Python" to "all six shipped language plugins — C++, Rust, Go, Python, C#, and Java".
- [ ] In `crates/code-graph-parse-test/src/main.rs`, add the equivalent registration block. Note: this file uses a different pattern (explicit `match Parser::new()` and explicit `match registry.register()` with error printing) — follow the Python block at lines ~83–96 as the template.
- [ ] Run `cargo build --release -p code-graph-mcp` and confirm a binary is produced.
- [ ] Smoke-run the parse-test harness against `testdata/csharp/` and `testdata/java/` and confirm both produce non-empty symbol dumps.
- [ ] Run `cargo build --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`.

### Notes

This task is mechanically simple but touches the integration boundary — verify behavior end-to-end before declaring done.

## 4.2: Widen mixed-language collision regression to 5-way

### Subtasks

- [ ] In `crates/code-graph-tools/tests/mixed_language.rs`, edit `build_init_collision_fixture()` to write five source files (was three): add `init_cs.cs` and `init_java.java`. Each file declares an `init` symbol (lowercase — load-bearing per the design's Cross-Language Collision Regression Widening section) and a unique `caller_<lang>` function.
- [ ] Update `language_from_file` (currently maps `.cpp/.cc/.h/.go/.py/.pyi`) with `.cs` → `"csharp"` and `.java` → `"java"`.
- [ ] Update `server_with_all_parsers()` to register all six plugins.
- [ ] Rename `search_init_returns_all_three_languages` → `search_init_returns_all_five_languages` and update its assertion (3 → 5 results).
- [ ] Extend `cross_language_init_callers_stay_isolated`:
  - For each of 5 languages (`cpp`, `go`, `python`, `csharp`, `java`), assert positive: `caller_<lang>` IS in the `get_callers` result for `<lang>::init`.
  - For each of 5 × 4 = 20 cross-language pairs, assert negative: `caller_<other-lang>` IS NOT in `<lang>::init`'s callers.
  - The asymmetric (positive AND negative) shape is the load-bearing pattern — Phase 6 debrief established this; Phase 7 confirmed.
- [ ] Run `cargo test -p code-graph-tools --test mixed_language` and confirm pass.
- [ ] Run `cargo clippy -p code-graph-tools --all-targets -- -D warnings`.

### Notes

PascalCase `Init` for C# and camelCase `Init` for Java would NOT work for the load-bearing test — the `(Language, name)` index key is the literal name string, so a different casing makes the symbol a different name key entirely and the test no longer pins cross-language *name-key* isolation. The design explicitly mandates lowercase `init` for all five fixtures.

A second optional fixture using PascalCase `Init` for C# can document the casing convention separately, but it's not part of the load-bearing 5-way regression.

## 4.3: Wire-format snapshots accepted; snapshot-clean gate

### Subtasks

- [ ] Run `cargo test --workspace` to generate `.snap.new` files. Expect new snapshots for both languages across `analyze_codebase`, `get_file_symbols`, `search_symbols`, `get_class_hierarchy`, `get_dependencies` — 10 total new `.snap` files plus updates to any existing aggregate snapshots that span all languages.
- [ ] Run `cargo insta review` and visually accept each new snapshot. Reject any that look wrong (e.g., wrong language tag, missing fields, unexpected ordering).
- [ ] **Diff discipline:** snapshots for the existing four-language fixtures (cpp/rust/go/python paths) should NOT change — except for index/sort-order updates caused by adding new languages to a shared symbol list. If a non-language-list change appears in a cpp/rust/go/python snapshot, investigate before accepting (the `make snapshot-audit ARGS=...` mechanism is the documented escape hatch).
- [ ] Run `make snapshot-clean` and confirm zero `.snap.new` pending.
- [ ] Run `cargo test --workspace` once more to confirm all snapshot tests pass against the accepted state.
- [ ] Commit. The pre-commit hook (`scripts/hooks/pre-commit`, installed via `make install-hooks`) re-runs `make snapshot-clean` and refuses the commit if any pending snapshots remain.

### Notes

Forgetting `cargo insta review` after generating `.snap.new` files is the canonical way to ship a stale snapshot that fails on a clean checkout. The pre-commit hook + `make snapshot-clean` is the safety net.

## 4.4: Documentation + final structural verification + plan close-out

### Subtasks

#### Documentation

- [ ] Update README.md's supported-languages table (4 → 6 rows; add C# and Java with file extensions and a 1-line capability summary)
- [ ] Update CLAUDE.md:
  - Architecture table — add `code-graph-lang-csharp` and `code-graph-lang-java` rows with their responsibilities (mirror Python's row format)
  - Update "As of the Phase 7 cutover, all four languages" → "all six languages — C++, Rust, Go, Python, C#, and Java"
  - Architecture diagram (Mermaid in CLAUDE.md) — add C# and Java plugin nodes
  - `[extensions]` config block — built-in defaults grow from 4 → 6 entries (`csharp = [.cs]`, `java = [.java]`)
  - "Optional: dogfood-baseline submodules" table — grows from 6 → 8 rows (efcore, commons-lang)
  - **Build-section count update**: the `make submodules # init all six (shallow clones)` reference (and any other "all six" count language in CLAUDE.md) → `init all eight`. Grep CLAUDE.md for `all six` and `all four` to catch any remaining count references that need bumping
- [ ] Add `## C# Parser Limitations` section after Python Parser Limitations:
  - Validated against `tree-sitter-c-sharp v<pinned>`
  - Supported list (mirror existing four sections' shape — partial classes, default interface methods, extension methods, generic types, all import forms)
  - Known Limitations (numbered list — heuristic resolution disclaimer; any language-specific surprises surfaced during Phase 2; the partial-class search-UX caveat from Decision 3; the extension-method discoverability caveat from Decision 5)
- [ ] Add `## Java Parser Limitations` section:
  - Validated against `tree-sitter-java v<pinned>`
  - Supported list (records as Class, anonymous classes invisible, default methods as Function, enum methods, all import forms)
  - Known Limitations (heuristic resolution; Decision 4 anonymous-collision caveat; Decision 6 records/sealed simplification; method-reference handling per 3.3 reality)

#### Final structural verification (workspace-wide)

- [ ] `cargo build --release -p code-graph-mcp` succeeds and produces a binary
- [ ] `cargo test --workspace` passes the full suite
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
- [ ] `cargo audit` clean (or any new advisories acknowledged in writing — for the new tree-sitter-c-sharp / tree-sitter-java transitive deps)
- [ ] `make snapshot-clean` clean
- [ ] `make submodules` initializes all 8 submodules

#### Plan close-out

- [ ] Write `notes/04-Integration-And-Cutover.md` debrief — capture any integration-time surprises (snapshot diff anomalies, docs drift caught at the last minute, registration ordering issues).
- [ ] Flip plan README.md frontmatter `status: active` → `status: complete`. (Status flow per `shared/frontmatter-schema.md`: plan transitions `draft → approved → active → complete`. `/plan`'s approval step moves the folder `Plans/ → Plans/` and bumps the status to `approved`; `/planner:implement` moves it to `Plans/` and flips to `active` when the first phase starts. This task's job is the final flip and the final move.)
- [ ] Move the plan folder to `Plans/CSharpJavaSupport` via `git mv` from its **current location**. Determine the source path at close-out time — if `/planner:implement` moved the plan to `Plans/` (the expected path), use `git mv .plans/Plans/CSharpJavaSupport .plans/Plans/CSharpJavaSupport`. If the plan never moved out of `Plans/` or `Plans/` for some reason, fall back to whichever directory currently holds the folder. Verify with `find .plans/Plans -name CSharpJavaSupport -type d` before running the `git mv`.
- [ ] If `dashboard: true` in `planning-config.json`, run `make dashboard` from the planning root.

### Notes

The "Documentation read cold" CLAUDE.md quality lens (per CLAUDE.md's quality-scanner project-specific lenses) applies hard to the Limitations sections — read them cold before declaring done; check for framing contradictions across sibling sections, stale references, and load-bearing phrases.

## Acceptance Criteria

- [ ] Both plugins registered in `code-graph-mcp/src/main.rs` and `code-graph-parse-test/src/main.rs`
- [ ] 5-way `init` collision regression with asymmetric (positive + negative) assertions for all 20 cross-language pairs
- [ ] All new `.snap` files accepted; `make snapshot-clean` clean
- [ ] README.md, CLAUDE.md, supported-languages table, `[extensions]` table, dogfood-submodules table all updated to 6 languages / 8 submodules
- [ ] `## C# Parser Limitations` and `## Java Parser Limitations` CLAUDE.md sections written
- [ ] Workspace structural gates all pass: `cargo build --release`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --all --check`, `cargo audit`, `make snapshot-clean`, `make submodules`
- [ ] Phase 4 debrief at `notes/04-Integration-And-Cutover.md`
- [ ] Plan README.md status `active` → `complete`; folder moved from its current location (typically `Plans/`) to `Plans/CSharpJavaSupport`
- [ ] If applicable, `make dashboard` regenerated
