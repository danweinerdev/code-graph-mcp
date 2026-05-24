---
title: "Dogfood baseline (UE plugin submodule)"
type: phase
plan: UeMacroSupport
phase: 5
status: complete
created: 2026-05-13
updated: 2026-05-14
deliverable: "Scope changed from third-party submodule to in-tree synthetic fixture (user decision, see notes). New `crates/code-graph-tools/tests/fixtures/ue_synthetic/` (7 header files + `.code-graph.toml`) exercises every UE macro shape: simple/multi-arg/meta-nested UCLASS; UFUNCTION with comma-in-string, comma-in-meta, multi-line args; UPROPERTY shapes; module-level DECLARE_*_DELEGATE incl. multi-line; USTRUCT/UENUM; adversarial macro lookalikes in comments and strings; user-function-named-like-macro (UCLASS function in `namespace collision`). 4 integration tests in `tests/ue_synthetic.rs` assert exact symbol presence/absence — deterministic, always-on in CI, no submodule init required. Replaces both 5.1 (submodule pin) and 5.2 (baseline file); 5.3 (dogfood test) becomes the synthetic-fixture test; 5.4 (CLAUDE.md baseline-table entry) NOT applied since the synthetic approach belongs in test-conventions, not the dogfood-submodule table."
tasks:
  - id: "5.1"
    title: "Choose and pin a public UE-flavored submodule"
    status: complete
    verification: "A small public UE plugin or UE-style C++ project is selected, pinned to a specific tag or commit SHA, and added under `external/<name>`. Selection criteria: (a) public license permissive (MIT/Apache/Unlicense — match the workspace's existing baseline licenses); (b) uses UE reflection macros (`UCLASS`, `UFUNCTION`, etc.) extensively — confirmed by `grep -c 'UCLASS' <repo>` returning >= 50; (c) total `.cpp` + `.h` LOC is small enough to parse in <30 seconds (typically < 50k LOC); (d) the upstream tag/commit is stable (named release or long-lived branch head). Documented candidates from the design: `OpenPF2-Game/OpenPF2Core` (MIT, UE 5 plugin), small UE Marketplace open-source plugins, or a UE-flavored sample project. The choice is recorded in this task's notes section along with reason. `git submodule status external/<name>` shows the pin."
  - id: "5.2"
    title: "Record initial baseline symbol count"
    status: complete
    verification: "After `git submodule update --init external/<name>`: run the parse-test harness or an analyze call against the submodule with a `.code-graph.toml` containing the recommended UE preset (Phase 4.4); capture the resulting symbol count via `code-graph-parse-test` or by querying the indexed graph; write to `crates/code-graph-lang-cpp/tests/baselines/<name>.txt`. The baseline file format mirrors `fmt.txt` / `curl.txt` / `abseil-cpp.txt`: a `tag:` or `commit:` header naming the pinned upstream revision, a `symbols: N` line with the recorded count, plus any other fields the existing baselines record (inspect them and match). The number is the measured value from THIS plan's tooling — not an upstream-claimed figure."
    depends_on: ["5.1"]
  - id: "5.3"
    title: "Add the dogfood test with auto-skip and explicit preprocess"
    status: complete
    verification: "New test function lives in `crates/code-graph-lang-cpp/tests/corpus.rs` alongside the existing `fmt_dogfood_baseline_within_ten_percent` / `curl_dogfood_baseline_within_ten_percent` / `abseil_cpp_dogfood_baseline_within_ten_percent` functions. The new test does NOT live under `tests/baselines/` — Cargo only auto-discovers `tests/*.rs` (one level deep); `tests/baselines/` is data, not code. The existing `dogfood_within_ten_percent(repo_name, source_subpath)` helper at `corpus.rs:653` is INSUFFICIENT — it calls `parser.parse_file(f, &bytes)` directly at :681 without first running `preprocess`. Without `preprocess`, the UE preset doesn't apply and every `UCLASS(...) class AActor` produces zero symbols, making the baseline count near-zero and the test useless. A new helper `dogfood_within_ten_percent_with_preprocess(repo_name, source_subpath, cfg)` is added, parallel to the existing helper, that calls `let preprocessed = parser.preprocess(&bytes, &cfg);` (where `cfg: &RootConfig` carries the UE preset in `cpp.macro_strip_with_args`) and then `parser.parse_file(f, &preprocessed)`. The auto-skip pattern matches `corpus.rs:658-665` exactly — `eprintln!` setup hint and early `return`. The new test function (`<ue_name>_dogfood_baseline_within_ten_percent`) calls the new helper with a `RootConfig` whose `cpp.macro_strip_with_args` is the Phase 4 preset; asserts the symbol count is within ±10% of the baseline file value; on failure prints both the recorded baseline and actual count so SHA-bump-required vs real-regression is diagnosable at a glance."
    depends_on: ["5.2"]
  - id: "5.4"
    title: "Update CLAUDE.md dogfood-baseline table"
    status: complete
    verification: "`CLAUDE.md`'s `Dogfood-baseline submodules` table (currently lists ripgrep, logrus, requests, fmt, curl, abseil-cpp, efcore, commons-lang) gains a row for the new UE submodule. Columns: `Lang | Submodule | Pin | Baseline`. Pin is the tag or commit. Baseline path matches the file from 5.2. The C++ row group placement matches the existing `fmt`/`curl`/`abseil-cpp` cluster. The SHA-bump protocol paragraph immediately below the table needs NO change — it already covers all baselines."
    depends_on: ["5.3"]
  - id: "5.5"
    title: "Structural verification"
    status: complete
    verification: "`cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all --check` clean; `cargo test --workspace` green with submodule UNINITIALIZED (the new test auto-skips); `cargo test --workspace` green with submodule INITIALIZED (the new test passes within ±10% of baseline); existing 49 corpus tests + fmt/curl/abseil-cpp baselines stay within ±10% (the new pass is gated on `macro_strip_with_args` and doesn't affect these non-UE baselines); `make snapshot-clean` passes; `make submodules` works for the new entry."
    depends_on: ["5.4"]
tags: [cpp, tree-sitter, ue, unreal-engine, parser, config, macros]
---

# Phase 5: Dogfood baseline (UE plugin submodule)

## Overview

Long-term regression cover. A real UE-flavored C++ codebase is pinned as a submodule; an auto-skipping baseline test parses it through the new preset and asserts the resulting symbol count is within ±10% of a recorded baseline. Future regressions — a tree-sitter-cpp version bump, a scanner refactor, a config-validation regression — surface as a drift in this number, not as a silent loss of coverage.

Same shape as the existing `fmt`/`curl`/`abseil-cpp` C++ baselines (per CLAUDE.md). CI default is auto-skip; developers and release-time validation init the submodule to run the test.

## 5.1: Choose and pin a public UE-flavored submodule

### Subtasks
- [ ] Survey candidates. Design-suggested shortlist: `OpenPF2-Game/OpenPF2Core` (UE 5 plugin, MIT-licensed pathfinder 2e RPG framework), a UE5 ChaosVD-like open plugin, or a Unreal Marketplace open-source release
- [ ] Verify each candidate against the selection criteria: license, UCLASS density (`git clone && grep -rc 'UCLASS' --include='*.h' . | awk -F: '{s+=$2} END {print s}'` — target >= 50), total LOC, upstream stability
- [ ] Record the choice in this task's `Notes` section with a one-sentence justification
- [ ] Add the submodule: `git submodule add -b <branch> <url> external/<name>`; pin to a specific tag via `git -C external/<name> checkout <tag>` followed by `git add` of the submodule pointer
- [ ] Update `.gitmodules` to include the new entry
- [ ] Run `make submodules` (per CLAUDE.md commands) to confirm the workspace tooling handles it

### Notes
Submodule selection is genuinely an implementation-time decision; the plan can't pick it in advance because licensing and contents need fresh inspection. If no suitable candidate is found at implementation time, this phase can be deferred — Phases 1–4 still deliver the feature end-to-end. Mark the phase `deferred` in that case and file the discovery as a follow-up.

## 5.2: Record initial baseline symbol count

### Subtasks
- [ ] After `make submodules` (or `git submodule update --init external/<name>`) succeeds, create a temporary `.code-graph.toml` in the submodule root with the UE preset enabled (copy from Phase 4.4's `.code-graph.toml.example`)
- [ ] Run the parse-test harness: `cargo run -p code-graph-parse-test -- external/<name>`
- [ ] Capture the reported symbol count
- [ ] Write `crates/code-graph-lang-cpp/tests/baselines/<name>.txt` following the exact format used by `fmt.txt` (inspect first):
  ```
  source: <upstream repo URL>
  tag: <tag or commit SHA>
  commit: <commit SHA>
  symbols: <N>
  ```
- [ ] Verify the baseline file path matches the existing convention exactly — CLAUDE.md says C++ baselines live at `crates/code-graph-lang-cpp/tests/baselines/`, NOT `testdata/cpp/`

### Notes
The recorded number is whatever this tooling reports today. If a future tree-sitter-cpp version improves extraction by 15% globally, the baseline file gets bumped along with the SHA per the CLAUDE.md SHA-bump protocol — that's not a regression, it's a baseline refresh.

## 5.3: Add the dogfood test with auto-skip and explicit preprocess

### Subtasks
- [ ] Read the existing dogfood pattern in `crates/code-graph-lang-cpp/tests/corpus.rs` — the helper `dogfood_within_ten_percent(repo_name, source_subpath)` at :653 and the three callers (`fmt_dogfood_baseline_within_ten_percent` at :697, `curl_…` at :705, `abseil_cpp_…` at :713). The auto-skip + `eprintln!` pattern at :658-665 is what you mirror
- [ ] **The existing helper bypasses `preprocess`.** At :681 it calls `parser.parse_file(f, &bytes)` directly. For the UE test this is wrong — the preset must be applied first, or the recorded baseline count is near-zero (the parser drops every `UCLASS(...) class AActor` to ERROR and emits zero symbols)
- [ ] Add a new helper `dogfood_within_ten_percent_with_preprocess(repo_name: &str, source_subpath: Option<&str>, cfg: &RootConfig)` in the same file, parallel to the existing one. Body mirrors :653-693 but with: (a) `let preprocessed: Cow<[u8]> = parser.preprocess(&bytes, cfg);` between `read` and `parse_file`; (b) `parser.parse_file(f, &preprocessed)` instead of `parse_file(f, &bytes)`
- [ ] Add `<ue_name>_dogfood_baseline_within_ten_percent` test function (parallel to `fmt_…` etc.) that constructs a `RootConfig` with `cpp.macro_strip_with_args` set to the Phase 4 preset (use `RootConfig::default()` and mutate `cfg.cpp.macro_strip_with_args = vec![...]`; do NOT load from disk — the test owns the config inline), then calls the new helper
- [ ] The test file is `crates/code-graph-lang-cpp/tests/corpus.rs` — NO new file under `tests/baselines/`. Cargo's integration-test discovery only catches `tests/*.rs` one level deep; `tests/baselines/*.rs` would never run
- [ ] The submodule-absence skip path: same `eprintln!` setup hint and early `return` as the existing helper, with `make submodules` named in the hint
- [ ] On failure: assert message includes both baseline (`baseline_count`) and actual (`total_symbols`) values per the existing `corpus.rs:689-693` pattern

### Notes
The new helper is shaped intentionally as a sibling, not a refactor of the existing one. Refactoring `dogfood_within_ten_percent` to take an `Option<&RootConfig>` would touch three working tests for no benefit. A parallel `_with_preprocess` variant keeps the existing tests byte-identical and isolates the UE-specific code path.

The `RootConfig` constructed inline (no disk load) means the test doesn't depend on a `.code-graph.toml` fixture inside the submodule — the preset is baked into the test itself, which is more debuggable and avoids race conditions with submodule pinning. If a future test wants to vary the preset, it constructs a different `cfg` inline.

The `±10%` tolerance is the documented workspace convention. Tighter would produce flapping tests on minor tree-sitter behavior shifts; looser would mask real regressions. Don't change it.

## 5.4: Update CLAUDE.md dogfood-baseline table

### Subtasks
- [ ] Open `CLAUDE.md`, locate the `Dogfood-baseline submodules` table
- [ ] Add a new row in the C++ cluster (between abseil-cpp and the next-language entry, or wherever maintains alphabetical/clustered ordering)
- [ ] Fields per the existing table: Lang (`C++`), Submodule (`external/<name>` with parenthesized upstream URL), Pin (tag or commit SHA), Baseline (path to the file from 5.2)
- [ ] Verify no other table column needs widening or reflowing

### Notes
The SHA-bump protocol paragraph below the table is already generic — it doesn't list per-submodule procedures, it covers "any baseline." No update needed there.

## 5.5: Structural verification

### Subtasks
- [ ] With submodule UNINITIALIZED: `cargo test --workspace` — confirm the new test reports as skipped (look for the `eprintln!` hint in test output); zero failures
- [ ] With submodule INITIALIZED (`make submodules`): `cargo test --workspace` — confirm the new test runs and passes
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Run `cargo fmt --all --check`
- [ ] Run `make snapshot-clean`
- [ ] Verify the other C++ baselines stay byte-identical in count: `cargo test -p code-graph-lang-cpp fmt`, `... curl`, `... abseil-cpp` — the new pass doesn't affect them (their configs don't enable `macro_strip_with_args`)

### Notes
CI runs with submodules UNINITIALIZED by default (per the existing baselines' skip pattern). The new test on CI is therefore best-effort coverage; the real signal comes from developers running locally with submodules + release-time validation. The new test joins the existing baseline cohort with the same caveats.

## Acceptance Criteria
- [ ] Submodule chosen, added under `external/<name>`, pinned to a stable revision
- [ ] Baseline file at `crates/code-graph-lang-cpp/tests/baselines/<name>.txt` records the symbol count
- [ ] Auto-skipping baseline test added per existing convention
- [ ] CLAUDE.md dogfood-baseline table updated
- [ ] `cargo test --workspace` green with submodule uninitialized (test skips)
- [ ] `cargo test --workspace` green with submodule initialized (test passes within ±10%)
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
- [ ] `make snapshot-clean` passes
- [ ] Other C++ baselines (fmt, curl, abseil-cpp) stay within ±10%
