---
title: "Watch Mode, Cross-Compile & Go Cutover"
type: phase
plan: RustRewrite
phase: 4
status: planned
created: 2026-04-28
updated: 2026-04-28
deliverable: "Working watch_start/watch_stop with debounced reindex protected by the index lock; cargo-zigbuild producing release binaries for all 6 target platforms from one Linux host; the Go source tree removed and old planning artifacts marked superseded — the Rust binary is now the single supported implementation"
tasks:
  - id: "4.1"
    title: "Watch mode: notify-debouncer-full handle, recursive watch setup"
    status: planned
    verification: "WatchHandle holds the Debouncer<RecommendedWatcher, FileIdMap> and a tokio::sync::oneshot::Sender for shutdown; watch_start checks require_indexed first (an unindexed watch_start returns the unindexed error); recursively adds the indexed root via Watcher::watch(root, RecursiveMode::Recursive); debounce window is 250ms (constant); watch_start sets ServerInner.watch to Some(handle), sets active flag; second watch_start while watching returns 'watch mode is already active'; tests cover successful start, double-start error, and stop teardown"
  - id: "4.2"
    title: "Watch loop: debounced events + index_lock-aware reindex_file"
    status: planned
    depends_on: ["4.1"]
    verification: "watch_loop tokio task receives DebouncedEvent batches via mpsc channel; for each event, attempts ServerInner.index_lock.try_lock — if held (analyze_codebase running) the event is dropped (not queued, not retried — design Decision); when not held, takes the lock for the entire snapshot+resolve+merge sequence so no concurrent analyze can clear the graph mid-reindex; reindex_file dispatches by file extension via registry.for_path: removed events call graph.remove_file; created/modified events read+parse+resolve_edges (per-language) and merge_file_graph; non-source files (no plugin match) ignored; **reindex_file uses the cached `inner.config` (loaded by the most recent analyze_codebase) rather than re-reading `<root>/.code-graph.toml` on every event** — a unit test injects a modified config onto ServerInner before triggering a watch event and asserts the modified setting (e.g., a different parsing.max_threads) is observed; race regression test: spawn a watch task watching a directory and a parallel analyze_codebase loop; assert no panics, no graph in inconsistent state (every successful query returns a coherent snapshot); watch_stop cancels the oneshot, drops the debouncer, clears state"
  - id: "4.3"
    title: "Cross-compile via cargo-zigbuild for 6 platforms"
    status: planned
    depends_on: ["4.2"]
    verification: "Top-level Makefile or justfile recipes produce release binaries for x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl, aarch64-unknown-linux-musl, x86_64-apple-darwin, aarch64-apple-darwin, x86_64-pc-windows-gnu — all from a single Linux host using cargo-zigbuild; binaries land under bin/<target>/code-graph-mcp(.exe); each binary smoke-tests by running `<bin> --version` (or equivalent stdio handshake) and `<bin>` indexing testdata/cpp on a host that supports running it (Linux native, macOS via emulation skipped or run on a separate runner, Windows via wine if available); CI workflow file (or documented commands) demonstrates the release path; size check: each release binary under 30MB stripped"
  - id: "4.4"
    title: "Go cutover commit"
    status: planned
    depends_on: ["4.3"]
    verification: "Single commit removes: cmd/, internal/, go.mod, go.sum, original Makefile; root CLAUDE.md rewritten to describe Rust build commands (cargo build / cargo test / cargo clippy) and remove all Go references; .plans/Plans/CodeGraphMCP/ marked status: superseded with related forward-link to Plans/Active/RustRewrite (or current location after move-to-Ready); .plans/Plans/{GoParser, PythonParser, RustParser}/ marked status: superseded — their content is preserved for historical reference but they no longer drive work; .plans/Designs/CodeGraphMCP/ and Designs/LLMOptimization/ marked status: superseded with forward-link to Designs/RustRewrite; testdata/cpp preserved unchanged; commit message references this phase doc and lists every removed top-level path; post-commit: `find . -name '*.go' -not -path './.git/*'` returns no results; `cargo build --release` from a fresh clone succeeds without any Go toolchain installed"
  - id: "4.5"
    title: "Structural verification + release readiness"
    status: planned
    depends_on: ["4.4"]
    verification: "`cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test --workspace` green (all phase 1-4 tests pass); `cargo audit` (or equivalent) shows no known vulnerabilities in dependencies; release build for the host platform completes without warnings; new top-level README.md (or updated existing) describes installation via prebuilt binaries and via `cargo install --path crates/code-graph-mcp`; a Linux release artifact (tar.gz) contains the binary and a sample .code-graph.toml; manual end-to-end smoke test against an MCP client confirms watch mode works in practice (modify a file, observe reindex, query reflects the change)"
---

# Phase 4: Watch Mode, Cross-Compile & Go Cutover

## Overview

The cutover phase. After this phase ships green, the Go binary is gone — the Rust binary is the single supported implementation. Three deliverables:

1. **Watch mode**: `notify-debouncer-full` driving an index-lock-aware reindex loop that closes the race the Go implementation has (snapshot+resolve+merge under a separate lock from the analyze path).
2. **Cross-compilation**: `cargo-zigbuild` producing all 6 platform binaries from one Linux host — replacing the Go Makefile's per-platform-toolchain hunting.
3. **Cutover commit**: Go source tree removed, old plans and designs marked `superseded`, root CLAUDE.md rewritten.

This phase is intentionally small (5 tasks) so the cutover commit can ship as a single coherent diff with full test coverage.

## 4.1: Watch mode: notify-debouncer-full handle, recursive watch setup

### Subtasks
- [ ] `WatchHandle` struct holds the `Debouncer<RecommendedWatcher, FileIdMap>` and a `tokio::sync::oneshot::Sender<()>` for shutdown
- [ ] `watch_start` handler: `require_indexed()` first; check `inner.watch` is None (else return `"watch mode is already active"`); construct debouncer with 250ms timeout; `watcher.watch(root, RecursiveMode::Recursive)`; spawn the watch_loop task; populate `inner.watch = Some(handle)`
- [ ] `watch_stop` handler: take `inner.watch`; if None, return `"watch mode is not active"`; send the cancel signal; drop the debouncer; clear state
- [ ] Tests: start when not indexed → unindexed error; start happy path; double-start → already-active error; stop happy path; stop when not watching → not-active error; the exact error wording matches Go byte-for-byte

## 4.2: Watch loop: debounced events + index_lock-aware reindex_file

### Subtasks
- [ ] `watch_loop(server: Arc<ServerInner>, mut events: mpsc::Receiver<DebouncedEvent>, cancel: oneshot::Receiver<()>)` async function
- [ ] `tokio::select!` between cancel and event arrival
- [ ] On event: for each path in the event, call `server.try_reindex_file(&path, is_remove).await`
- [ ] `try_reindex_file`: attempt `inner.index_lock.try_lock` — if held, log a debug message and return (drop the event); if obtained, hold the lock for the full snapshot+resolve+merge sequence
- [ ] Inside the lock: `registry.for_path` to get plugin (None → return, not a source file); on remove, `graph.write().remove_file(path)`; on create/modify, read + parse_file + build per-Language SymbolIndex from current graph + resolve_edges (language-aware) + `graph.write().merge_file_graph(fg)`
- [ ] **Race regression test:** in `tests/watch_race.rs`, spawn a watch loop on a tmpdir + a parallel loop calling `analyze_codebase` on the same dir; modify files in the tmpdir during the test; assert no panics, no deadlocks, every concurrent query returns a coherent graph snapshot
- [ ] Test: editor-style atomic save (rename .tmp → file.cpp) produces exactly one reindex event after the debounce window
- [ ] Test: removing a watched file triggers `remove_file` and a subsequent `get_file_symbols` returns the symbols-not-found error

### Notes
The "drop the event when index_lock is held" rule (rather than queue) is a deliberate choice from the design (Concurrency Model section): the in-flight `analyze_codebase` will pick up the file's current state anyway. Queuing would create unbounded growth on a busy editor session.

## 4.3: Cross-compile via cargo-zigbuild for 6 platforms

### Subtasks
- [ ] Add cargo-zigbuild instructions to top-level documentation (Makefile/justfile/README)
- [ ] Per-platform recipes:
  - `linux-x86_64-gnu` → `cargo zigbuild --release --target x86_64-unknown-linux-gnu`
  - `linux-x86_64-musl` → `cargo zigbuild --release --target x86_64-unknown-linux-musl`
  - `linux-aarch64-musl` → `cargo zigbuild --release --target aarch64-unknown-linux-musl`
  - `darwin-x86_64` → `cargo zigbuild --release --target x86_64-apple-darwin`
  - `darwin-aarch64` → `cargo zigbuild --release --target aarch64-apple-darwin`
  - `windows-x86_64-gnu` → `cargo zigbuild --release --target x86_64-pc-windows-gnu`
- [ ] Each output goes to `bin/<target>/code-graph-mcp(.exe)`
- [ ] CI workflow file (or documented manual steps) demonstrating the release path on a Linux runner
- [ ] Smoke test each platform binary that can be run on the host: Linux native binaries run on the build host; macOS and Windows binaries are validated by `file <bin>` for arch correctness, and a separate CI runner if available
- [ ] Strip release binaries (cargo profile or `strip` post-step); verify each is under 30MB

### Notes
Cross-compilation was a major motivator for the rewrite (the Go Makefile hunts for per-platform `gcc/clang/mingw` and silently skips targets when unavailable). With cargo-zigbuild, all 6 targets build from one Linux host with no additional toolchain installation.

## 4.4: Go cutover commit

### Subtasks
- [ ] Remove `cmd/`, `internal/`, `go.mod`, `go.sum`, original `Makefile`
- [ ] Rewrite root `CLAUDE.md`:
  - Replace "Go MCP server" with "Rust MCP server"
  - Update Build section: `cargo build --release`, `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`
  - Remove `CGO_ENABLED=1` references
  - Update Architecture section to reflect the Rust crate layout
  - Keep the C++ Parser Limitations section unchanged (those are intentional and still accurate)
  - Add a "Configuration" section pointing at `<root>/.code-graph.toml`
- [ ] Mark old planning artifacts `status: superseded`:
  - `.plans/Plans/CodeGraphMCP/` README and all phase docs
  - `.plans/Plans/{GoParser, PythonParser, RustParser}/` READMEs and phase docs
  - `.plans/Designs/CodeGraphMCP/README.md`
  - `.plans/Designs/LLMOptimization/README.md`
  - Each gets a one-line note at the top: `**Superseded by [Plans/Active/RustRewrite](...)**`
- [ ] Move this plan from `Plans/Ready/RustRewrite/` (after the post-approval move) to `Plans/Active/RustRewrite/` once Phase 1 starts; this Phase-4 cutover task confirms the plan is currently in `Active/` and updates the README's `status: active` if not already set
- [ ] `testdata/cpp/` preserved unchanged
- [ ] Commit message lists every removed path and references this phase doc
- [ ] Post-commit verification: `find . -name '*.go' -not -path './.git/*' -not -path './.plans/*'` returns no results; fresh clone + `cargo build --release` succeeds without Go toolchain

## 4.5: Structural verification + release readiness

### Subtasks
- [ ] `cargo fmt --check` clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` green — all Phase 1-4 tests pass
- [ ] `cargo audit` (install if needed) reports no known vulnerabilities in pinned dependencies
- [ ] Top-level README.md updated:
  - Installation: prebuilt binaries (link to release artifacts) or `cargo install --path crates/code-graph-mcp`
  - MCP client config (claude_desktop_config.json snippet)
  - Configuration: `.code-graph.toml` schema with examples
  - Tool reference (15 tools)
  - Limitations (preserved from CLAUDE.md)
- [ ] Sample `.code-graph.toml` shipped at repo root with comments explaining each field
- [ ] Manual end-to-end smoke test:
  - Index a small C++ project
  - `watch_start`; modify a file; observe automatic reindex; `get_file_symbols` reflects the change
  - `watch_stop`
  - Restart the binary; confirm cache hit on second analyze

## Acceptance Criteria
- [ ] watch_start and watch_stop work end-to-end with debouncing and index-lock-aware reindex
- [ ] Race regression test passes (watch + analyze_codebase concurrent)
- [ ] All 6 platform binaries built from one Linux host
- [ ] Go source tree removed; CLAUDE.md and old plans/designs marked superseded
- [ ] Fresh clone builds with only Rust toolchain (no Go)
- [ ] Lint, format, test gates green
- [ ] Manual smoke test on a real MCP client confirms watch mode works in practice
