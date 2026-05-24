---
title: "Documentation, tool-description sweep, CI honesty"
type: phase
plan: PathNormalization
phase: 4
status: complete
created: 2026-05-13
updated: 2026-05-13
deliverable: "CLAUDE.md captures the new path-normalization invariant; Cache-invalidation section notes the migration-on-load behavior; CLAUDE.md known-limitations records the deferred watch-event re-contamination follow-up; PR description template includes the CI-coverage gap disclosure for reviewers."
tasks:
  - id: "4.1"
    title: "CLAUDE.md 'Core invariants' bullet on path normalization"
    status: complete
    verification: "`CLAUDE.md` 'Core invariants' section gains a bullet that names the contract: `\"All stored file paths are absolute and `\\\\?\\`-prefix-stripped via `dunce` at index time; incoming file-path args on `get_file_symbols`, `get_coupling`, `get_dependencies`, and `generate_diagram(file=…)` are routed through `code_graph_core::paths::normalize_user_path` before lookup.\"` The existing 'Paths: all stored file paths are absolute.' bullet is updated to this expanded form (not duplicated). A `grep -c 'normalize_user_path' CLAUDE.md` shows ≥1 hit. The bullet uses the exact function name developers will see in the codebase."
  - id: "4.2"
    title: "CLAUDE.md 'Cache invalidation' note on auto-migration"
    status: complete
    verification: "`CLAUDE.md` 'Cache invalidation' section gains a one-line note: 'JSON caches containing `\\\\?\\`-prefixed paths (from a pre-PathNormalization index) auto-migrate during `Graph::load` via `paths::simplify`. No `force=true` is required to apply the migration — it runs unconditionally on every load and is a no-op on already-clean caches.' Placement: as a new sub-bullet under the existing list of what triggers vs. does not trigger force-reindex. The line distinguishes path-string migration (auto) from semantic re-parsing (still requires `force=true`)."
    depends_on: ["4.1"]
  - id: "4.3"
    title: "PR description template captures the CI-coverage gap"
    status: complete
    verification: "The PR description for the bundle (or each of the constituent PRs if shipped separately) includes a 'CI-coverage caveat' section reading roughly: 'This fix targets Windows-only behavior (`\\\\?\\` extended-path prefix). The existing CI matrix is Linux-only; `dunce::simplified` is documented to be a no-op on non-Windows targets, so the Linux test suite cannot exercise the strip logic. The load-bearing automated check is the `#[cfg(windows)]`-gated unit test in `crates/code-graph-core/src/paths.rs`, which runs only when a developer or contributor invokes `cargo test` on a Windows host. Manual smoke against a UE-style fixture on Windows before release is the supplementary verification. A Windows CI matrix entry is a tracked follow-up.' The disclosure is verbatim in the PR body so reviewers and future archaeologists see the gap explicitly."
    depends_on: ["4.2"]
  - id: "4.4"
    title: "CLAUDE.md 'Known limitations' breadcrumb for watch-event re-contamination"
    status: complete
    verification: "`CLAUDE.md` (likely under a 'Known limitations' or 'Watch mode' subsection — pick the closest existing context; create one if neither exists) gains a one-line note: 'Watch-mode reindex via `notify-debouncer-full` may receive `\\\\?\\`-prefixed event paths on Windows, re-contaminating a clean post-PathNormalization graph. Filed as a deferred follow-up plan; see `Designs/PathNormalization/README.md` Non-Goals.' This is a breadcrumb, not a workaround documentation. Goal: a future Windows-watch-mode user can find the known issue by grepping CLAUDE.md."
    depends_on: ["4.3"]
  - id: "4.5"
    title: "Final structural verification across the full plan"
    status: complete
    verification: "After Phases 1–4 are merged: `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all --check` clean; `cargo test --workspace` green on Linux; `make snapshot-clean` passes; `make snapshot-audit` if run shows only the deliberate Phase 3 tools-list regenerations; pre-existing 49 corpus tests + fmt/ripgrep/logrus/requests/efcore/commons-lang baselines all within ±10%. The 5 anti-regression tests from Phases 1–3 (Phase 1's `#[cfg(windows)]` strip tests; Phase 2's `cache_migration_strips_all_path_locations` and `cache_migration_preserves_cross_field_consistency`; Phase 3's `tests/path_normalization.rs` integration test) all pass on Linux, with the two Windows-gated ones skipped invisibly."
    depends_on: ["4.4"]
tags: [paths, windows, cross-platform, mcp, ue, unreal-engine, ergonomics]
---

# Phase 4: Documentation, tool-description sweep, CI honesty

## Overview

Documentation phase. Captures the new invariant in CLAUDE.md, records the cache-migration behavior, files the watch-event re-contamination breadcrumb for the deferred follow-up plan, and threads the CI-coverage gap into PR descriptions so reviewers see the honest picture.

No code changes in this phase. All work is `.md` edits plus PR-description text.

## 4.1: CLAUDE.md 'Core invariants' bullet on path normalization

### Subtasks
- [ ] Open `CLAUDE.md`, locate the 'Core invariants' section
- [ ] Find the existing bullet `**Paths:** all stored file paths are absolute.`
- [ ] Update to: `**Paths:** all stored file paths are absolute and `\\?\`-prefix-stripped via `dunce` at index time; incoming file-path args on `get_file_symbols`, `get_coupling`, `get_dependencies`, and `generate_diagram(file=…)` are normalized through `code_graph_core::paths::normalize_user_path` before lookup.`
- [ ] Verify the existing bullet's neighbors (Tool handler return type, State guard, etc.) remain in place; only the Paths bullet is modified
- [ ] `grep -c 'normalize_user_path' CLAUDE.md` returns ≥1

### Notes
This is the primary documentation surface agents read first when learning the codebase. The bullet must name the exact function (`paths::normalize_user_path`) so a future agent grep-searching for the helper finds it immediately.

## 4.2: CLAUDE.md 'Cache invalidation' note on auto-migration

### Subtasks
- [ ] Open `CLAUDE.md`, locate the 'Cache invalidation' section
- [ ] Add a new sub-bullet (or paragraph if the section is prose-formatted) noting: `JSON caches containing `\\?\`-prefixed paths (from a pre-PathNormalization index) auto-migrate during `Graph::load` via `paths::simplify`. No `force=true` is required to apply the migration — it runs unconditionally on every load and is a no-op on already-clean caches.`
- [ ] Place the new line near the other "what does NOT need force=true" notes (the existing distinction between mtime-driven and config-driven invalidation is the closest semantic neighbor)
- [ ] The line distinguishes path-string migration (automatic, no user action) from semantic re-parsing (still requires `force=true` for `[cpp].macro_strip` changes etc.)

### Notes
The cache-invalidation section is one of the trickier ones agents and humans both consult under stress (a stale-cache problem in production). Adding the migration note here, not in a separate "migration" section, keeps the surface area small and the answer one paragraph away from related questions.

## 4.3: PR description template captures the CI-coverage gap

### Subtasks
- [ ] When opening each PR (Phase 1, 2, 3 — or the bundled PR if shipping all together), include the disclosure verbatim
- [ ] Recommended language:
  ```
  ## CI-coverage caveat

  This fix targets Windows-only behavior (`\\?\` extended-path prefix).
  The existing CI matrix is Linux-only; `dunce::simplified` is documented
  to be a no-op on non-Windows targets, so the Linux test suite cannot
  exercise the strip logic. The load-bearing automated check is the
  `#[cfg(windows)]`-gated unit test in `crates/code-graph-core/src/paths.rs`,
  which runs only when a developer or contributor invokes `cargo test` on
  a Windows host. Manual smoke against a UE-style fixture on Windows before
  release is the supplementary verification. A Windows CI matrix entry is
  a tracked follow-up.
  ```
- [ ] Confirm the disclosure stays in the PR body (not the commit message, where it would be lost to history fragmentation)

### Notes
The CI-coverage gap is a real risk. Reviewers approving the PR need to know the test suite they see passing doesn't actually prove the fix works on Windows. Hiding the gap would invite a regression that ships in a future refactor without anyone realizing.

## 4.4: CLAUDE.md 'Known limitations' breadcrumb for watch-event re-contamination

### Subtasks
- [ ] Open `CLAUDE.md`, scan for a 'Known limitations', 'Watch mode', or equivalent section; if none exists, create one near the bottom alongside per-language limitations
- [ ] Add a one-line note: `**Watch-mode path re-contamination on Windows:** `notify-debouncer-full` may deliver `\\?\`-prefixed event paths during reindex, re-contaminating a clean post-PathNormalization graph. Filed as a deferred follow-up plan; see `Designs/PathNormalization/README.md` Non-Goals.`
- [ ] The note is intentionally a breadcrumb — it doesn't prescribe a workaround, it just makes the gap discoverable via grep

### Notes
Per Decision 8 in the plan README: the watch fix is filed as a separate plan, not bundled here. This breadcrumb is the link future readers follow when they hit the symptom.

## 4.5: Final structural verification across the full plan

### Subtasks
- [ ] After all phases merged, run: `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Run: `cargo fmt --all --check`
- [ ] Run: `cargo test --workspace` — green on Linux
- [ ] Run: `make snapshot-clean`
- [ ] Run: `make snapshot-audit ARGS="path_normalization"` to confirm the integration-test snapshots are accounted for (if any were added in 3.4)
- [ ] Run the dogfood baselines if submodules are initialized: `cargo test -p code-graph-lang-rust ripgrep`, `cargo test -p code-graph-lang-go logrus`, `cargo test -p code-graph-lang-python requests`, `cargo test -p code-graph-lang-cpp fmt`, `cargo test -p code-graph-lang-csharp efcore`, `cargo test -p code-graph-lang-java commons-lang` — confirm each within ±10% of recorded baseline (path normalization should be a complete no-op on Linux-rooted submodules)
- [ ] Open one of the new integration tests (`tests/path_normalization.rs`) and the `#[cfg(windows)]` unit tests in `paths.rs` to confirm they're discoverable in the test output (`cargo test --workspace -- --list` or similar)

### Notes
This final structural pass is the "is the plan actually complete?" checkpoint. It explicitly re-runs every gate from Phases 1–3 to catch any regression introduced by the doc-only edits in 4.1–4.4 (unlikely but cheap to verify).

## Acceptance Criteria
- [ ] CLAUDE.md 'Core invariants' bullet updated with new normalization contract
- [ ] CLAUDE.md 'Cache invalidation' section notes auto-migration on load
- [ ] CLAUDE.md known-limitations breadcrumb for watch-event re-contamination
- [ ] PR descriptions for all constituent PRs include the CI-coverage gap disclosure
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
- [ ] `cargo test --workspace` green on Linux
- [ ] `make snapshot-clean` passes
- [ ] All dogfood baselines stay within ±10%
