---
title: "MCP Server, Tools, Persistence & Parallel Discovery"
type: phase
plan: RustRewrite
phase: 3
status: planned
created: 2026-04-28
updated: 2026-04-28
deliverable: "code-graph-mcp binary serving 15 wire-format-compatible MCP tools over stdio (rmcp) with the parallel discovery walker, the per-job rayon parsing pool, the rayon→tokio progress bridge, language-aware edge resolution, and atomic JSON cache v2 persistence"
tasks:
  - id: "3.1"
    title: "rmcp server scaffold, ServerInner state, require_indexed guard"
    status: planned
    verification: "code-graph-mcp bin starts and serves stdio MCP via `rmcp::ServiceExt::serve(stdio()).await`; ServerInner holds graph (parking_lot::RwLock), registry, indexed AtomicBool, index_lock (tokio::sync::Mutex), root_path (RwLock<Option<PathBuf>>), watch (RwLock<Option<WatchHandle>>), config (RwLock<RootConfig>); require_indexed returns the exact Go error wording 'no codebase indexed — call analyze_codebase first'; #[tool_router] macro generates dispatch table; #[tool_handler] wires ServerHandler trait; binary built via `cargo build --release` and a smoke MCP `tools/list` call returns 15 tools; tool descriptions are copied verbatim from `internal/tools/tools.go` for 13 of the 15 tools (every error wording, did-you-mean wording, and parameter description preserved byte-for-byte) — **two tools require updates**: (a) `analyze_codebase` description widens from 'Index a C/C++ codebase…' to 'Index a codebase (C/C++, Rust, Go, Python) and build the code graph. Must be called before any query tools.'; (b) `search_symbols` adds a parameter description for the new `language` filter ('Filter by source language: cpp, rust, go, or python'); these two updated strings are captured as the snapshot baseline in task 3.7 and become the wire-format-of-record going forward; ProgressSink trait is defined in `codegraph-tools::indexer` as a stub interface here so subsequent tasks (3.2 discovery, 3.3 ChannelProgressSink) can both depend on it without circularity"
  - id: "3.2"
    title: "Parallel discovery walker with config-controlled thread pool"
    status: planned
    depends_on: ["3.1"]
    verification: "codegraph-tools::discovery::discover walks via ignore::WalkBuilder::build_parallel().threads(N) where N = cfg.discovery.max_threads (already clamped to NumCPU); files filtered in-thread by registry.language_for_path so non-source files (e.g. .png .o node_modules/*.js) never enter the result Vec; results collected via crossbeam_channel::unbounded; walk warnings (permission denied, broken symlinks) flow via a sibling channel and surface in the Discovered.warnings field; respects .gitignore by default (cfg.respect_gitignore); follow_symlinks defaults to false; extra_ignore patterns honored; integration test on a synthetic 50k-file directory tree with mixed languages, ignored dirs, and unsupported extensions produces only registered-language files in the result; benchmark vs the Phase 1 synchronous walkdir shows measurable speedup on a directory with >1000 files; **walker-migration regression gate**: after `codegraph-parse-test` is migrated to use `discovery::discover` (replacing the Phase 1.6 synchronous walkdir), re-run both Phase 1 corpus gates and confirm identical numbers — `parse-test testdata/cpp/` produces 17 symbols / 21 edges (unchanged from Phase 1.6) and `parse-test <fmtlib/fmt clone>/src/` produces 32 symbols / 244 edges with 0 crashes / 0 warnings (unchanged from Phase 1.6); any divergence indicates the parallel walker is dropping or duplicating files"
  - id: "3.3"
    title: "Indexer: per-job rayon pool + language-aware edge resolution + tokio progress bridge"
    status: planned
    depends_on: ["3.2"]
    verification: "Indexer constructs a per-job rayon::ThreadPoolBuilder with num_threads=cfg.parsing.max_threads (clamped); pool.install runs par_iter().map(parse) so the global rayon pool isn't monopolized; ChannelProgressSink writes to a tokio::sync::mpsc::Sender from rayon worker threads via try_send; a small tokio task on the async side drains and forwards to peer.notify_progress so notifications reach the agent without requiring Peer<RoleServer>: Send into spawn_blocking; resolve_all_edges dispatches per-language: SymbolIndex keyed by (Language, name) so a Python init never collides with C++ init; LanguagePlugin::resolve_call default impl mirrors Go's same-file>same-parent>same-namespace>global heuristic; LanguagePlugin::resolve_include default does basename matching; warnings from parse failures, read failures, and concurrency clamping all flow into the analyze_codebase response warnings array"
  - id: "3.4"
    title: "P0 tool handlers (8): analyze_codebase, get_file_symbols, search_symbols, get_symbol_detail, get_symbol_summary, get_callers, get_callees, get_dependencies"
    status: planned
    depends_on: ["3.3"]
    verification: "analyze_codebase parameters are `path` (required) and `force` (optional bool); the optional `language` filter mentioned in the design's MCP Tools table is **out of scope for this rewrite** — analyze always indexes all registered languages — and is captured as Open Question 7 in the design for future work; analyze_codebase loads .code-graph.toml, runs discovery → parse → resolve → merge inside spawn_blocking, sends progress notifications, returns AnalyzeResult JSON with files/symbols/edges/root_path/warnings — surfaces concurrency-clamp warnings if config exceeded NumCPU; get_file_symbols returns Vec<symbolResult> never null even when top_level_only filters everything out (initialize as `Vec::with_capacity(...)`); brief defaults to true on both get_file_symbols and search_symbols; search_symbols requires at least one of query/kind/namespace/language — language-only search is explicitly accepted; pagination envelope `{results, total, offset, limit}`; get_symbol_detail returns full detail (brief=false semantics) and produces did-you-mean suggestions from a 100-candidate pool on not-found; get_symbol_summary handles optional file param; get_callers/get_callees use BFS depth (default 1), did-you-mean on unknown symbol; get_dependencies returns [] (never null) for unknown or include-empty files; integration tests cover happy paths + each error path with exact wording"
  - id: "3.5"
    title: "P1+P2 tool handlers (5 real + 2 stubs): detect_cycles, get_orphans, get_class_hierarchy, get_coupling, generate_diagram, watch_start stub, watch_stop stub"
    status: planned
    depends_on: ["3.4"]
    verification: "After this task, `tools/list` returns exactly 15 tools — task 3.4 ships 8 P0 handlers (analyze_codebase, get_file_symbols, search_symbols, get_symbol_detail, get_symbol_summary, get_callers, get_callees, get_dependencies); task 3.5 ships 5 substantive P1+P2 handlers (detect_cycles, get_orphans, get_class_hierarchy, get_coupling, generate_diagram) plus 2 stubs (watch_start, watch_stop); 8 + 5 + 2 = 15. Per handler: detect_cycles returns [] (not null) on acyclic graphs, [[a,b],...] on cycles; get_orphans defaults to callables, accepts kind filter; get_class_hierarchy uses widened {Class, Struct, Interface, Trait} root filter, depth (default 1) supports transitive walk, did-you-mean on unknown class; get_coupling supports direction in {outgoing(default), incoming, both}; generate_diagram dispatches by exclusive parameter (symbol|file|class), depth default 1, max_nodes default 30, format in {edges(default), mermaid}, styled bool default false; mermaid output is valid graph syntax; edges format returns [] never null on empty; all error paths produce exact Go-matching error wording; watch_start and watch_stop registered as tool stubs that return McpError with 'watch mode not yet implemented in this build' (Phase 4 replaces both with real implementations)"
  - id: "3.6"
    title: "Persistence: cache v2 with atomic save"
    status: planned
    depends_on: ["3.3"]
    verification: "GraphCache v2 schema includes `version: u32 = 2`, generator string, nodes, adj, radj, files (with Language tag), includes, mtimes; Graph::save(dir) writes to `<dir>/.code-graph-cache.json.tmp`, calls File::sync_all(), then std::fs::rename to swap atomically — verified by a fault-injection test that crashes mid-write and confirms the original cache file is intact; Graph::load(dir) returns Ok(false) when file is absent or version mismatch (not an error — triggers re-index); Ok(true) on successful load; Err on IO failure or JSON parse failure; stale_paths(dir) returns paths whose current mtime differs from cached mtime; tests cover save/load round-trip, version mismatch silent-reindex, mtime invalidation, atomic-rename crash safety"
  - id: "3.7"
    title: "Wire-format snapshot tests + integration tests"
    status: planned
    depends_on: ["3.4", "3.5", "3.6"]
    verification: "tests/snapshots/ directory holds insta-managed snapshots of every tool's tools/list schema entry (description + parameters JSON schema) and a representative response body for each tool (analyze_codebase summary, get_file_symbols full result, search_symbols pagination envelope, get_symbol_detail full detail, etc.); the snapshot suite is captured initially against output produced by the Rust binary indexing testdata/cpp/ and is then frozen — any divergence in a future PR triggers `cargo insta review`; integration tests in tests/ exercise: end-to-end MCP startup → tools/list → analyze_codebase → each query tool → expected JSON shape; concurrent analyze_codebase calls return 'indexing already in progress' for the second; analyze with bad path returns the exact Go error string; cache hit on second analyze_codebase (no force flag) loads from .code-graph-cache.json"
  - id: "3.8"
    title: "Structural verification"
    status: planned
    depends_on: ["3.7"]
    verification: "`cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo test --workspace` green including all snapshot tests, integration tests, and per-tool unit tests; release build (`cargo build --release`) produces a working binary; binary smoke-tests via a real MCP client (Claude Desktop or scripted JSON-RPC over stdio) exposing all 15 tools; no new unsafe; no allow attributes added to suppress findings"
---

# Phase 3: MCP Server, Tools, Persistence & Parallel Discovery

## Overview

The largest phase — wires the Phase 2 graph engine and the Phase 1 C++ parser into a working MCP server. Ships the parallel discovery walker (the user's massive-codebase optimization), the per-job rayon parsing pool, the rayon→tokio progress notification bridge, language-aware edge resolution with `(Language, name)`-keyed symbol index, all 15 MCP tools with wire-format snapshot tests, and atomic JSON cache v2 persistence. After this phase the Rust binary is feature-equivalent to the Go binary for C++ workloads (Phase 4 adds watch mode and ships the cutover).

## 3.1: rmcp server scaffold, ServerInner state, require_indexed guard

### Subtasks
- [ ] `crates/code-graph-mcp/src/main.rs`: `#[tokio::main] async fn main() -> anyhow::Result<()>` builds the registry (Cpp parser only for this phase), constructs `CodeGraphServer`, calls `server.serve(stdio()).await?.waiting().await`
- [ ] `crates/codegraph-tools/src/server.rs`: `CodeGraphServer { inner: Arc<ServerInner> }` plus `ServerInner` with all fields per Designs/RustRewrite/ State Management section
- [ ] `require_indexed(&self) -> Result<(), McpError>` returns the exact Go error string
- [ ] `#[tool_router] impl CodeGraphServer { ... }` declares all 15 tools (handlers can stub initially with `unimplemented!()` for sub-task tracking, then fill in across 3.4 and 3.5)
- [ ] `#[tool_handler] impl ServerHandler for CodeGraphServer` provides the default rmcp wiring
- [ ] Tool descriptions copied verbatim from `internal/tools/tools.go` `mcp.WithDescription(...)` strings so wire-format snapshots match byte-for-byte
- [ ] Smoke test: `cargo run --release` then send `{"jsonrpc":"2.0","id":1,"method":"tools/list"}` over stdio; assert 15 tools returned with matching descriptions

## 3.2: Parallel discovery walker with config-controlled thread pool

### Subtasks
- [ ] `crates/codegraph-tools/src/discovery.rs`: `discover(root, registry, &cfg.discovery, progress) -> Discovered`
- [ ] Use `ignore::WalkBuilder::new(root).threads(cfg.max_threads).standard_filters(cfg.respect_gitignore).follow_links(cfg.follow_symlinks)`
- [ ] `extra_ignore` patterns added via `add_ignore_path_from_pattern` (or globset wrapper if needed)
- [ ] `build_parallel().run(|| ...)` — per-thread closure receives `Result<DirEntry, Error>` and returns `WalkState::Continue`
- [ ] Inside the closure: file-type check (skip dirs), language registry check (`language_for_path`), send to `crossbeam_channel::Sender<DiscoveredFile>`
- [ ] Walk errors sent to a sibling warnings channel
- [ ] After `run()` returns, drop senders and drain receivers; build `Discovered { files, warnings }`
- [ ] Progress: report 0/total at start, then a "Discovered N files across M languages" message
- [ ] Test: synthetic directory tree (use `tempfile::TempDir`) with 50k files mixed across .cpp .py .rs .go .png .o node_modules/*.js — verify only the 4 source extensions are returned
- [ ] Test: `.gitignore` containing `target/` excludes those files when `respect_gitignore=true`, includes them when false
- [ ] Bench (criterion or simple `Instant`-based): 1k file directory walk with N=4 threads completes faster than synchronous walkdir baseline

### Notes
The walker is thread-safe by construction; `LanguageRegistry` is `Send + Sync` because all plugins are `Send + Sync` (LanguagePlugin trait bound).

## 3.3: Indexer: per-job rayon pool + language-aware edge resolution + tokio progress bridge

### Subtasks
- [ ] `codegraph-tools::indexer::index_directory(root, registry, cfg, progress)` orchestrates discover → parse → resolve
- [ ] Construct per-job `rayon::ThreadPoolBuilder::new().num_threads(cfg.parsing.max_threads).thread_name(|i| format!("codegraph-parse-{i}")).build()?`
- [ ] `pool.install(|| files.par_iter().map(parse).collect())` keeps parsing scoped to the job pool
- [ ] `parse` closure: `registry.plugin_for(df.language)`, `std::fs::read(&df.path)`, `plugin.parse_file(&df.path, &content)`, increment counter, send progress
- [ ] `ProgressSink` trait + `ChannelProgressSink(tokio::sync::mpsc::Sender<ProgressEvent>)` implementation; `try_send` so a full channel doesn't block the rayon thread (progress is best-effort)
- [ ] `analyze_codebase` handler spawns a forwarding tokio task before `spawn_blocking`: receiver task pulls events and calls `peer.notify_progress`; sender drops when blocking job ends, signalling task exit
- [ ] `resolve_all_edges` dispatches per-`Language`: `SymbolIndex { by_name: HashMap<(Language, String), Vec<SymbolEntry>> }` so cross-language collisions are impossible
- [ ] `LanguagePlugin::resolve_call` default impl ports the Go scope-aware heuristic (same_file=4, same_parent=3, same_namespace=2)
- [ ] `LanguagePlugin::resolve_include` default impl ports basename + suffix matching
- [ ] Tests: scoped resolution (multiple candidates → highest-score wins); language isolation (Python `init` never returned for C++ resolve_call); progress events received in order

## 3.4: P0 tool handlers (8)

### Subtasks (one per tool)
- [ ] **`analyze_codebase`** — try_lock index_mu; load RootConfig; resolve_concurrency (warnings); cache hit path (load + stale_paths check); discover; spawn_blocking(parse + resolve); merge under graph write lock; save cache; return AnalyzeResult JSON with warnings array including config clamp warnings
- [ ] **`get_file_symbols`** — params: `file` (required), `top_level_only` (default false), `brief` (default true); `Vec::with_capacity(symbols.len())` so empty filter result serializes as `[]` not `null`
- [ ] **`search_symbols`** — params: `query`, `kind`, `namespace`, `language`, `limit` (default 20), `offset` (default 0), `brief` (default true); validation: at least one of query/kind/namespace/language must be non-empty (else `'query', 'kind', 'namespace', or 'language' is required`); pagination envelope; brief mode omits column/end_line/signature
- [ ] **`get_symbol_detail`** — params: `symbol`; full detail (brief=false); did-you-mean uses `suggestSymbols(name, 5)` which calls Search with `Limit: 100` so candidate pool isn't 20-capped
- [ ] **`get_symbol_summary`** — params: `file` (optional)
- [ ] **`get_callers`** — params: `symbol`, `depth` (default 1); did-you-mean on not-found
- [ ] **`get_callees`** — same shape as get_callers
- [ ] **`get_dependencies`** — params: `file`; returns `Vec<PathBuf>`, marshalled as JSON array (never null)

### Notes
Every handler returns `Ok(CallToolResult)` even for user errors — `Err(McpError)` is reserved for protocol-level failures (deserialization). Error messages match Go's wording byte-for-byte to keep snapshots stable.

## 3.5: P1+P2 tool handlers (4) + watch_start/watch_stop stubs

### Subtasks
- [ ] **`detect_cycles`** — no params; cycles always Vec, never null
- [ ] **`get_orphans`** — params: `kind`; default callables only; brief mode for symbol results
- [ ] **`get_class_hierarchy`** — params: `class`, `depth` (default 1); did-you-mean on unknown
- [ ] **`get_coupling`** — params: `file`, `direction` (default outgoing); 'both' merges outgoing+incoming
- [ ] **`generate_diagram`** — params: `symbol` | `file` | `class` (exactly one), `depth` (default 1), `max_nodes` (default 30), `format` (default edges), `styled` (default false); dispatch to diagram_call_graph / diagram_file_graph / diagram_inheritance based on which exclusive param is set; format=edges returns `Vec<DiagramEdge>` (never null); format=mermaid returns rendered string
- [ ] **`watch_start` stub** — returns `McpError::tool_error("watch mode not yet implemented in this build")` (Phase 4 fills in the real handler)
- [ ] **`watch_stop` stub** — same

## 3.6: Persistence: cache v2 with atomic save

### Subtasks
- [ ] `codegraph-graph::persist::GraphCache` v2 schema with `version: u32 = 2` and `generator: String`
- [ ] `Graph::save(dir)` serializes a snapshot, writes to `<dir>/.code-graph-cache.json.tmp`, `File::sync_all()`, then `std::fs::rename(tmp, final)`
- [ ] `Graph::load(dir)` reads, parses, version-checks; v1 (Go cache) → return Ok(false) (silent re-index); v2 → populate graph and return Ok(true)
- [ ] `stale_paths(dir)` reads only the `mtimes` field, compares against current mtimes, returns paths needing re-parse
- [ ] `cache_path(dir)` helper for tests
- [ ] Crash-safety test: kill the save mid-stream by replacing the writer with a panicking one; assert the original cache file is byte-identical
- [ ] Round-trip test: build a graph, save, clear, load, assert all queries return identical results
- [ ] mtime invalidation test: save, modify a tracked file's mtime, call stale_paths, assert that file is reported

## 3.7: Wire-format snapshot tests + integration tests

### Subtasks
- [ ] `tests/snapshots/` directory created; insta configured per workspace
- [ ] Snapshot every `tools/list` entry (description + parameters JSON schema) — one snapshot file per tool, including the `watch_start` and `watch_stop` stub entries (their schemas snapshot now so Phase 4's real implementations trigger `cargo insta review` if the parameters change)
- [ ] Snapshot a representative response body for each tool, captured against `testdata/cpp/`:
  - `analyze_codebase` → AnalyzeResult JSON
  - `get_file_symbols` for a known file → array of symbolResult
  - `search_symbols` with `query=Engine` → pagination envelope
  - `get_symbol_detail` for `engine.cpp:Engine::update` → full symbolResult
  - `get_symbol_summary` for the whole graph → namespace map
  - `get_callers` and `get_callees` for `engine.cpp:Engine::update` → CallChain array
  - `get_dependencies` for `engine.cpp` → resolved include paths
  - `detect_cycles` → cycle list (with `circular_a.h`/`circular_b.h`)
  - `get_orphans` → orphan list
  - `get_class_hierarchy` for `Engine` → HierarchyNode tree
  - `get_coupling` for `engine.cpp` (each direction) → file→count map
  - `generate_diagram` (each combination of symbol/file/class × edges/mermaid)
- [ ] Integration tests in `tests/integration.rs`:
  - End-to-end startup → analyze testdata/cpp → each query
  - Concurrent analyze_codebase: second call returns 'indexing already in progress'
  - Bad path returns 'directory does not exist'
  - Cache hit on second analyze (no force) — confirm by deleting a parsed file and verifying the cached graph still has it (cache trumps walk)
  - `force=true` skips cache load
  - Stale-mtime detection: modify a file's mtime, call analyze, expect re-parse path

## 3.8: Structural verification

### Subtasks
- [ ] `cargo fmt --check` clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` green including snapshot suite and integration tests
- [ ] Release build produces a working binary
- [ ] Manual smoke test against an MCP client (Claude Desktop or a scripted client): tools/list returns 15 tools; analyze_codebase indexes a small project; queries return sensible JSON
- [ ] No `#[allow(clippy::...)]` introduced; no new `unsafe`

## Acceptance Criteria
- [ ] Binary starts and serves all 15 tools over stdio via rmcp
- [ ] Parallel discovery walker honors `<root>/.code-graph.toml` and clamps to NumCPU with warnings
- [ ] Per-job rayon pool runs parsing concurrently with progress notifications reaching the client
- [ ] Language-aware edge resolution prevents cross-language symbol collisions
- [ ] All 15 tools' wire-format matches the Go binary (snapshot tests green)
- [ ] Cache v2 persists and reloads correctly; atomic save survives crash injection
- [ ] Lint, format, and test gates green
