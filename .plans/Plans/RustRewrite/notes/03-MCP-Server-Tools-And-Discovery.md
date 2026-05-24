---
title: "Phase 3 Debrief: MCP Server, Tools, Persistence & Parallel Discovery"
type: debrief
plan: "RustRewrite"
phase: 3
phase_title: "MCP Server, Tools, Persistence & Parallel Discovery"
status: complete
created: 2026-04-29
updated: 2026-04-29
tags: [rewrite, rust, mcp, code-graph, tree-sitter, cpp, multi-language]
---

# Phase 3 Debrief: MCP Server, Tools, Persistence & Parallel Discovery

## Decisions Made

- **Locking lives at `ServerInner`, not inside `Graph`.** `pub use parking_lot::RwLock` is re-exported from `codegraph-graph`; the server wraps `Graph` in `RwLock` directly via this re-export. The `tokio::sync::Mutex` `index_lock` provides the single-flight `analyze_codebase` guard. This matches the design's State Management section and avoids the Go binary's "lock inside the Graph methods + lock at the call site" double-lock pattern.
- **`require_indexed` returns `Result<(), CallToolResult>`, not `Result<(), McpError>`.** First Phase 3.1 submission used `McpError::internal_error`, which propagates as a JSON-RPC protocol error (code -32603) instead of the tool-level `{is_error: true}` envelope. The rmcp 1.5 SDK exposes `CallToolResult::error(content)` which produces the right shape; handlers use the `if let Err(r) = self.require_indexed() { return Ok(r); }` early-return pattern (NOT `?`) so the error envelope reaches the wire correctly.
- **Tool descriptions copied byte-for-byte from Go for 13 tools.** Two updates: `analyze_codebase` widened to "Index a codebase (C/C++, Rust, Go, Python)…", and `search_symbols` gains a `language` parameter. The 35-snapshot suite locks every description plus full JSON schema; future drift triggers `cargo insta review`.
- **Handler bodies live in submodules (`handlers/{analyze,symbols,query,structure,watch}.rs`).** Keeps `server.rs` lean (~700 lines) instead of growing past 1500 once all 15 handlers shipped. The `#[tool]`-decorated methods on `CodeGraphServer` are thin parameter-marshalling wrappers; the actual work lives in plain `async fn` exports that take `&Arc<ServerInner>` so unit tests can call them without spinning up an rmcp router.
- **Parallel discovery via `ignore::WalkBuilder::build_parallel`.** This is the user's massive-codebase optimization — `.gitignore`/`.git/info/exclude` respected by default; `require_git(false)` set so subtrees and non-git source roots also honor `.gitignore`. Files filtered in-thread via `registry.language_for_path`. `crossbeam_channel` collects results across worker threads.
- **Per-job rayon pool, not the global pool.** `rayon::ThreadPoolBuilder::new().num_threads(cfg.parsing.max_threads).build()?` then `pool.install(|| par_iter().map(parse))`. Scoped so `analyze_codebase` never starves other concurrent rayon work elsewhere in the binary.
- **Rayon→tokio progress bridge via `tokio::sync::mpsc`.** `peer.notify_progress` cannot run inside `spawn_blocking` (rmcp 1.5 doesn't guarantee `Peer<RoleServer>: Send` across the blocking boundary). Solution: spawn a forwarder tokio task BEFORE `spawn_blocking`; the forwarder owns the receiver and calls `peer.notify_progress` for each event. The blocking job pushes via `try_send` (best-effort drop on full channel). When the blocking job completes, the sender drops and the forwarder exits cleanly.
- **`SymbolIndex` keyed by `(Language, name)`.** Cross-language collisions become structurally impossible — a Python `init` cannot resolve to a C++ `init` and vice versa. This is the multi-language design's headline payoff in the indexer.
- **Cache v2 with atomic save** (write `.tmp` → `sync_all()` → `rename`). Closes the partial-write window the Go binary's `os.WriteFile` had. v1 (Go-written) caches return `Ok(false)` from `load` either via the structured-version-mismatch path OR via `PersistError::Json` (Go's schema doesn't match Rust's, so any non-empty Go cache fails to deserialize). The handler treats both as "re-index from scratch."
- **`mtime` stored as `u64` nanoseconds since epoch.** Pre-epoch timestamps aren't real on supported filesystems. Files whose mtime can't be read get `0` so the next `stale_paths` call flags them.
- **Edge resolution preserved Go's same-namespace bug for parity, but fixed `caller_id_parent`.** Go's `resolveCall` has `callerNS` initialized to `""` and never updated, so the score=2 same-namespace bonus is dead code. The Rust port preserves this with an explicit `let _ = caller_ns;` and a `// NOTE: matches Go's resolveCall, including the unreachable same-namespace bonus` comment. Separately, Go's `caller_id_parent` (via `LastIndex(":")` on `file:Foo::bar`) lands on the second `:` of `::` and produces an empty parent — so the same-parent bonus (score=3) was *also* dead code in Go. Rust's `caller_id_parent` correctly extracts `Foo`, making the same-parent bonus reachable. This is a documented divergence (deliberate Rust improvement) that affects edge resolution for codebases with two methods of the same name across classes.
- **Snapshots are the wire-format-of-record.** Wherever Rust diverges from Go (specific path errors, four-term filter validation, kind-filtered did-you-mean, exactly-one-of for diagram, TD-only mermaid direction, atomic save), the divergence is documented in code comments AND captured in the `.snap` baselines. Phase 3.7 made the Rust binary's wire format the canonical contract going forward; Go-byte-identity is no longer the gate.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| Binary starts and serves all 15 tools over stdio via rmcp | Met | `cargo build --release -p code-graph-mcp` produces an 11.3 MB binary; `tools/list` returns 15 tools verified via `crates/code-graph-mcp/tests/smoke.rs` |
| Parallel discovery walker honors `<root>/.code-graph.toml` and clamps to NumCPU with warnings | Met | `RootConfig::resolve_concurrency` (Phase 1.3) clamps; `analyze_codebase` surfaces clamp warnings in `AnalyzeResult.warnings`; `discover()` uses `WalkBuilder::threads(cfg.max_threads)` |
| Per-job rayon pool runs parsing concurrently with progress notifications reaching the client | Met | `index_directory` constructs a per-job pool, `pool.install(par_iter)`. Progress bridge is `ChannelProgressSink → tokio::mpsc → peer.notify_progress`. |
| Language-aware edge resolution prevents cross-language symbol collisions | Met | `SymbolIndex { by_name: HashMap<(Language, String), Vec<SymbolEntry>> }` enforces it structurally. Test `default_scope_aware_resolve_isolates_languages` asserts no Python `init` returned for C++ caller. |
| All 15 tools' wire format locked by snapshot tests | Met | 35 `.snap` files: 15 tools/list schema + 20 response bodies. 10 consecutive `cargo test` runs byte-stable. |
| Cache v2 persists and reloads correctly; atomic save survives crash injection | Met (with caveat) | Round-trip works; atomic save verified by API-contract test (corrupt pre-existing file gets cleanly replaced; stray `.tmp` doesn't poison final). True cross-process crash-injection requires forking a child and SIGKILL — out of scope for `cargo test`. The two API-contract tests prove the property the rename relies on. |
| Lint, format, and test gates green | Met | `cargo fmt --check` clean; `cargo clippy --workspace --all-targets -- -D warnings` clean; 380 tests pass; zero `#[allow]`; zero `unsafe` (workspace `unsafe_code = "forbid"`) |
| Brief mode + pagination envelope working | Met | `get_file_symbols` and `search_symbols` default to `brief=true`; `search_symbols` returns `{results, total, offset, limit}` |
| Did-you-mean on missing symbol/class | Met | `suggest_symbols` (kind-agnostic, used by `get_symbol_detail`/`get_callers`/`get_callees`); `suggest_class_symbols` (kind-filtered to {Class, Struct, Interface, Trait}, used by `get_class_hierarchy`/`generate_diagram(class=)`) |
| Watch stubs return `"watch mode not yet implemented in this build"` | Met | `watch_start`/`watch_stop` ship as stubs; Phase 4 replaces. Comment in `server.rs` reminds Phase 4 to restore the `require_indexed` gate. |

## Deviations

- **Wave 4 serialized instead of parallel.** Plan said 3.4 and 3.6 could run in parallel after 3.3. Practical reality: 3.4's `analyze_codebase` handler depends on 3.6's `Graph::save`/`load` API. Running 3.6 first eliminated the need to stub the cache path and fix it later. Same lesson as Phase 2's Wave 2 — file-overlap and API-readiness analysis should drive wave parallelism, not just `depends_on`.
- **`require_indexed` envelope fix mid-Phase 3.1.** First submission returned `Err(McpError)` and used `?` in handlers; this would have produced JSON-RPC protocol errors instead of tool-level `is_error: true` results. Quality-scanner caught it; fix was a 3.1 follow-up commit. Without that fix, all 13 future handlers would have been wrong.
- **`caller_id_parent` deliberately diverges from Go.** Go's `LastIndex(":")` lands on the second `:` of `::` and produces an empty parent — so same-parent bonus is dead code in Go. Rust's helper correctly extracts `Foo` from `file:Foo::bar`. Documented in code; the Phase 3.7 snapshots lock in the post-resolution edge format that includes the same-parent bonus when applicable. The pre-resolution Phase 1.6 parity (28/148 for fmt) is unchanged because the parser layer doesn't go through resolution.
- **Plan said snapshots are bound to `testdata/cpp` baseline of "17 symbols / 21 edges"; actual Phase 3 baseline is 18/21.** Phase 1's debrief already corrected this typo in the phase doc; Phase 3 carried it forward. The snapshot file `response_analyze_codebase_testdata_cpp.snap` shows 18/21/0.
- **Multiple Go-deviating error wordings.** The user's "Rust > Go-parity" directive (delivered mid-Phase 3.4) reframed several quality-scanner findings. Specifically:
  - Path errors: `"directory does not exist"` vs `"path is not a directory"` — Rust keeps `canonicalize`'s richer info.
  - `search_symbols` validation: 4-term message includes `language`.
  - `get_coupling` direction: `"invalid direction: <d>. Expected one of: …"` (matches Rust's own `invalid kind:` / `invalid format:` shapes).
  - `generate_diagram` exclusivity: `"exactly one of …"` enforces single-param dispatch instead of Go's silent precedence.
  - `class_hierarchy` did-you-mean: kind-filtered to class-likes only (a Function named `FooBar` won't show up).
  - `generate_diagram` mermaid direction: unified to TD (Go used BT for inheritance).
  All documented in code comments AND captured in `.snap` baselines.
- **Crash-injection test substituted with API-contract tests.** True crash-during-save requires forking a child process and SIGKILL'ing it — out of scope for `cargo test`. Two tests prove the API contract: `save_overwrites_existing_cache_atomically` (corrupt pre-existing file is replaced cleanly) and `save_does_not_disturb_unrelated_tmp_file` (stray `.tmp` from a prior crash doesn't poison the final file). Documented as a deviation in the phase doc.
- **Phase 3.6's `load_v1_cache_returns_false` test was misleading.** Quality-scanner pointed out the test name claimed Go-cache compat but the schema is incompatible (Go's `EdgeEntry` lacks JSON tags; Go's `Symbol` has no `language` field). Real Go caches return `Err(PersistError::Json)`, not `Ok(false)`. Renamed to `load_version_mismatch_returns_false` and documented honestly: the structured-version branch exists for a future Rust→Rust schema bump, while real Go caches fail loudly.

## Risks & Issues Encountered

- **`McpError` vs `CallToolResult` envelope confusion (Phase 3.1).** rmcp 1.5 distinguishes JSON-RPC protocol errors (`Err(McpError)`) from tool-level errors (`Ok(CallToolResult { is_error: Some(true), ... })`). First submission conflated them. Fix: change `require_indexed` to return `Result<(), CallToolResult>` so the early-return pattern produces the right wire shape. This is a load-bearing convention — all 13 query handlers depend on it.
- **`HashMap` iteration non-determinism in snapshot tests.** Three sources surfaced: `get_symbol_summary` (HashMap key order), `get_orphans` (walks nodes HashMap), `diagram_*` BFS (walks adj/radj HashMap). Each got a normalization helper at the test boundary: `sort_json` recursive, `sort_array_by_id`, `sort_diagram_edges` / `sort_mermaid_lines`. 10 consecutive runs verified byte-stable.
- **Concurrent `analyze_codebase` cache race in tests.** Tokio test infra runs multiple `#[tokio::test]` tests in parallel; the snapshot tests all `analyze_codebase`'d the same `testdata/cpp` directory; concurrent calls raced on the `.code-graph-cache.json.tmp` write and produced spurious "cache save failed" warnings ~10% of runs. Fixed by per-test `TempDir` copy of testdata. Each test gets its own root.
- **Vacuous test: callers/callees array order.** Initial implementation didn't sort the JSON arrays before snapshotting. Today the order is stable (single-file single-parse insertion order), but a future testdata edit that adds a second file would surface non-determinism. Quality-scanner flagged it as forward-proofing; fix was a tiny `sort_chains_by_symbol_id` helper plus regenerating the two snapshots.
- **Killed agent during Phase 3.3 dispatch.** Agent exited early after making one stray edit to `Cargo.toml` (changing the `repository` URL — unrelated to 3.3). Reverted and re-dispatched. The kill was a runtime issue, not a real failure mode of the work; the second agent completed the task cleanly.
- **`cargo-insta` CLI not installed for snapshot acceptance.** Pending `.snap.new` files needed manual `mv` to promote to `.snap`. Worked but is an awkward developer-experience gap. Future fix: document `cargo install cargo-insta` in the contributor README OR add a `make accept-snapshots` recipe that does the `mv` itself.

## Lessons Learned

- **The user's "Rust > Go-parity" directive reshaped the disposition criterion.** Before: "any deviation from Go is a finding to investigate." After: "Rust-idiomatic improvements that aren't snapshot contracts are wins; document them, lock them in snapshots, move on." Several quality-scanner Major findings became "approve as-is with a doc comment" once this principle was applied. The output is a more useful Rust binary than a faithful Go port would have been.
- **Wire-format snapshots are the contract; code comments are the rationale.** Once a `.snap` file exists, the wire format is locked. The accompanying code comment ("`generate_diagram` rejects multiple params instead of Go's silent precedence — see Phase 3.4 deviation") explains *why*. Future readers don't need to consult the design doc; the deviation is right next to the code.
- **Handler-body extraction was 100% the right call.** `server.rs` stays at 700 lines (mostly tool descriptors and Args schemas); 15 handler bodies live in 5 submodules organized by category. Adding a new handler in Phase 4 (when watch_start/watch_stop replace their stubs) is a one-file change.
- **`if let Err(r) = self.require_indexed() { return Ok(r); }` is the load-bearing pattern.** Conveying this to the implementer via a single sentence in the brief plus an example block was sufficient. Once 3.4 standardized it, 3.5 followed automatically. The pattern is worth elevating to a project-wide convention (CLAUDE.md or similar).
- **Per-test TempDir for fixture-based integration tests.** Avoids cache-write races and keeps the canonical `testdata/` tree pristine. The `IndexedFixture` helper makes this cheap. Worth elevating: any future test that touches `testdata/` should `copy_testdata` into a fresh `TempDir` first.
- **Quality-scanner Major findings on wire format are usually downgrade-able.** Of ~6 Majors across 8 task reviews, 4 were "Rust diverges from Go in error wording" findings. With the user's directive, these became "document the deliberate divergence, lock it in snapshots." 1 was a real correctness bug (`require_indexed` envelope) — fixed. 1 was a real correctness bug (`analyze_path_is_file_returns_not_a_directory` was originally claimed to mirror Go but didn't) — kept the Rust idiom, fixed the comment.
- **Kill-and-re-dispatch is fine for stalled agents.** The Phase 3.3 agent kill produced one stray edit and zero real progress. Reverting and re-dispatching with the same brief recovered without state issues. Worth knowing: there's no hidden state in agent dispatches that survives a kill.
- **The `tests/common/mod.rs` Rust idiom needs explicit setup.** Cargo treats `tests/*.rs` as independent crates by default; sharing helpers needs the special-cased `tests/common/mod.rs` path with `mod common;` declarations in each test file. Phase 3.7 hit this when the implementer initially duplicated helpers across files. The fix was straightforward but the convention is non-obvious.
- **`cargo doc` warnings on private-item links recur.** Same as Phase 2.7. Pattern: rustdoc emits `private_intra_doc_links` warning when a public item's doc comment links to a private item via `[`Foo::bar`]`. Fix: replace with bare backtick `` `Foo::bar` ``. Not a load-bearing concern but the structural verification gate catches it consistently.

## Impact on Subsequent Phases

- **Phase 4 (Watch mode + cross-compile + Go cutover)** picks up:
  - Replace `watch_start`/`watch_stop` stubs with `notify-debouncer-full`-backed implementations. **Must restore the `require_indexed` gate** that the stub deliberately skips (comment in `server.rs` reminds the implementer).
  - The `WatchHandle` placeholder type in `server.rs` becomes the real channel + task handle.
  - Watch-driven `reindex_file` must acquire `index_lock` to close the analyze-vs-watch race the design called out.
  - Cross-compile via `cargo-zigbuild` for 6 platforms.
  - **The Go cutover commit removes** `cmd/`, `internal/`, `go.mod`, `go.sum`, `Makefile`'s Go targets, the Phase 1 `testdata/cpp/MANIFEST.md` references in Go code (the manifest itself stays as Rust testdata). Phase 3.7's snapshots become the contract; no Go binary will exist to compare against post-cutover.

- **Phases 5-7 (Rust/Go/Python parsers)** inherit:
  - Working `LanguagePlugin` trait + `LanguageRegistry`.
  - Real default impls for `resolve_call` / `resolve_include` (Phase 3.3's `default_scope_aware_resolve` / `default_basename_resolve`). Each language plugin overrides for its semantics:
    - Rust: use-tree resolution (`use foo::bar::baz` brings `baz` into scope)
    - Go: package-path resolution (`pkg.FuncName` → import map)
    - Python: dotted import + `__init__.py` semantics
  - The `(Language, name)`-keyed `SymbolIndex` already structurally isolates languages; new plugins just register and the indexer handles the rest.
  - Phase 5's Rust dogfooding gate: index this workspace itself. The post-resolution edge format is now snapshot-locked, so dogfooding will produce stable counts.

- **Wire-format snapshots will need rebaselining whenever:**
  - A new tool is added (Phase 4 stubs replaced + new tools)
  - A new language gets the `language` filter (Phases 5-7)
  - Any handler's response shape changes deliberately
  Each rebaseline is a `cargo insta review` step, not silent drift.

- **The post-resolution edge counts** for `testdata/cpp` and the fmt clone are now snapshot-locked. Any future edit to `caller_id_parent`, `default_scope_aware_resolve`, or `default_basename_resolve` that changes resolution behavior will trigger snapshot diffs. This is the right gate — there's no silent drift path.

## Skill Opportunities

- **What you did repeatedly:** Run `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo doc --workspace --no-deps` after every commit and after every quality-scanner follow-up. 4 commands, ~3-5 invocations per task.
  - **Where it belongs:** A `Makefile` recipe `make rust-verify` (carry-forward from Phase 2 debrief — still unimplemented). Already have `rust-build`, `rust-test`, `rust-lint`, `rust-fmt-check`. Just needs the chained recipe.
  - **Why a skill:** Phase 4 onwards will run this gate dozens more times. Single-command gate cuts the friction.
  - **Rough shape:** `rust-verify: rust-fmt-check rust-lint rust-test rust-doc` — make's dependency graph handles the rest. Each sub-recipe already exists.

- **What you did repeatedly:** Capture pending insta snapshot files (`.snap.new`) and manually promote to `.snap` because `cargo-insta` CLI isn't installed.
  - **Where it belongs:** Either a `make accept-snapshots` recipe, or a contributor-readme line `cargo install cargo-insta` so `cargo insta accept` works directly.
  - **Why a skill:** Phase 4+ will produce more snapshots (new tools, watch mode behaviors). Friction every time.
  - **Rough shape:** `make accept-snapshots` recipe that runs `find target -name '*.snap.new' -exec sh -c 'mv $1 ${1%.new}' _ {} \;` — or just install `cargo-insta` workspace-wide.

- **What you did repeatedly:** Apply the "Rust > Go-parity" disposition to quality-scanner Major findings about wire-format wording.
  - **Where it belongs:** A note in the `planner:quality-scanner` agent prompt OR in `planner:implement`'s "process review findings" section. Something like: "When a Major finding is 'Rust diverges from Go in error/wire wording but the Rust form is more informative/idiomatic', the disposition is 'document the deviation in code comments + lock it in the snapshot baseline,' not 'revert to Go.' The Rust binary is the wire-format-of-record post-Phase-3.7."
  - **Why a skill:** Phase 4+ will hit the same disposition decision repeatedly. Codifying saves re-litigating each time.
  - **Rough shape:** Documentation update in the agent or implement-skill prompt.

- **What you did repeatedly:** Refactor the `if let Err(r) = self.require_indexed() { return Ok(r); }` pattern across 13 handlers.
  - **Where it belongs:** Already an established convention. Worth a one-line note in the project's CLAUDE.md or a future contribution guide: "Tool handlers must use `require_indexed`'s early-return pattern; never `?` the result. The `?` would propagate a JSON-RPC protocol error instead of the tool-level `is_error: true` envelope."
  - **Why a skill:** Phase 4 will add new handlers (real watch_start/watch_stop). Convention enforcement matters.
  - **Rough shape:** Documentation only. The pattern itself is enforced by the type system once `require_indexed` returns `Result<(), CallToolResult>`.

- **What you did repeatedly:** Run the killed-agent recovery: `git status` to see strays, `git checkout --` to revert, re-dispatch with the same brief.
  - **Where it belongs:** No skill needed. Single occurrence per phase at most.
  - **Verdict:** No action.

- **What you did repeatedly:** Build per-test `TempDir` fixtures via `copy_testdata` because the shared `testdata/` tree is unsafe to mutate.
  - **Where it belongs:** Already extracted to `tests/common/mod.rs` in Phase 3.7's follow-up. Future test files just `mod common;` and use the helpers.
  - **Why a skill:** Convention now established — no further action.
  - **Verdict:** No action.
