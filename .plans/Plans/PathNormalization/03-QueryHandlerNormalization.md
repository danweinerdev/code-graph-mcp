---
title: "Query-handler normalization for file-taking tools"
type: phase
plan: PathNormalization
phase: 3
status: complete
created: 2026-05-13
updated: 2026-05-13
deliverable: "Four file-taking MCP tools (`get_file_symbols`, `get_coupling`, `get_dependencies`, `generate_diagram(file=…)`) route the user-supplied `file` arg through `paths::normalize_user_path` before lookup. Users can paste paths in their natural form regardless of the form stored in the graph. Tool descriptions document the new tolerance per the 'Agent-facing tool descriptions' lens."
tasks:
  - id: "3.1"
    title: "Route get_file_symbols(file=…) through normalize_user_path"
    status: complete
    verification: "`crates/code-graph-tools/src/handlers/symbols.rs` `get_file_symbols` body wraps the incoming `file: &str` parameter with `code_graph_core::paths::normalize_user_path(file)` and uses the resulting `PathBuf` for the `graph.read().file_symbols(...)` lookup (replacing the existing `Path::new(file)` call). All existing snapshot tests for `get_file_symbols` stay byte-identical on Linux (where normalize is a near-no-op on canonical paths). One new unit test asserts that lookup against a graph built with a canonical path succeeds when the handler is called with both the canonical form and a form containing `.` / `..` segments that resolve to the canonical."
  - id: "3.2"
    title: "Route get_coupling, get_dependencies, generate_diagram(file=…) through normalize_user_path"
    status: complete
    verification: "Three call sites updated to wrap `file: &str` with `paths::normalize_user_path`: (a) `crates/code-graph-tools/src/handlers/structure.rs:320` in `get_coupling`; (b) `crates/code-graph-tools/src/handlers/query.rs:133` in `get_dependencies`; (c) `crates/code-graph-tools/src/handlers/structure.rs:422-423` in `generate_diagram` — the `if let Some(path) = file {` branch at :422 with the wrap landing on the `g.diagram_file_graph(Path::new(path), …)` call at :423. Each has a matching unit test asserting normalize behavior. Existing snapshots for these three tools stay byte-identical on Linux."
    depends_on: ["3.1"]
  - id: "3.3"
    title: "Tool-description sweep for the four file-taking tools"
    status: complete
    verification: "The `#[tool(description=...)]` strings (or the `file` parameter docs within them) in `crates/code-graph-tools/src/server.rs` for `get_file_symbols`, `get_coupling`, `get_dependencies`, `generate_diagram` each gain a one-line note: 'Path is resolved against the indexed graph; `\\\\?\\` extended-path prefix is handled automatically, and relative segments (`.` / `..`) resolve against the on-disk file.' The line is consistent across all four tools. Snapshot tests covering the tools-list response regenerate; `cargo insta review` accepts the new description strings deliberately (not blanket-accept). The CLAUDE.md 'Agent-facing tool descriptions' lens is applied: verbs in suggested actions match production behavior; no misleading hints introduced."
    depends_on: ["3.2"]
  - id: "3.4"
    title: "Integration test: 4 tools resolve short-form paths on a real analyze"
    status: complete
    verification: "New integration test in `crates/code-graph-tools/tests/path_normalization.rs`: analyze a tempdir whose fixture is engineered to produce cross-file edges (e.g. one Rust file `main.rs` that has a `mod util; use util::helper;` and a sibling `util.rs` defining `pub fn helper() {}` — produces a real `Inherits`/`Calls`/include relationship). Capture `root_path` from the analyze response — assert it does NOT contain `\\\\?\\` regardless of platform. For each of the 4 tools, construct a call using the *short-form* path (the form returned by the indexer; on Linux this is the only form, on Windows the meaningful test). For `get_file_symbols`: assert non-empty `results`. For `get_coupling` / `get_dependencies`: assert the response is a successful (non-error) tool call AND contains the expected cross-file edge to the sibling file. For `generate_diagram(file=…)`: assert non-empty `edges`. Each tool gets its own assertion block; failures name the offending tool. The test runs on all platforms; on Linux it's effectively a non-regression check; on Windows it's the load-bearing fix verification."
    depends_on: ["3.2"]
  - id: "3.5"
    title: "Structural verification"
    status: complete
    verification: "`cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all --check` clean; `cargo test --workspace` green on Linux; `make snapshot-clean` passes after deliberate snapshot regenerations from 3.3. The 4 tools-list snapshots (one per affected tool: `get_file_symbols`, `get_coupling`, `get_dependencies`, `generate_diagram`) regenerate (description strings change); no other snapshots regenerate (handler bodies behaviorally unchanged on Linux)."
    depends_on: ["3.4"]
tags: [paths, windows, cross-platform, mcp, ue, unreal-engine, ergonomics]
---

# Phase 3: Query-handler normalization for file-taking tools

## Overview

The four file-taking MCP tools (`get_file_symbols`, `get_coupling`, `get_dependencies`, `generate_diagram(file=…)`) today call `Path::new(file)` directly against the user-supplied string. On Windows where the graph stores canonical `\\?\D:\…`-prefixed keys (today) or short-form `D:\…` keys (after Phase 1), the user-supplied form has to match exactly. Phase 3 routes every incoming file-path argument through `paths::normalize_user_path` so users can paste paths in their natural form.

The handler change is a one-line wrap at each call site. The real work is verification: 5 snapshot regenerations (tool descriptions in the tools-list response) plus a focused integration test that proves the four tools accept short-form paths end-to-end.

This phase depends only on Phase 1 (the `paths` module). It is independent of Phase 2 (cache migration) and can develop in parallel.

## 3.1: Route get_file_symbols(file=…) through normalize_user_path

### Subtasks
- [ ] Open `crates/code-graph-tools/src/handlers/symbols.rs`, locate `get_file_symbols` body
- [ ] Find the existing `Path::new(file)` usage (or equivalent) before the `graph.read().file_symbols(...)` call
- [ ] Replace with: `let path = code_graph_core::paths::normalize_user_path(file);` then pass `&path` to the graph lookup
- [ ] Verify the surrounding error paths are unchanged: `file == ""` early-return stays the same; not-found responses unchanged
- [ ] Add a unit test asserting that an existing fixture with a canonical path can be looked up via a path containing `.` / `..` segments that resolve to the same canonical
- [ ] Run `cargo test -p code-graph-tools handlers::symbols` — confirm existing tests stay green

### Notes
The `normalize_user_path` helper falls back to a lexical-only strip when the path doesn't exist on disk (per Phase 1 Decision 3). This means a stale-graph case (file in graph but deleted from disk) still produces a usable lookup key — the lookup just returns "not found" cleanly if the key doesn't exist in the graph either. No new error path is introduced.

## 3.2: Route get_coupling, get_dependencies, generate_diagram(file=…) through normalize_user_path

### Subtasks
- [ ] Edit `crates/code-graph-tools/src/handlers/structure.rs:320` in `get_coupling` — same one-line wrap
- [ ] Edit `crates/code-graph-tools/src/handlers/query.rs:133` in `get_dependencies` — same one-line wrap
- [ ] Edit `crates/code-graph-tools/src/handlers/structure.rs:422` in `generate_diagram` — same one-line wrap (file-mode branch only; the `class=…` and `symbol=…` modes take symbol IDs, not file paths, and don't need this)
- [ ] Add a unit test for each tool, parallel to the one in 3.1
- [ ] Run `cargo test -p code-graph-tools` — confirm green

### Notes
Three identical call-site patches. Keep the same idiom (`let path = paths::normalize_user_path(file);`) across all three so reviewers see the pattern uniformly. The temptation is to introduce a tiny `normalize_or_err(...)` macro — resist; the helper call is already one line and the explicit form keeps the intent visible.

## 3.3: Tool-description sweep for the four file-taking tools

### Subtasks
- [ ] Open `crates/code-graph-tools/src/server.rs`
- [ ] Locate the `#[tool(description=...)]` macro invocations for `get_file_symbols`, `get_coupling`, `get_dependencies`, `generate_diagram`
- [ ] For each, add a sentence to the description string (or to the `file` parameter doc within) reading: "Path is resolved against the indexed graph; `\\?\` extended-path prefix is handled automatically, and relative segments (`.` / `..`) resolve against the on-disk file."
- [ ] Use the same wording verbatim in all four tools to keep agents pattern-matching on a single string
- [ ] Apply the CLAUDE.md 'Agent-facing tool descriptions' lens: confirm the verb in the description ('resolved') matches production behavior; the "extended-path prefix is handled automatically" claim is true regardless of platform
- [ ] Run `cargo test -p code-graph-tools` — the tools-list snapshots will produce `*.snap.new` files
- [ ] Run `cargo insta review` — deliberately review each new description, not blanket-accept
- [ ] Commit accepted snapshots

### Notes
Verified by `ls crates/code-graph-tools/tests/snapshots/`: there is one snapshot file per tool description (`snapshot_tools_list__tools_list_<tool>.snap`), not a monolithic tools-list snapshot. Exactly four files regenerate, one per affected tool.

## 3.4: Integration test: 4 tools resolve short-form paths on a real analyze

### Subtasks
- [ ] Create `crates/code-graph-tools/tests/path_normalization.rs` (new file)
- [ ] Use `tempfile::TempDir` to set up a fixture with *cross-file edges*. Recommended: two Rust files in the same crate — `src/main.rs` with `mod util; fn main() { util::helper(); }` and `src/util.rs` with `pub fn helper() {}`. This produces real `Calls` + `Includes` edges so `get_coupling` and `get_dependencies` return non-empty results
- [ ] Use the existing test-server harness (or mirror the pattern in existing `tests/*.rs`) to drive `analyze_codebase` against the tempdir
- [ ] Capture `root_path` from the response; assert it does NOT contain `\\?\` (Linux trivially passes; Windows is the load-bearing assertion)
- [ ] For each tool, call with the file path returned by the analyze response (short form on all platforms post-Phase-1):
  - `get_file_symbols("…/src/main.rs")` — assert `results.len() > 0`
  - `get_coupling("…/src/main.rs")` — assert non-error AND contains a coupling entry pointing to `util.rs`
  - `get_dependencies("…/src/main.rs")` — assert non-error AND contains an entry pointing to `util.rs`
  - `generate_diagram(file="…/src/main.rs")` — assert `edges.len() > 0`
- [ ] Each assertion block names the offending tool on failure
- [ ] Run on Linux; on Windows this test is the end-to-end proof the fix works

### Notes
The test value on Linux is non-regression: confirms the wrapping didn't break anything. The Windows value is the actual fix verification. Per the design's CI-coverage gap disclosure, this test is the strongest automated guarantee that exists; manual smoke-on-Windows before each release supplements.

## 3.5: Structural verification

### Subtasks
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Run `cargo fmt --all --check`
- [ ] Run `cargo test --workspace` — green on Linux
- [ ] Run `make snapshot-clean` — confirm zero `*.snap.new` files remain after the deliberate 3.3 accept step

### Notes
The clippy lens catches stylistic regressions in the four handler edits; the fmt lens catches whitespace; the test lens catches behavior; the snapshot-clean lens catches forgotten review-then-accept on the 3.3 regenerations. All four are non-negotiable per workspace policy.

## Acceptance Criteria
- [ ] 4 file-taking handlers route through `paths::normalize_user_path`
- [ ] Each tool's description string includes the path-form-tolerance note
- [ ] Tools-list snapshots regenerated and reviewed
- [ ] Integration test in `tests/path_normalization.rs` passes on Linux
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo fmt --all --check` clean
- [ ] `make snapshot-clean` passes
