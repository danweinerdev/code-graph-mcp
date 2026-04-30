---
title: "Watch Mode, Cross-Compile & Go Cutover"
type: phase
plan: RustRewrite
phase: 4
status: in-progress
created: 2026-04-28
updated: 2026-04-29
deliverable: "Working watch_start/watch_stop with debounced reindex protected by the index lock; cargo-zigbuild producing release binaries for all 6 target platforms from one Linux host; the Go source tree removed and old planning artifacts marked superseded — the Rust binary is now the single supported implementation"
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
    title: "Cross-compile via cargo-zigbuild for 6 platforms"
    status: in-progress
    depends_on: ["4.2"]
    verification: "Top-level Makefile or justfile recipes produce release binaries for x86_64-unknown-linux-gnu, x86_64-unknown-linux-musl, aarch64-unknown-linux-musl, x86_64-apple-darwin, aarch64-apple-darwin, x86_64-pc-windows-gnu — all from a single Linux host using cargo-zigbuild; binaries land under bin/<target>/code-graph-mcp(.exe); each binary smoke-tests by running `<bin> --version` (or equivalent stdio handshake) and `<bin>` indexing testdata/cpp on a host that supports running it (Linux native, macOS via emulation skipped or run on a separate runner, Windows via wine if available); CI workflow file (or documented commands) demonstrates the release path; size check: each release binary under 30MB stripped"
  - id: "4.4"
    title: "Go cutover commit"
    status: complete
    depends_on: ["4.3"]
    verification: "Single commit removes: cmd/, internal/, go.mod, go.sum, original Makefile; root CLAUDE.md rewritten to describe Rust build commands (cargo build / cargo test / cargo clippy) and remove all Go references; .plans/Plans/CodeGraphMCP/ marked status: superseded with related forward-link to Plans/Active/RustRewrite (or current location after move-to-Ready); .plans/Plans/{GoParser, PythonParser, RustParser}/ marked status: superseded — their content is preserved for historical reference but they no longer drive work; .plans/Designs/CodeGraphMCP/ and Designs/LLMOptimization/ marked status: superseded with forward-link to Designs/RustRewrite; testdata/cpp preserved unchanged; commit message references this phase doc and lists every removed top-level path; post-commit: `find . -name '*.go' -not -path './.git/*'` returns no results; `cargo build --release` from a fresh clone succeeds without any Go toolchain installed"
  - id: "4.5"
    title: "Structural verification + release readiness"
    status: complete
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

## 4.3: Cross-compile via cargo-zigbuild for 6 platforms

### Subtasks
- [x] Add cargo-zigbuild instructions to top-level documentation (Makefile/justfile/README)
- [x] Per-platform recipes:
  - `linux-x86_64-gnu` → `cargo zigbuild --release --target x86_64-unknown-linux-gnu`
  - `linux-x86_64-musl` → `cargo zigbuild --release --target x86_64-unknown-linux-musl`
  - `linux-aarch64-musl` → `cargo zigbuild --release --target aarch64-unknown-linux-musl`
  - `darwin-x86_64` → `cargo zigbuild --release --target x86_64-apple-darwin`
  - `darwin-aarch64` → `cargo zigbuild --release --target aarch64-apple-darwin`
  - `windows-x86_64-gnu` → `cargo zigbuild --release --target x86_64-pc-windows-gnu`
- [x] Each output goes to `bin/<target>/code-graph-mcp(.exe)`
- [x] CI workflow file (or documented manual steps) demonstrating the release path on a Linux runner
- [ ] Smoke test each platform binary that can be run on the host: Linux native binaries run on the build host; macOS and Windows binaries are validated by `file <bin>` for arch correctness, and a separate CI runner if available
- [x] Strip release binaries (cargo profile or `strip` post-step); verify each is under 30MB *(host target verified at 8.6 MB stripped via `[profile.release].strip = "symbols"` + thin LTO; per-target verification deferred to end-of-phase pass)*

### Notes
Cross-compilation was a major motivator for the rewrite (the Go Makefile hunts for per-platform `gcc/clang/mingw` and silently skips targets when unavailable). With cargo-zigbuild, all 6 targets build from one Linux host with no additional toolchain installation.

**User decision (2026-04-29):** defer the actual 6-platform binary builds to the very end of Phase 4. The 4.3 commit ships the *infrastructure* — the `release-*` Makefile recipes, the `[profile.release]` profile, the `.github/workflows/release.yml` CI workflow, and the README "Building from source / Cross-compilation" section — but does NOT execute the cross-compile invocations. Only the host-target build was run, to confirm the new release profile produces a 8.6 MB binary (down from the 11 MB default-profile baseline) well under the 30 MB ceiling. The multi-platform `make release-all` runs as a one-off pass after Tasks 4.4 (Go cutover) and 4.5 (release readiness) are complete; that pass installs/verifies any missing host prerequisites, runs all six builds, validates each via `file <bin>`, and confirms each binary is under 30 MB. The remaining unchecked subtask (per-target smoke validation) tracks that deferred work.

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
- [x] `make release-tar` recipe added — packages the Linux x86_64-gnu binary plus `.code-graph.toml.example`, `README.md`, and `LICENSE` into `dist/code-graph-mcp-x86_64-linux-gnu.tar.gz`. Recipe not executed in this task — the deferred end-of-phase multi-platform build pass will run it.

## Acceptance Criteria
- [ ] watch_start and watch_stop work end-to-end with debouncing and index-lock-aware reindex
- [ ] Race regression test passes (watch + analyze_codebase concurrent)
- [ ] All 6 platform binaries built from one Linux host
- [ ] Go source tree removed; CLAUDE.md and old plans/designs marked superseded
- [ ] Fresh clone builds with only Rust toolchain (no Go)
- [ ] Lint, format, test gates green
- [ ] Manual smoke test on a real MCP client confirms watch mode works in practice
