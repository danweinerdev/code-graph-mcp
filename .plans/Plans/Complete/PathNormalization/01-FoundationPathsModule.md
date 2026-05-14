---
title: "paths module, dunce dependency, analyze.rs swap"
type: phase
plan: PathNormalization
phase: 1
status: complete
created: 2026-05-13
updated: 2026-05-13
deliverable: "code-graph-core::paths module (canonicalize, simplify, normalize_user_path) compiled and unit-tested; dunce added as a workspace dep; analyze_codebase uses paths::canonicalize so root_path comes back without \\\\?\\ on Windows."
tasks:
  - id: "1.1"
    title: "Add dunce as a workspace dependency"
    status: complete
    verification: "`dunce = \"1\"` appears in the workspace Cargo.toml [workspace.dependencies] table; `code-graph-core/Cargo.toml` declares `dunce.workspace = true` (unconditional, NOT cfg(windows)-gated, consistent with other cross-platform shims); `cargo tree -p code-graph-core | rg dunce` shows the dep is reachable; `cargo build --workspace` succeeds on Linux."
  - id: "1.2"
    title: "Implement paths module (canonicalize, simplify, normalize_user_path)"
    status: complete
    verification: "`crates/code-graph-core/src/paths.rs` exports three pub fns: `canonicalize(p: &Path) -> io::Result<PathBuf>` wraps `dunce::canonicalize`, `simplify(p: &Path) -> PathBuf` wraps `dunce::simplified(p).to_path_buf()`, `normalize_user_path(p: &str) -> PathBuf` tries `dunce::canonicalize` and falls back to `dunce::simplified` on ANY error (not just NotFound). Module is re-exported via `crates/code-graph-core/src/lib.rs`. Unit tests cover: (a) `canonicalize` on an existing tempdir resolves to an absolute path without `\\\\?\\` on Linux; (b) `simplify` on an already-clean Linux path is a no-op; (c) `normalize_user_path` on an existing tempdir returns the canonical form; (d) `normalize_user_path` on a non-existent path falls back without panicking and the returned PathBuf round-trips through `to_string_lossy`; (e) `normalize_user_path` on a path containing `.` and `..` segments resolves them when the underlying path exists. All tests pass on Linux."
    depends_on: ["1.1"]
  - id: "1.3"
    title: "Windows-gated unit test for `\\\\?\\` strip behavior"
    status: complete
    verification: "Inside `crates/code-graph-core/src/paths.rs`, a `#[cfg(windows)]`-gated unit test constructs a `PathBuf` from the literal string `\\\\?\\D:\\proj\\file.h` (no filesystem call, no real D: drive needed for the lexical-only assertion path), calls `simplify`, and asserts the result equals `PathBuf::from(\"D:\\\\proj\\\\file.h\")`. A second `#[cfg(windows)]` test asserts `\\\\?\\UNC\\server\\share\\file.h` is rewritten to `\\\\server\\share\\file.h`. These tests do not run on Linux CI; their existence is the load-bearing regression check that the strip logic is wired correctly when developers run on Windows."
    depends_on: ["1.2"]
  - id: "1.4"
    title: "Swap std::fs::canonicalize -> paths::canonicalize in analyze.rs"
    status: complete
    verification: "`crates/code-graph-tools/src/handlers/analyze.rs:61` now calls `code_graph_core::paths::canonicalize(&path_raw)` (or equivalent re-export path) instead of `std::fs::canonicalize`. The error wording on the `Err(_)` branch is BYTE-IDENTICAL to today (`\"directory does not exist: {path_raw}\"`) — Phase 3.7 snapshot of the Rust-specific wording stays green. The `abs_path.is_dir()` check below remains. On a Linux tempdir analyze run, `root_path` in the response is a clean absolute path with no `\\\\?\\` (Linux trivially; the meaningful change is on Windows where `paths::canonicalize` returns short form whenever possible)."
    depends_on: ["1.2"]
  - id: "1.5"
    title: "Structural verification"
    status: complete
    verification: "`cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all --check` clean; `cargo test --workspace` green on Linux (the Windows-gated test in 1.3 is silently skipped); existing 49 corpus tests + fmt/ripgrep/logrus/requests/efcore/commons-lang baselines all within ±10%."
    depends_on: ["1.4"]
---

# Phase 1: paths module, dunce dependency, analyze.rs swap

## Overview

Foundation phase. Establishes the `paths` module everything else consumes. No query handler is touched; no cache migration code is touched. The only behavior change at end-of-phase is that `analyze_codebase` returns `root_path` without `\\?\` on Windows. Linux/macOS behavior is unchanged byte-for-byte.

This phase is the only one that touches `code-graph-core`. Phases 2 and 3 stay in `code-graph-graph` and `code-graph-tools` respectively.

## 1.1: Add dunce as a workspace dependency

### Subtasks
- [x] Edit workspace `Cargo.toml`, add `dunce = "1"` to `[workspace.dependencies]` (place alphabetically among existing entries)
- [x] Edit `crates/code-graph-core/Cargo.toml`: add `dunce.workspace = true` under `[dependencies]`
- [x] Run `cargo build --workspace` — confirm zero new warnings
- [x] Run `cargo tree -p code-graph-core | rg dunce` — confirm `dunce v1.x.y` shows (`dunce v1.0.5`)
- [x] Confirm the dep is unconditional (NOT `[target.'cfg(windows)'.dependencies]`), matching workspace precedent for cross-platform shims

### Notes
`dunce` is ~60 lines, last meaningful change 2022, no open issues affecting our use. On Linux/macOS it compiles to identity wrappers — zero behavioral change there.

`dunce` is NOT currently in the workspace `Cargo.lock` as a transitive dep; this PR adds it as a new leaf dependency. The `cargo tree -p code-graph-core | rg dunce` step above is the verification that the add wired correctly.

## 1.2: Implement paths module (canonicalize, simplify, normalize_user_path)

### Subtasks
- [x] Create `crates/code-graph-core/src/paths.rs` with three pub fns
- [x] `canonicalize(p: &Path) -> io::Result<PathBuf>` — wraps `dunce::canonicalize`. Doc comment: "Filesystem-bound canonicalization. On Windows, returns the short `D:\\…` form whenever possible; the extended-path `\\\\?\\` prefix survives only when the short form is invalid (e.g. path > 260 chars, special device names)."
- [x] `simplify(p: &Path) -> PathBuf` — wraps `dunce::simplified(p).to_path_buf()`. Doc comment: "Infallible lexical strip. Use on already-canonical paths (e.g. cache deserialization migration). Identity on non-Windows."
- [x] `normalize_user_path(p: &str) -> PathBuf` — tries `canonicalize`, falls back to `simplify` on ANY error kind. Doc comment: "Normalize a user-supplied file-path argument before graph lookup. The fallback covers the stale-graph case (file deleted since indexing) and any other canonicalize failure (permission, broken symlink, malformed input). The worst outcome of fallback is a graph miss, never a panic."
- [x] Add `pub mod paths;` to `crates/code-graph-core/src/lib.rs` and re-export the three fns (or use `pub mod paths;` directly — match the existing module-export pattern in this crate)
- [x] Write 5 unit tests per the verification field. Use `tempfile::TempDir` for the existing-path tests; use a clearly non-existent path for the fallback test
- [x] Verify on Linux: all 5 tests pass, none of the returned PathBuf strings contain `\\?\`

### Notes
The fallback to `simplify` on any error (not just `NotFound`) is intentional per design Decision 3 rationale. The alternative — distinguishing error kinds — adds branching for no win because every `canonicalize` failure has the same correct outcome at this layer: try the lexical form and let the eventual graph lookup return "not found" cleanly.

## 1.3: Windows-gated unit test for `\\?\` strip behavior

### Subtasks
- [x] Add `#[cfg(windows)]` unit test `simplify_strips_extended_disk_prefix` in `paths.rs`
- [x] Test body: `let input = PathBuf::from(r"\\?\D:\proj\file.h"); let out = simplify(&input); assert_eq!(out, PathBuf::from(r"D:\proj\file.h"));`
- [x] Add second `#[cfg(windows)]` test — **renamed to `simplify_leaves_verbatim_unc_unchanged` after code review revealed `dunce::simplified` does NOT strip `VerbatimUNC` paths; only `VerbatimDisk` is stripped.** Test asserts identity (input unchanged) instead of the rewrite the design originally specified
- [x] Test body: `let input = PathBuf::from(r"\\?\UNC\server\share\file.h"); let out = simplify(&input); assert_eq!(out, input);`
- [x] These tests run only on Windows targets — confirmed via `cargo test --workspace` on Linux NOT compiling them (the `#[cfg(windows)]` attribute conditionally removes them at compile time)
- [x] Document in the test module doc comment that these are the load-bearing strip-correctness regression tests and that Linux CI cannot exercise them

### Notes
Per the design's "CI coverage gap" disclosure: these tests are the *only* automated guarantee that the strip logic works on Windows. They cost nothing in CI time on Linux (they don't compile) and provide real coverage when a developer runs `cargo test` on Windows. Phase 4 task 4.3 documents the gap in the PR description for reviewers.

## 1.4: Swap std::fs::canonicalize -> paths::canonicalize in analyze.rs

### Subtasks
- [x] Edit `crates/code-graph-tools/src/handlers/analyze.rs:61` — change the call from `std::fs::canonicalize(&path_raw)` to `code_graph_core::paths::canonicalize(&path_raw)` (use the crate's existing import idiom; if `code_graph_core::paths` is the imported form, use that; otherwise the re-exported `canonicalize` directly)
- [x] Verify the surrounding code is unchanged: the `match` arms, the `abs_path.is_dir()` check at :67, and the error wording at :64 (`format!("directory does not exist: {path_raw}")`) all stay byte-identical
- [x] Run the existing `handlers/analyze.rs` snapshot tests (Phase 3.7 of RustRewrite): `cargo test -p code-graph-tools handlers::analyze` — confirm no `*.snap.new` produced
- [x] On Linux, run a single `analyze_codebase` against a tempdir; manually inspect the response to confirm `root_path` is a clean absolute path
- [x] Also updated stale comment at `analyze.rs:70` (`Rust's std::fs::canonicalize` → `Rust's paths::canonicalize`) per code review

### Notes
This is the one production-behavior change in Phase 1. Per design Decision 4, the error wording stays unchanged because the user's input is what they expect to see; the fix is on the success path, not the error path.

## 1.5: Structural verification

### Subtasks
- [x] Run `cargo clippy --workspace --all-targets -- -D warnings` (clean)
- [x] Run `cargo fmt --all --check` (clean)
- [x] Run `cargo test --workspace` — confirm green on Linux; the `#[cfg(windows)]` tests from 1.3 are silently absent from the test count (62 tests in code-graph-core including 5 new `paths::tests::*`; all other crates green; total >900 tests pass)
- [x] Run `make snapshot-clean` — confirm no `*.snap.new` files (✓ No pending snapshots)
- [ ] (If submodules initialized) Run `cargo test -p code-graph-lang-rust ripgrep` and the other dogfood baselines — confirm symbol counts within ±10% of recorded baselines (path normalization should be a complete no-op on these Linux-rooted submodules) — deferred; submodules not initialized in this environment, and Phase 1 is a no-op on Linux for parser output anyway

### Notes
Structural verification is its own task here because Phase 1 is the foundation; downstream phases consume the paths module and lean on its correctness. Catching a clippy or test regression here saves churn in Phase 2/3.

## Acceptance Criteria
- [ ] `code-graph-core::paths` exports `canonicalize`, `simplify`, `normalize_user_path`
- [ ] 5 platform-independent unit tests pass on Linux
- [ ] 2 `#[cfg(windows)]`-gated unit tests exist (untested on CI; documented as Windows-developer-runs-locally coverage)
- [ ] `analyze_codebase` uses `paths::canonicalize`; existing snapshot tests stay green
- [ ] `dunce = "1"` declared as unconditional workspace dep
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
- [ ] `make snapshot-clean` passes
- [ ] All existing baselines stay within ±10% (no behavioral change expected on Linux)
