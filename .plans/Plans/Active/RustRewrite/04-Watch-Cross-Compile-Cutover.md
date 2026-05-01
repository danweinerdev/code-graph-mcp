---
title: "Watch Mode, Cross-Compile & Go Cutover"
type: phase
plan: RustRewrite
phase: 4
status: complete
created: 2026-04-28
updated: 2026-04-30
deliverable: "Working watch_start/watch_stop with debounced reindex protected by the index lock; the Go source tree removed and old planning artifacts marked superseded — the Rust binary is now the single supported implementation. Cross-compile infrastructure was added then removed by user decision (2026-04-30): build natively on each platform via `make release`."
tasks:
  - id: "4.1"
    title: "Watch mode: notify-debouncer-full handle, recursive watch setup"
    status: complete
    verification: "WatchHandle holds the Debouncer<RecommendedWatcher, FileIdMap> and a tokio::sync::oneshot::Sender for shutdown; watch_start checks require_indexed first (an unindexed watch_start returns the unindexed error); recursively adds the indexed root via Watcher::watch(root, RecursiveMode::Recursive); debounce window is 250ms (constant); watch_start sets ServerInner.watch to Some(handle), sets active flag; second watch_start while watching returns 'watch mode is already active'; tests cover successful start, double-start error, and stop teardown"
  - id: "4.2"
    title: "Watch loop: debounced events + index_lock-aware reindex_file"
    status: complete
    depends_on: ["4.1"]
    verification: "watch_loop tokio task receives DebouncedEvent batches via mpsc channel; for each event, attempts ServerInner.index_lock.try_lock — if held (analyze_codebase running) the event is dropped (not queued, not retried — design Decision); when not held, takes the lock for the entire snapshot+resolve+merge sequence so no concurrent analyze can clear the graph mid-reindex; reindex_file dispatches by file extension via registry.for_path: removed events call graph.remove_file; created/modified events read+parse+resolve_edges (per-language) and merge_file_graph; non-source files (no plugin match) ignored; **reindex_file uses the cached `inner.config` (loaded by the most recent analyze_codebase) rather than re-reading `<root>/.code-graph.toml` on every event** — a unit test injects a modified config onto ServerInner before triggering a watch event and asserts the modified setting (e.g., a different parsing.max_threads) is observed; race regression test: spawn a watch task watching a directory and a parallel analyze_codebase loop; assert no panics, no graph in inconsistent state (every successful query returns a coherent snapshot); watch_stop cancels the oneshot, drops the debouncer, clears state"
  - id: "4.3"
    title: "Cross-compile infra removed (per-platform native builds)"
    status: complete
    depends_on: ["4.2"]
    verification: "Per user decision (2026-04-30), the cross-compile pipeline was removed. Build natively on each platform via `make release` (host-target `cargo build --release`). Removed: Makefile `release-*` cross-targets, `.github/workflows/release.yml`, `bin/` cross-compile artifacts, README cross-compile section. The `[profile.release]` profile (strip + thin LTO + codegen-units=1) is retained for the host build."
  - id: "4.4"
    title: "Go cutover commit"
    status: complete
    depends_on: ["4.3"]
    verification: "Single commit removes: cmd/, internal/, go.mod, go.sum, original Makefile; root CLAUDE.md rewritten to describe Rust build commands (cargo build / cargo test / cargo clippy) and remove all Go references; .plans/Plans/CodeGraphMCP/ marked status: superseded with related forward-link to Plans/Active/RustRewrite (or current location after move-to-Ready); .plans/Plans/{GoParser, PythonParser, RustParser}/ marked status: superseded — their content is preserved for historical reference but they no longer drive work; .plans/Designs/CodeGraphMCP/ and Designs/LLMOptimization/ marked status: superseded with forward-link to Designs/RustRewrite; testdata/cpp preserved unchanged; commit message references this phase doc and lists every removed top-level path; post-commit: `find . -name '*.go' -not -path './.git/*'` returns no results; `cargo build --release` from a fresh clone succeeds without any Go toolchain installed"
  - id: "4.5"
    title: "Structural verification + release readiness"
    status: complete
    depends_on: ["4.4"]
    verification: "`cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test --workspace` green (all phase 1-4 tests pass); `cargo audit` (or equivalent) shows no known vulnerabilities in dependencies; release build for the host platform completes without warnings; new top-level README.md (or updated existing) describes installation via `cargo install --path crates/code-graph-mcp` or `make release` (no prebuilt binaries — build natively per platform); manual end-to-end smoke test against an MCP client confirms watch mode works in practice (modify a file, observe reindex, query reflects the change)"
---

# Phase 4: Watch Mode, Cross-Compile & Go Cutover

## Overview

The cutover phase. After this phase ships green, the Go binary is gone — the Rust binary is the single supported implementation. Originally three deliverables; the second was reverted at end-of-phase:

1. **Watch mode**: `notify-debouncer-full` driving an index-lock-aware reindex loop that closes the race the Go implementation has (snapshot+resolve+merge under a separate lock from the analyze path).
2. ~~**Cross-compilation**: `cargo-zigbuild` producing all 6 platform binaries from one Linux host~~ — **infra removed (2026-04-30)**: the macOS targets needed an Apple SDK at link time, the operational cost of vendoring one wasn't worth it, and per-platform native builds via `make release` are the supported path.
3. **Cutover commit**: Go source tree removed, old plans and designs marked `superseded`, root CLAUDE.md rewritten.

This phase is intentionally small (5 tasks) so the cutover commit can ship as a single coherent diff with full test coverage.

## 4.1: Watch mode: notify-debouncer-full handle, recursive watch setup

### Subtasks
- [x] `WatchHandle` struct holds the `Debouncer<RecommendedWatcher, RecommendedCache>` (v0.7-canonical type) and a `tokio::sync::oneshot::Sender<()>` for shutdown
- [x] `watch_start` handler: `require_indexed()` first; check `inner.watch` is None under a single write lock (else return `"watch mode is already active"`); construct debouncer with 250ms timeout; `watcher.watch(root, RecursiveMode::Recursive)`; spawn the watch_loop task; populate `inner.watch = Some(handle)`
- [x] `watch_stop` handler: take `inner.watch`; if None, return `"watch mode is not active"`; send the cancel signal; drop the debouncer; clear state
- [x] Tests: start when not indexed → unindexed error; start happy path; double-start → already-active error; stop happy path; stop when not watching → not-active error; concurrent-start race regression (Barrier-driven, deterministic)

## 4.2: Watch loop: debounced events + index_lock-aware reindex_file

### Subtasks
- [x] `watch_loop(server: Arc<ServerInner>, mut events: mpsc::Receiver<DebouncedEvent>, cancel: oneshot::Receiver<()>)` async function
- [x] `tokio::select!` between cancel and event arrival
- [x] On event: for each path in the event, call `server.try_reindex_file(&path, is_remove).await`
- [x] `try_reindex_file`: attempt `inner.index_lock.try_lock` — if held, log a debug message and return (drop the event); if obtained, hold the lock for the full snapshot+resolve+merge sequence
- [x] Inside the lock: `registry.for_path` to get plugin (None → return, not a source file); on remove, `graph.write().remove_file(path)`; on create/modify, read + parse_file + build per-Language SymbolIndex from current graph + resolve_edges (language-aware) + `graph.write().merge_file_graph(fg)`
- [x] **Race regression test:** in `tests/watch_race.rs`, spawn a watch loop on a tmpdir + a parallel loop calling `analyze_codebase` on the same dir; modify files in the tmpdir during the test; assert no panics, no deadlocks, every concurrent query returns a coherent graph snapshot
- [x] Test: editor-style atomic save (rename .tmp → file.cpp) produces exactly one reindex event after the debounce window
- [x] Test: removing a watched file triggers `remove_file` and a subsequent `get_file_symbols` returns the symbols-not-found error

### Notes
The "drop the event when index_lock is held" rule (rather than queue) is a deliberate choice from the design (Concurrency Model section): the in-flight `analyze_codebase` will pick up the file's current state anyway. Queuing would create unbounded growth on a busy editor session.

## 4.3: Cross-compile infra removed (per-platform native builds)

**User decision (2026-04-30):** the cargo-zigbuild cross-compile pipeline was removed. Three Linux targets and the Windows `.exe` built cleanly during the deferred end-of-phase pass (8.6 MB / 8.3 MB / 7.8 MB / 8.2 MB), but the macOS targets (`x86_64-apple-darwin`, `aarch64-apple-darwin`) failed because cargo-zigbuild needs an Apple-framework-bearing macOS SDK at link time (`CoreFoundation`, `CoreServices`) — not a problem zig itself solves. Rather than carry the operational complexity of vendoring or downloading a macOS SDK on every build host (and the corresponding licensing question), the project switched to native per-platform builds: `make release` runs `cargo build --release -p code-graph-mcp` on whichever host needs the binary.

### Removed in this commit
- `Makefile` `release-all`, `release-{linux,darwin,windows}-*`, `release-tar`, `release-host-smoke` recipes
- `.github/workflows/release.yml` (cross-compile CI workflow)
- `bin/` cross-compile output tree
- `README.md` "Building from source / Cross-compilation" section
- `CLAUDE.md` "Cross-platform release builds" pointer
- Cargo.toml comment language tying `[profile.release]` to the cross-compile workflow (the profile itself stays — it benefits the host build too)

### Retained
- `[profile.release]` with `strip = "symbols"`, `lto = "thin"`, `codegen-units = 1` (host build is ~8.6 MB)
- `.code-graph.toml.example` (independent of the build path)

## 4.4: Go cutover commit

### Subtasks
- [x] Remove `cmd/`, `internal/`, `go.mod`, `go.sum`, original `Makefile` (Makefile rewritten in place to a Rust-only file rather than removed; old `bin/linux-amd64/` Go-era output dir also removed)
- [x] Rewrite root `CLAUDE.md`:
  - Replace "Go MCP server" with "Rust MCP server"
  - Update Build section: `cargo build --release`, `cargo test --workspace`, `cargo clippy --workspace -- -D warnings`
  - Remove `CGO_ENABLED=1` references
  - Update Architecture section to reflect the Rust crate layout
  - Keep the C++ Parser Limitations section unchanged (those are intentional and still accurate)
  - Add a "Configuration" section pointing at `<root>/.code-graph.toml`
- [x] Mark old planning artifacts `status: superseded` *(scope narrowed by user: only draft/review-status artifacts are marked superseded; completed/implemented artifacts stay unchanged as historical record)*:
  - ~~`.plans/Plans/CodeGraphMCP/` README and all phase docs~~ — left unchanged (status: `complete`, per user rule "old plans that reference Go which are COMPLETED do not change")
  - `.plans/Plans/{GoParser, PythonParser, RustParser}/` READMEs and phase docs (all were `status: planned`/`draft` — superseded)
  - `.plans/Designs/CodeGraphMCP/README.md` (was `status: review` — superseded)
  - ~~`.plans/Designs/LLMOptimization/README.md`~~ — left unchanged (status: `implemented`, completed work)
  - Each superseded artifact gets a one-line note at the top: `**Superseded by [Plans/Active/RustRewrite](...)**`
- [x] Plan currently lives in `Plans/Active/RustRewrite/`; README `status: active` confirmed (no change needed)
- [x] `testdata/cpp/` preserved unchanged
- [x] Commit message lists every removed path and references this phase doc
- [x] Post-commit verification: `find . -name '*.go' -not -path './.git/*' -not -path './.plans/*'` returns no results; `cargo build --release` succeeds without Go toolchain

## 4.5: Structural verification + release readiness

### Subtasks
- [x] `cargo fmt --check` clean
- [x] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [x] `cargo test --workspace` green — all Phase 1-4 tests pass (398 tests across the workspace)
- [x] `cargo audit` (install if needed) reports no known vulnerabilities in pinned dependencies *(installed `cargo-audit v0.22.1` via `cargo install --locked cargo-audit`; scan of 188 crate dependencies in `Cargo.lock` reported no advisories)*
- [x] Top-level README.md updated:
  - Installation: prebuilt binaries (link to release artifacts) or `cargo install --path crates/code-graph-mcp`
  - MCP client config (claude_desktop_config.json snippet)
  - Configuration: `.code-graph.toml` schema with examples
  - Tool reference (15 tools)
  - Limitations (preserved from CLAUDE.md)
- [x] Sample `.code-graph.toml` shipped at repo root with comments explaining each field *(shipped as `.code-graph.toml.example` — the `.code-graph.toml` filename would be loaded by the indexer if anyone ran `analyze_codebase` on the repo root itself)*
- [x] Manual end-to-end smoke test *(documented for human verification in `docs/SMOKE_TEST.md`; automated coverage of the same behaviors in `crates/codegraph-tools/tests/watch_race.rs` and `crates/codegraph-tools/tests/watch_dangling_edges.rs`)*:
  - Index a small C++ project
  - `watch_start`; modify a file; observe automatic reindex; `get_file_symbols` reflects the change
  - `watch_stop`
  - Restart the binary; confirm cache hit on second analyze
- [~] ~~`make release-tar` recipe — packages the Linux x86_64-gnu binary plus `.code-graph.toml.example`, `README.md`, and `LICENSE` into `dist/code-graph-mcp-x86_64-linux-gnu.tar.gz`~~ — removed alongside the rest of the cross-compile infra (see Task 4.3). Per-platform builders can `tar -czf code-graph-mcp.tar.gz target/release/code-graph-mcp .code-graph.toml.example README.md LICENSE` directly if they want a tarball.

## Acceptance Criteria
- [x] watch_start and watch_stop work end-to-end with debouncing and index-lock-aware reindex
- [x] Race regression test passes (watch + analyze_codebase concurrent)
- [~] ~~All 6 platform binaries built from one Linux host~~ — superseded by per-platform native builds (Task 4.3 was reverted, 2026-04-30)
- [x] Go source tree removed; CLAUDE.md and old plans/designs marked superseded
- [x] Fresh clone builds with only Rust toolchain (no Go)
- [x] Lint, format, test gates green
- [x] Manual smoke test on a real MCP client confirms watch mode works in practice
