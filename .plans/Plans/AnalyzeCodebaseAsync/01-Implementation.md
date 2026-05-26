---
title: "Implementation"
type: phase
plan: AnalyzeCodebaseAsync
phase: 1
status: in-progress
created: 2026-05-23
updated: 2026-05-25
deliverable: "End-to-end working feature: `analyze_codebase_async` registered, `get_status` extended with `analyze_job` + `analyze_job_previous_terminal`, sync `analyze_codebase` rewritten to share the slot machinery (wire format unchanged). Agent can kick off → poll → read result. Tool descriptions and CLAUDE.md updated to document the polling pattern and grace-window semantics. Tests beyond what already covers the lifted code path land in Phase 2; this phase ships green via `cargo build` and the existing test suite (which must stay green after the sync refactor)."
tasks:
  - id: "1.1"
    title: "Add AnalyzeSlot / AnalyzeJob / JobMutableState types + analyze_slot field on ServerInner + visibility prerequisites"
    status: complete
    verification: "(a) `AnalyzeResult` in `handlers/analyze.rs:39` gains `Clone` in its derive list (`#[derive(Debug, Serialize, Clone)]`); all its fields (`Vec<String>`, primitives) already implement `Clone`, so this is mechanical. (b) `now_nanos_u64` (`handlers/analyze.rs:66`) and `format_unix_nanos_rfc3339` (`handlers/status.rs:168`) are made `pub(crate)` so the new module can call them. (c) `crates/code-graph-tools/src/server.rs` `ServerInner` struct gains `pub analyze_slot: PlRwLock<AnalyzeSlot>` (initialized to `AnalyzeSlot::default()` in `CodeGraphServer::new`). (d) New module `crates/code-graph-tools/src/analyze_job.rs` declared in `lib.rs` defines `AnalyzeSlot { current: Option<Arc<AnalyzeJob>>, previous_terminal: Option<Arc<AnalyzeJob>> }`, `AnalyzeJob { job_id, path, force, started_at, state: PlRwLock<JobMutableState> }`, `JobMutableState { status, finished_at, progress, progress_total, progress_message }`, `enum JobStatus { Running, Completed(AnalyzeResult), Failed(String) }`. No atomics on `AnalyzeJob` — single inner `PlRwLock<JobMutableState>` is the only synchronization primitive for mutable state, per Design Decision 7. (e) `AnalyzeJob::new_running(job_id, path, force, started_at)` constructor used by both sync and async handlers. (f) `cargo build -p code-graph-tools` clean."
  - id: "1.2"
    title: "Lift analyze pipeline into run_analyze_job worker"
    status: complete
    verification: "Existing pipeline body in `crates/code-graph-tools/src/handlers/analyze.rs` (config load, cache fast-path, spawn_blocking with index pipeline, forwarder, install graph, save_cache) is refactored into `async fn run_analyze_job(inner: Arc<ServerInner>, job: Arc<AnalyzeJob>)` taking the `AnalyzeJob` Arc and writing terminal state (Completed with `AnalyzeResult` OR Failed with error string) into `job.state`. **Specifically: `RootConfig::load`, `cfg.resolve_concurrency()`, `paths::canonicalize`, and the `is_dir` check are MOVED from their pre-lock positions in the existing handler into the worker** so disk-touching validation can fail into `JobStatus::Failed`. Pre-rotation cheap validation (empty path) stays in the handler — see Task 1.3/1.4. The worker acquires `index_lock.lock().await` (NOT `try_lock`) at its start — Design Decision 1. Existing eprintln per-phase logs preserved verbatim. Existing forwarder behavior (commit `0d32b55` time-bound) preserved; the `report()` call additionally writes `progress` / `progress_total` / `progress_message` into `job.state` via the fan-out path (Design Decision 8). Disk-touching validation errors (non-existent dir, file-not-dir, malformed toml, canonicalize fails) are surfaced as `JobStatus::Failed` in the job state — NOT as the worker's return value. `cargo build` clean."
    depends_on: ["1.1"]
  - id: "1.3"
    title: "Rewrite sync analyze_codebase handler to use slot protocol + inline-await worker"
    status: complete
    verification: "`crates/code-graph-tools/src/handlers/analyze.rs::analyze_codebase` is rewritten to: (a) **pre-rotation validation** — empty-path check returns `tool_error(\"'path' is required\")` immediately, NO slot rotation, NO job written (preserves slot cleanliness for failing-path tests; matches today's wire format byte-identical); (b) acquire `inner.analyze_slot.write()` write guard; (c) check `current.status` via `current.state.read()` — if `Running`, drop the write guard and return `tool_error(\"indexing already in progress\")` (byte-identical to today's snapshot-locked wording); (d) rotate (move existing terminal `current` into `previous_terminal`), install new `Arc<AnalyzeJob>` with `JobStatus::Running` as `current`, drop the write guard; (e) `run_analyze_job(inner, job_arc).await` inline; (f) read the terminal `JobStatus` from `job.state.read()` — on `Completed(result)` return `tool_success_json(&result)`, on `Failed(msg)` return `tool_error(msg)`. The wire format of every success and every error response is byte-identical to today's behavior (snapshot suite is the regression gate). Index_lock acquisition moves into the worker per Design Decision 1; the sync handler itself no longer touches `index_lock` directly. `cargo build` clean; all 9 existing `handlers::analyze::tests::*` pass with the test `analyze_concurrent_call_returns_indexing_in_progress` updated to hold the SLOT externally (assign `analyze_slot.current` to a synthetic Running job) instead of `index_lock` — wire assertion unchanged. Tests `analyze_missing_path_errors`, `analyze_nonexistent_directory_errors`, and `analyze_path_is_file_errors` continue to produce their existing wire errors; the first stays at the handler (empty-path pre-rotation check), the latter two now flow through the worker as `JobStatus::Failed`, with the sync handler unwrapping that into the same `tool_error` strings."
    depends_on: ["1.2"]
  - id: "1.4"
    title: "Add analyze_codebase_async tool handler + arg struct + tool registration"
    status: planned
    verification: "`crates/code-graph-tools/src/handlers/analyze.rs` gains `pub async fn analyze_codebase_async(inner: Arc<ServerInner>, path_raw: String, force: bool) -> CallToolResult` implementing the kickoff protocol: (a) acquire `analyze_slot.write()`; (b) if `current` is `Running`, build response with `existing: true` carrying the existing job_id and return; (c) otherwise validate args (path non-empty — args validation errors return `tool_error` with same wording as sync, NO slot rotation on validation failure); (d) rotate, install new Running job, drop guard; (e) `tokio::spawn(run_analyze_job(inner, job_arc))`; (f) return success JSON `{ job_id, status: \"running\", started_at, existing: false, note }`. New arg struct `AnalyzeCodebaseAsyncArgs { path: String, force: Option<bool> }` parallel to existing `AnalyzeCodebaseArgs`. New tool method `async fn analyze_codebase_async(&self, ...) -> Result<CallToolResult, McpError>` on `CodeGraphServer` in `crates/code-graph-tools/src/server.rs` registered via `#[tool(description = …)]` — description landed in this task per Key Decision 2 (operationally explains poll pattern, names the response shape, names the grace-window semantics, names the `existing: true` semantic). `job_id` is a 20-char zero-padded decimal nanosecond timestamp from existing `now_nanos_u64()` (Decision 6). `started_at` is RFC3339 via existing `format_unix_nanos_rfc3339()` helper. `tool_count()` returns 19 (was 18). `cargo build` clean."
    depends_on: ["1.3"]
  - id: "1.5"
    title: "Extend StatusResult with analyze_job + analyze_job_previous_terminal fields + tool description + CLAUDE.md"
    status: planned
    verification: "`crates/code-graph-tools/src/handlers/status.rs::StatusResult` gains two optional fields: `pub analyze_job: Option<AnalyzeJobView>` and `pub analyze_job_previous_terminal: Option<AnalyzeJobView>` (both `#[serde(skip_serializing_if = \"Option::is_none\")]` … NO — see below). New type `AnalyzeJobView { job_id, status, path, force, started_at, finished_at, progress, progress_total, progress_message, error: Option<String>, result: Option<AnalyzeResult> }`. `status` serializes as `\"running\"` / `\"completed\"` / `\"failed\"` (lowercase string, not the enum tag). `error` populated only on Failed; `result` populated only on Completed. **Wire compatibility note:** per CLAUDE.md's existing `Page<T>` envelope contract pattern, new optional fields use `serde(default)` and the JSON SHOULD emit `null` (not absent) for unset values so clients can distinguish \"no analyze ever\" from \"missing field due to old server\" — match the project's convention by NOT using `skip_serializing_if` (emit `\"analyze_job\": null` explicitly). Conversion from `AnalyzeJob` + `JobMutableState` to `AnalyzeJobView` is a single helper `AnalyzeJobView::from(&AnalyzeJob)` that acquires `job.state.read()` once and snapshots. `get_status` handler reads `analyze_slot.read()`, Arc-clones the two job slots, drops the slot lock, builds two `AnalyzeJobView`s outside the slot lock. `get_status` tool description updated to mention the new fields and the poll-pattern hint. CLAUDE.md updated: tool count 18 → 19; new `analyze_codebase_async` row in the MCP tools table; `get_status` row mentions `analyze_job` and `analyze_job_previous_terminal`; new paragraph under the existing `MCP_TOOL_TIMEOUT` paragraph that async kickoff is the structural workaround; new paragraph under the watch handler section that sync `analyze_codebase` now waits for an in-flight watch reindex instead of erroring (Decision 9). `cargo build` clean; existing `get_status` tests stay green (response shape change is additive at the wire — old fields untouched)."
    depends_on: ["1.4"]
tags: [analyze, mcp, async, get_status, single-flight]
---

# Phase 1: Implementation

## Overview

End-to-end working feature: slot types, worker lift, sync refactor (same wire), async tool, extended `get_status`, tool descriptions, and CLAUDE.md updates — landed in dependency order so each task leaves the workspace in a buildable state. Phase 2 adds the race coverage, integration test, snapshot rebaseline, and the structural verification pass.

## 1.1: Add AnalyzeSlot / AnalyzeJob / JobMutableState types + analyze_slot field on ServerInner + visibility prerequisites

### Subtasks
- [x] **Prerequisite — `AnalyzeResult` derive change.** Edit `handlers/analyze.rs:38` from `#[derive(Debug, Serialize)]` to `#[derive(Debug, Serialize, Clone)]`. Required because `JobStatus::Completed(AnalyzeResult)` is cloned out of the inner lock during `AnalyzeJobView::from_job` snapshotting (Task 1.5). All `AnalyzeResult` fields (`u32`s, `String`, `Vec<String>`) already implement `Clone`, so the derive is mechanical.
- [x] **Prerequisite — helper visibility.** Change `fn now_nanos_u64()` (`handlers/analyze.rs:66`) to `pub(crate) fn`. Change `fn format_unix_nanos_rfc3339(nanos: u64) -> String` (`handlers/status.rs:168`) to `pub(crate) fn`. Both are used by `AnalyzeJob::new_running` (timestamp) and the async handler's response builder (RFC3339 formatting) — without this, the new module can't compile.
- [x] Decide module placement: new `crates/code-graph-tools/src/analyze_job.rs` module declared in `lib.rs`, OR inline in `handlers/analyze.rs`. Recommend the dedicated module — `analyze_job.rs` is reusable by the watch handler if a future change wants to read job state, and keeps `handlers/analyze.rs` focused on the handler bodies.
- [x] Define `pub struct AnalyzeSlot` with `current: Option<Arc<AnalyzeJob>>` and `previous_terminal: Option<Arc<AnalyzeJob>>`. Derive `Default` (both fields default to `None`).
- [x] Define `pub struct AnalyzeJob` with immutable fields (`job_id: String`, `path: String`, `force: bool`, `started_at: u64`) and mutable field (`state: parking_lot::RwLock<JobMutableState>`). NO `Clone` derive — held only via `Arc<AnalyzeJob>`.
- [x] Define `pub struct JobMutableState` with `status: JobStatus`, `finished_at: Option<u64>`, `progress: u32`, `progress_total: u32`, `progress_message: String`. Derive `Default` (status defaults to `JobStatus::Running`, progress fields to 0, message to empty string).
- [x] Define `pub enum JobStatus { Running, Completed(AnalyzeResult), Failed(String) }`. `AnalyzeResult` is the existing type in `handlers/analyze.rs` — make sure it's `pub` enough to use here (it's already `pub`).
- [x] Add constructor `AnalyzeJob::new_running(job_id: String, path: String, force: bool, started_at: u64) -> Arc<AnalyzeJob>` returning the Arc directly (callers never hold the bare struct).
- [x] Add helper `AnalyzeJob::is_terminal_status(state: &JobMutableState) -> bool` returning `true` for `Completed`/`Failed`, used by the slot rotation logic in Task 1.3/1.4.
- [x] Add `pub analyze_slot: PlRwLock<AnalyzeSlot>` field to `ServerInner` in `crates/code-graph-tools/src/server.rs`. Initialize to `PlRwLock::new(AnalyzeSlot::default())` in `CodeGraphServer::new`.
- [x] Module-level doc comment on `analyze_job.rs` summarizing the slot model and pointing at `Designs/AnalyzeCodebaseAsync/README.md` for the full design.
- [x] `cargo build -p code-graph-tools` clean.

### Notes
Type-level only — no behavior change. The slot exists but no handler reads or writes it yet. This makes Task 1.2/1.3/1.4 strictly additive at the type layer.

## 1.2: Lift analyze pipeline into run_analyze_job worker

### Subtasks
- [ ] Identify the existing pipeline body in `handlers/analyze.rs::analyze_codebase` — everything between the existing `try_lock` guard acquisition (line 56) and the final `tool_success_json(&result)` return.
- [ ] Refactor into `pub async fn run_analyze_job(inner: Arc<ServerInner>, job: Arc<AnalyzeJob>)` — function takes the job by Arc, returns nothing (`()`), writes terminal state into `job.state` on completion.
- [ ] **Move disk-touching validation into the worker.** `RootConfig::load`, `cfg.resolve_concurrency()`, `paths::canonicalize`, and the `is_dir` check currently appear in the handler BEFORE `index_lock.try_lock`; relocate all four into the worker AFTER `index_lock.lock().await`. Reachability of `failed_job_surfaces_error_in_get_status` (Task 2.3) depends on this move — the malformed-TOML error must surface as `JobStatus::Failed`, which only happens if `RootConfig::load` runs inside the worker. Observable ordering changes: today TOML errors are detected before the worker starts; after this change they surface after the worker acquires the lock. This is correct under the new model (slot is the gate, not `index_lock`) but a non-obvious behavior shift.
- [ ] Worker acquires `inner.index_lock.lock().await` (NOT `try_lock`) at its start — Design Decision 1. The slot serializes all analyses; only watch contends, and watch reindexes are bounded.
- [ ] Disk-touching validation errors (canonicalize fails, non-existent dir, file-not-dir, malformed toml, etc.) → write `JobStatus::Failed(msg)` into `job.state`, set `finished_at`, return. These no longer escape the worker as a `CallToolResult` (the handler will surface them by reading `job.state` after `await`). Empty-path validation lives in the handler (NOT in the worker) — see Task 1.3 and 1.4.
- [ ] Indexing errors (parse panics, spawn_blocking failures) → write `JobStatus::Failed(msg)`, set `finished_at`, return.
- [ ] Success path → write `JobStatus::Completed(AnalyzeResult { files, symbols, edges, root_path, warnings })`, set `finished_at`, return.
- [ ] Inside the existing `ChannelProgressSink::report` (or equivalent indexer sink call site), add fan-out write to `job.state`:
  ```rust
  {
      let mut s = job.state.write();
      s.progress = progress;
      s.progress_total = total;
      s.progress_message = message.to_string();
  }
  ```
  (The existing `try_send` to the mpsc stays unchanged — Decision 8 fan-out: both sinks fire on every `report()` call.) The cleanest place to add this fan-out is inside `ChannelProgressSink::report` itself; either give the sink an `Option<Arc<AnalyzeJob>>` field, or create a new wrapper sink `JobAwareProgressSink { inner: ChannelProgressSink, job: Arc<AnalyzeJob> }` that delegates. Recommend the wrapper — keeps `ChannelProgressSink` unchanged for tests that use it standalone.
- [ ] Preserve every existing `eprintln!("[code-graph] phase: …")` log verbatim — operators rely on these for debugging.
- [ ] Preserve the existing forwarder time-bound fix from commit `0d32b55` exactly — do not refactor the `tokio::time::timeout` calls.
- [ ] Worker drops `index_lock` guard (via scope exit) at the end. The slot's `current.state` transition to terminal must happen BEFORE the lock is released so an agent polling immediately after `lock` release observes the terminal state. Order: write terminal state → return → guard drops via Drop.
- [ ] `cargo build -p code-graph-tools` clean.

### Notes
The worker has no public error type — all error paths terminate by writing `JobStatus::Failed` into the job. This unifies the sync and async error handling: both handlers read the terminal status from the job after the worker finishes (sync: inline await; async: agent polls).

## 1.3: Rewrite sync analyze_codebase handler to use slot protocol + inline-await worker

### Subtasks
- [x] Replace the existing `analyze_codebase` body (`handlers/analyze.rs`) with the slot protocol:
  - **Pre-rotation cheap validation.** Empty-path check first: `if path_raw.is_empty() { return tool_error("'path' is required"); }` — returns immediately, NO slot touch. This preserves the existing test `analyze_missing_path_errors` AND keeps the slot clean for tests that follow (test isolation). Disk-touching validation (canonicalize, is_dir, RootConfig::load) stays inside the worker per Task 1.2 — so a non-existent-dir error WILL rotate the slot to a Failed job, which is acceptable because the existing tests for that error don't inspect slot state.
  - Acquire `inner.analyze_slot.write()`.
  - If `current.is_some()` and `matches!(current.state.read().status, JobStatus::Running)` → drop the slot guard and return `tool_error("indexing already in progress")` (snapshot-locked wording, byte-identical to today).
  - Otherwise, rotate: if `current.is_some()` (must be terminal at this point), `slot.previous_terminal = slot.current.take()`. Build new `Arc<AnalyzeJob>` via `AnalyzeJob::new_running(...)` with a fresh `job_id` (20-char zero-padded `now_nanos_u64()`). Set `slot.current = Some(Arc::clone(&job))`. Drop the slot guard.
  - `run_analyze_job(inner, Arc::clone(&job)).await`.
  - Read `job.state.read()` → match `status`: `JobStatus::Completed(result) => tool_success_json(&result)`, `JobStatus::Failed(msg) => tool_error(msg.clone())`, `JobStatus::Running => unreachable!()` (the worker always writes a terminal status before returning).
- [x] The handler signature stays `pub async fn analyze_codebase(inner: Arc<ServerInner>, path_raw: String, force: bool, peer: Option<Peer<RoleServer>>, progress_token: Option<ProgressToken>) -> CallToolResult` — peer/token are passed through to the worker (the worker still drives the forwarder for client-side progress notifications when peer is `Some`).
- [x] Verify the wire format is byte-identical: success returns the same `AnalyzeResult` JSON as today; every error variant produces the same wording (the snapshot suite is the regression gate — those tests stay unchanged in this task).
- [x] Update the test `handlers::analyze::tests::analyze_concurrent_call_returns_indexing_in_progress`: today it does `let _held = inner.index_lock.try_lock().expect(...)`. Replace with writing the slot directly: build a synthetic `Arc<AnalyzeJob>` with `JobStatus::Running`, set `inner.analyze_slot.write().current = Some(job)`, then call sync `analyze_codebase` and assert the same error wording. The wire assertion is preserved; only the test's hold-mechanism changes (because per Design Decision 9, sync no longer errors on `index_lock` contention).
- [x] Run `cargo test -p code-graph-tools --lib handlers::analyze` — all 9 tests must pass.
- [x] Run `cargo test -p code-graph-tools` and `cargo test --workspace` to confirm no integration test regressed.

### Notes
The `unreachable!()` for `JobStatus::Running` after the worker's `await` is correct: the worker always writes a terminal status before its async function returns. If a future change adds a path where the worker can return without writing terminal state, that's a bug and panicking surfaces it immediately rather than letting `tool_success_json` serialize garbage.

## 1.4: Add analyze_codebase_async tool handler + arg struct + tool registration

### Subtasks
- [ ] Add `pub async fn analyze_codebase_async(inner: Arc<ServerInner>, path_raw: String, force: bool) -> CallToolResult` to `handlers/analyze.rs`. (No peer/token — async mode has no client-side progress channel.)
- [ ] Handler body:
  - Acquire `inner.analyze_slot.write()`.
  - If `current.is_some()` and `matches!(current.state.read().status, JobStatus::Running)`:
    - Capture `existing_id = current.job_id.clone()` and `existing_started_at_rfc3339`.
    - Drop the slot guard.
    - Build response: `{ job_id: existing_id, status: "running", started_at: existing_started_at_rfc3339, existing: true, note: "analyze already in progress — args ignored; poll get_status for progress" }`.
    - Return via `tool_success_json`.
  - Otherwise validate args FIRST (path non-empty — quick checks that don't require disk I/O). Args validation errors → return `tool_error(msg)`, NO slot rotation, NO job written. (Disk-touching validation like canonicalize-path-fails happens inside the worker per the unified error path from Task 1.2.)
  - Rotate: move terminal `current` to `previous_terminal` (if any); build new `Arc<AnalyzeJob>` via `AnalyzeJob::new_running(...)`; install as `current`.
  - Drop slot guard.
  - `tokio::spawn(run_analyze_job(Arc::clone(&inner), Arc::clone(&job)))` — return value (`JoinHandle`) is dropped, detaching the task.
  - Return response with `existing: false` carrying the new job_id and started_at.
- [ ] Add `#[derive(Debug, Deserialize, JsonSchema)] pub struct AnalyzeCodebaseAsyncArgs { pub path: String, pub force: Option<bool> }` to `server.rs` next to the existing `AnalyzeCodebaseArgs`.
- [ ] Add tool method on `CodeGraphServer` in `server.rs`:
  ```rust
  #[tool(description = "...")]
  async fn analyze_codebase_async(
      &self,
      Parameters(args): Parameters<AnalyzeCodebaseAsyncArgs>,
  ) -> Result<CallToolResult, McpError> {
      Ok(handlers::analyze::analyze_codebase_async(
          self.inner.clone(),
          args.path,
          args.force.unwrap_or(false),
      ).await)
  }
  ```
- [ ] Tool description body — write per the "Agent-facing tool descriptions" lens in CLAUDE.md. Must include: args + defaults; the immediate-return semantic (`< 1KB response, status: "running"`); the poll pattern (call `get_status`, read `analyze_job` field, look for `status: "completed"` or `status: "failed"`); the grace-window semantic (`analyze_job_previous_terminal` holds the previous terminal if the slot has rotated); the `existing: true` semantic; and the explicit hint that args of a duplicate call are ignored (Decision 3). Length: comparable to other complex tool descriptions in the same file (e.g., `get_file_symbols`'s description).
- [ ] Add the new tool name to the `tool_router_contains_every_expected_name` test's expected set in `server.rs` (or wherever the expected-tool-name list lives). Update `tool_count` assertions if any test pins the count to 18 (update to 19). Update any "18 registered tools" doc comment in `tests/snapshot_tools_list.rs` to "19".
- [ ] Note: the corresponding per-tool snapshot test `tools_list_analyze_codebase_async` and its `.snap` file are added in Task 2.4 (snapshot rebaseline ritual) — this task is the compile-time / runtime regression update only.
- [ ] `cargo build -p code-graph-tools` clean.
- [ ] `cargo test -p code-graph-tools tool_count` (or the equivalent existing test) passes with new count.

### Notes
The async handler does its own arg validation for the cheap checks (path non-empty) BEFORE rotating the slot. Disk-touching validation (canonicalize-path-fails, malformed toml) goes through the worker — that's a deliberate split to keep the kickoff handler's response time bounded by O(1) work, not by O(disk-stat) work that could be slow on a flaky NFS mount.

## 1.5: Extend StatusResult with analyze_job + analyze_job_previous_terminal fields + tool description + CLAUDE.md

### Subtasks
- [ ] Add to `handlers/status.rs::StatusResult`:
  ```rust
  pub analyze_job: Option<AnalyzeJobView>,
  pub analyze_job_previous_terminal: Option<AnalyzeJobView>,
  ```
  Do NOT use `serde(skip_serializing_if = "Option::is_none")`. The existing precedent in `StatusResult` (`pub index_force_built: Option<bool>` at `status.rs:90` has no `skip_serializing_if` attribute and therefore serializes as `null` when `None`) is to emit `null` explicitly so clients can distinguish "no job ever" from "missing field due to old server". Match that precedent.
- [ ] Define `pub struct AnalyzeJobView` with the 11 fields from the design (`job_id`, `status`, `path`, `force`, `started_at`, `finished_at`, `progress`, `progress_total`, `progress_message`, `error`, `result`). `status` is `String` ("running"/"completed"/"failed"). `finished_at` is RFC3339 `Option<String>`. `error` is `Option<String>`. `result` is `Option<AnalyzeResult>`.
- [ ] Implement conversion: `impl AnalyzeJobView { pub fn from_job(job: &AnalyzeJob) -> Self }` — acquires `job.state.read()` once, snapshots into the view, drops the lock. The match on `JobStatus` populates `status` + `error` + `result` correctly (`Running` → status="running", error=None, result=None; `Completed(r)` → status="completed", error=None, result=Some(r.clone()); `Failed(e)` → status="failed", error=Some(e.clone()), result=None).
- [ ] Update `handlers::status::get_status` body:
  - After building the existing `StatusResult` fields, acquire `inner.analyze_slot.read()`.
  - Arc-clone `current` and `previous_terminal`.
  - Drop the slot lock.
  - Build `AnalyzeJobView` for each (via `from_job`) outside the slot lock.
  - Set the new fields on `StatusResult` before `tool_success_json(&result)`.
- [ ] Update `get_status` tool description (the `#[tool(description = …)]` on `server.rs`) to mention the new fields and the poll-pattern hint. Specifically: "When an analyze is in flight or recently terminated, `analyze_job` carries `{ status, progress, progress_total, result | error, ... }`. Poll this tool while `status == \"running\"`; read `result` once `status == \"completed\"`. If you've kicked off a new analyze before reading the previous terminal, `analyze_job_previous_terminal` carries the prior result for one grace-window kickoff."
- [ ] CLAUDE.md updates (`/home/daniel/Development/Code/code-graph-mcp/CLAUDE.md`):
  - Tool count: `18` → `19` (every place it appears — search the file).
  - Add `analyze_codebase_async` row in the MCP tools table (under the "Indexing" group, alongside `analyze_codebase`).
  - Update the existing `analyze_codebase` row in the MCP tools table to note the Decision 9 behavior change: "Under watch contention, sync `analyze_codebase` now awaits the watch reindex (typically ms) rather than returning an error."
  - Update `get_status` row in the MCP tools table to mention `analyze_job` and `analyze_job_previous_terminal`.
  - Add a new paragraph under the existing `MCP_TOOL_TIMEOUT` bullet (in "Known cross-cutting limitations") noting that `analyze_codebase_async` is the structural workaround. Phrasing: "Clients hitting `MCP_TOOL_TIMEOUT` on long analyses should prefer `analyze_codebase_async` + poll `get_status` — every individual tool call is sub-second, so the per-call timer never fires."
  - CLAUDE.md has no dedicated "watch handler section" — the watch tooling appears as a row in the MCP tools table and the Windows watch-mode caveat lives in "Known cross-cutting limitations". Place the Decision 9 note as a new bullet in "Known cross-cutting limitations" titled "Sync `analyze_codebase` now waits for in-flight watch reindex", briefly stating the change and the rationale (slot is the gate; watch contention is bounded).
  - Add an entry to the Response shapes section describing `AnalyzeJobView` and how the grace-window rotation works.
- [ ] `cargo build` clean.
- [ ] Existing `get_status` tests still pass (the response shape change is additive — old fields untouched).

### Notes
The conversion helper acquires `job.state.read()` exactly once per job per `get_status` call. That's two read-lock acquisitions total per poll (one per slot field) — utterly negligible. The lock-acquisition-then-clone-into-view pattern means JSON serialization happens with NO locks held, preserving the existing handler invariant ("no locks held across `tool_success_json`" — see `handlers/status.rs:93`).

## Acceptance Criteria
- [ ] `cargo build -p code-graph-tools` clean.
- [ ] `cargo build --workspace` clean.
- [ ] `cargo test --workspace` passes (all existing tests still pass; one test updated per Task 1.3; no NEW tests added in this phase — those land in Phase 2).
- [ ] Tool count is 19 (verified by `tool_count()` assertions in existing tests).
- [ ] Manual smoke test: build the binary, point it at a small fixture corpus, call `analyze_codebase_async` via raw MCP request, verify the response time is < 1s; poll `get_status` and verify `analyze_job.status` progresses `running` → `completed`; verify `analyze_job.result` carries the expected `files`/`symbols`/`edges` counts.
- [ ] CLAUDE.md changes land in the same commit-set as the implementation, not in a follow-up doc-only commit (per the "Documentation read cold" lens — the doc must read coherently with the code at every commit).
