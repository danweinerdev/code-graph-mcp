---
title: "Testing"
type: phase
plan: AnalyzeCodebaseAsync
phase: 2
status: in-progress
created: 2026-05-23
updated: 2026-05-26
deliverable: "Race-tight unit coverage for the slot protocol (concurrent kickoffs, sync+async exclusion, rotation, terminal preservation, failure paths), a deterministic progress-reporting test, an end-to-end integration test for the agent's full kickoff→poll→query flow, rebaselined `get_status` snapshots, and a clean `make lint` / `make snapshot-clean` / full-workspace test pass. After this phase, the feature is shippable."
tasks:
  - id: "2.1"
    title: "Lifecycle + shape unit tests"
    status: complete
    verification: "Five tests added to `crates/code-graph-tools/src/handlers/analyze.rs` test module: (a) `async_kickoff_returns_immediately_with_running_job` — call returns in < 100ms on a small dir; response carries `status: \"running\"`, `existing: false`, a non-empty `job_id`. (b) `async_kickoff_then_poll_completes` — kickoff async, loop on `get_status` with 50ms `tokio::time::sleep` cadence (bounded to 5s total to catch hangs), assert eventual `status: \"completed\"` with `result.files == expected`. (c) `sync_analyze_populates_slot_with_completed` — after sync `analyze_codebase` returns, assert `inner.analyze_slot.read().current.as_ref().unwrap().state.read()` has `JobStatus::Completed(_)` with matching counts. (d) `get_status_with_no_analyze_returns_null_job_fields` — fresh server, `analyze_job` and `analyze_job_previous_terminal` both serialize as `null`. (e) `get_status_completed_carries_full_analyze_result` — kickoff, poll to Completed, assert `analyze_job.result` has `files`, `symbols`, `edges`, `root_path`, `warnings`. All five tests use small synthetic fixtures (1–3 files); none take > 1s wall time. `cargo test -p code-graph-tools --lib handlers::analyze` shows the 9 existing + 5 new tests passing."
    depends_on: []
  - id: "2.2"
    title: "Single-flight race tests"
    status: complete
    verification: "(0) Prerequisite: `test_recording_plugin.rs` gains a `SLEEP_PER_PARSE_MS: AtomicU64` knob and a `std::thread::sleep` call in `parse_file` gated by it. Four tests added to the same module exercising the slot's atomicity: (a) `concurrent_async_kickoffs_only_one_spawns_worker` — two `tokio::spawn`ed handler calls gated by `tokio::sync::Barrier::new(2)`, both calling `analyze_codebase_async` on the same empty-slot server. Assert: exactly one response has `existing: false`, exactly one has `existing: true`, both responses carry the SAME `job_id`. Determinism comes from the barrier — both tasks reach the slot write attempt simultaneously, but the `PlRwLock` write serializes them so the second observes the first's write. (b) `async_duplicate_kickoff_after_first_started_returns_existing_job_id` — sequential: kickoff, `tokio::task::yield_now().await`, kickoff again; assert second has `existing: true` and same `job_id` as first. (c) `async_kickoff_blocks_sync_analyze` — `SLEEP_PER_PARSE_MS=50` on a 5-file fixture (≥ 250ms in-progress window), kickoff async, immediately call sync `analyze_codebase`, assert sync returns `tool_error(\"indexing already in progress\")` byte-identical. (d) `sync_kickoff_blocks_async_kickoff` — `SLEEP_PER_PARSE_MS=50` on a 20-file fixture; spawn sync in a tokio task; spin-yield with 500ms bound until `analyze_slot.read().current.is_some_and(|j| status == Running)`; then call async; assert `existing: true` with sync's `job_id`. All tests reset the knob to 0 via a Drop guard. `cargo test -p code-graph-tools --lib race` (or matching pattern) passes deterministically across 10 consecutive runs."
    depends_on: ["2.1"]
  - id: "2.3"
    title: "Slot rotation, failure-path, and deterministic progress tests"
    status: complete
    verification: "Five tests covering the rotation rules and failure surfacing: (a) `terminal_job_rotates_to_previous_on_next_kickoff` — kickoff (T1), poll to Completed, kickoff (T2), assert `previous_terminal == T1` and `current == T2` Running. (b) `two_back_to_back_analyses_lose_oldest_terminal` — kickoff (T1) → Completed → kickoff (T2) → Completed → kickoff (T3), assert `previous_terminal == T2` and T1's `job_id` no longer appears in the slot. (c) `failed_job_surfaces_error_in_get_status` — point analyze at a tempdir with a malformed `.code-graph.toml` (`[discovery\\nmax_threads = nope\\n`), kickoff async, poll until `status: \"failed\"`, assert `error` contains `\"failed to parse .code-graph.toml\"` byte-identical to the existing sync error wording. (d) `failed_job_rotates_to_previous_terminal` — Failed counts as terminal: kickoff failed (T1), kickoff (T2), assert `previous_terminal == T1` (Failed) and `current == T2`. (e) `progress_increments_during_indexing` — generate 20 small fixture files using a custom `LanguagePlugin` wrapping `CppParser` that sleeps 10ms per `parse_file` call (or use `test_recording_plugin` parameterized with a sleep). Kickoff async. In a polling loop, sample `analyze_job.progress` every 30ms while `status == \"running\"`, recording values. Assert: sequence is monotonically non-decreasing AND ≥ 3 distinct values observed (NOT just 0 → final). Determinism comes from the controlled per-file sleep, not from runtime timing — the loop has 20 × 10ms = 200ms of guaranteed in-progress window, easily long enough for several 30ms polls to land mid-run. `cargo test` shows all five passing."
    depends_on: ["2.1"]
  - id: "2.4"
    title: "Integration test + get_status snapshot rebaseline"
    status: complete
    verification: "(a) New integration test file `crates/code-graph-tools/tests/analyze_async_lifecycle.rs`: builds a server with a real (small) corpus, calls `analyze_codebase_async` via the handler entry point, polls `get_status` until `status: \"completed\"`, then calls `get_file_symbols` and asserts the symbol count matches a parallel sync `analyze_codebase` run on the same fixture. (b) Rebaseline `snapshot_tools_list__tools_list_get_status.snap` to include the new `analyze_job: null` / `analyze_job_previous_terminal: null` fields. (c) Add a new `#[test] fn tools_list_analyze_codebase_async()` in `crates/code-graph-tools/tests/snapshot_tools_list.rs` mirroring the existing per-tool pattern; create the corresponding `.snap` file `snapshot_tools_list__tools_list_analyze_codebase_async.snap` via `cargo insta review`. (d) Update the `snapshot_tools_list.rs` module-level doc comment from \"18 registered tools\" to \"19 registered tools\". (e) Run `make snapshot-clean` — passes (no orphan `*.snap.new`). (f) Snapshot diffs reviewed manually: only the additive fields in `get_status`, and only the new per-tool snapshot file. Commit all snapshot changes alongside the implementation in the same commit (project convention; pre-commit hook enforces snapshot-clean)."
    depends_on: ["2.1", "2.2", "2.3"]
  - id: "2.5"
    title: "Structural verification + full-workspace pass"
    status: planned
    verification: "(a) `make lint` (= `cargo clippy --workspace --all-targets -- -D warnings`) clean. (b) `make fmt-check` (= `cargo fmt --all --check`) clean. (c) `make test` (= `cargo test --workspace`) green — all 9 + 14 = 23 analyze handler tests pass, plus all existing workspace tests, plus the new integration test. (d) `make snapshot-clean` passes. (e) Per `shared/languages/rust.md`: no new `unsafe` was introduced (the design forbids it; verify by grepping `crates/code-graph-tools/src/analyze_job.rs` and the modified handlers for `unsafe`). `miri` is therefore not required. (f) Manual smoke: build release binary (`make build`), point at a real codebase (e.g., `external/ripgrep` if initialized), kickoff async via raw stdio MCP request, poll, observe completion. Capture the binary's stderr to confirm the per-phase eprintln logs still print verbatim during async runs."
    depends_on: ["2.4"]
tags: [analyze, mcp, async, get_status, single-flight]
---

# Phase 2: Testing

## Overview

Race-tight unit coverage for the slot protocol, deterministic progress test via injected sleep, end-to-end integration test, rebaselined snapshots, and a clean `make lint` / `make snapshot-clean` / full-workspace test pass. Tasks 2.1, 2.2, 2.3 are parallel-safe (independent test additions in the same file). Task 2.4 depends on all three because it baselines snapshots that the new tests may touch via shared fixtures. Task 2.5 is the final gate.

## 2.1: Lifecycle + shape unit tests

### Subtasks
- [x] `async_kickoff_returns_immediately_with_running_job` — measure handler return time with `std::time::Instant::now()` before/after the call; assert elapsed < 100ms. Fixture: tempdir with 1 `.cpp` file. Assert response JSON has `status == "running"`, `existing == false`, `job_id.is_empty() == false`.
- [x] `async_kickoff_then_poll_completes` — kickoff, then in a `loop { tokio::time::sleep(50ms); read get_status; break on terminal; bail after 5s }` pattern. Assert eventual `status == "completed"`, `result.files == 1`.
- [x] `sync_analyze_populates_slot_with_completed` — call sync handler, after await read `inner.analyze_slot.read().current.as_ref().unwrap().state.read().status`, match for `JobStatus::Completed(result)`, assert `result.files == 1`.
- [x] `get_status_with_no_analyze_returns_null_job_fields` — fresh server (no analyze ever), call `get_status`, parse the JSON, assert both `analyze_job` and `analyze_job_previous_terminal` deserialize as JSON null.
- [x] `get_status_completed_carries_full_analyze_result` — kickoff async, poll to Completed, read `get_status`, assert `analyze_job.result` deserializes into the same `AnalyzeResult` shape that sync `analyze_codebase` returns (compare against a parallel sync run on the same fixture).
- [x] Run `cargo test -p code-graph-tools --lib handlers::analyze`. Expected: 14 tests pass (9 existing + 5 new).

### Notes
The 5-second poll bound on test (b) is a hang-catcher, not a real budget — a small 1-file fixture indexes in milliseconds. If the test ever hits the 5s bound, something is wrong (worker hung, slot not transitioning, atomic not flushed).

## 2.2: Single-flight race tests

### Subtasks
- [x] **Prerequisite — extend `test_recording_plugin.rs`.** Add `pub(crate) static SLEEP_PER_PARSE_MS: std::sync::atomic::AtomicU64 = AtomicU64::new(0)` and, inside the plugin's `parse_file` impl, call `std::thread::sleep(Duration::from_millis(SLEEP_PER_PARSE_MS.load(Ordering::Relaxed)))` when the value is non-zero. Use `std::thread::sleep` (NOT `tokio::time::sleep`) because `parse_file` runs inside rayon workers via `spawn_blocking`. Document the knob in the module's doc comment. Each test setting the knob resets it to 0 in a `Drop` guard helper so concurrent test runs don't cross-pollute.
- [x] `concurrent_async_kickoffs_only_one_spawns_worker` — use `tokio::sync::Barrier::new(2)` to synchronize two `tokio::spawn`ed tasks. Each task: `barrier.wait().await; call analyze_codebase_async(...)`. Collect both responses via a `JoinSet`. Sort by `existing` flag, assert one `false` + one `true`, both `job_id`s equal.
- [x] `async_duplicate_kickoff_after_first_started_returns_existing_job_id` — sequential calls with a `tokio::task::yield_now().await` between them (forces the slot write to commit before the second read).
- [x] `async_kickoff_blocks_sync_analyze` — set `SLEEP_PER_PARSE_MS=50`, generate a 5-file fixture (≥ 250ms guaranteed in-progress window). Kickoff async, then on the same task (no yield) call sync, assert sync returns `tool_error("indexing already in progress")` byte-identical. Reset the knob to 0 on test exit.
- [x] `sync_kickoff_blocks_async_kickoff` — set `SLEEP_PER_PARSE_MS=50`, generate a 20-file fixture (≥ 1s guaranteed in-progress window). Spawn sync analyze in a `tokio::spawn`. Spin-yield with bounded retries until the slot's `current.is_some()` and its `status == Running` (NOT a sleep — `loop { if slot is running, break; tokio::task::yield_now().await; if elapsed > 500ms panic }`). Then call async, assert `existing: true` with sync's job_id. Reset the knob on test exit.
- [x] Run `cargo test -p code-graph-tools --lib`. All 4 race tests pass deterministically across at least 10 consecutive runs (`for i in {1..10}; do cargo test -p code-graph-tools --lib race || break; done`) — if any test is flaky, the synchronization point is wrong; the spin-yield-with-bound pattern above is the deterministic primitive.

### Notes
The `test_recording_plugin` extension is the most fragile part. If it doesn't already support a per-call sleep injection, add one via a `pub static SLEEP_PER_PARSE: AtomicU64 = AtomicU64::new(0)` knob that the plugin reads inside `parse_file`. Document the knob in the test plugin's module doc.

## 2.3: Slot rotation, failure-path, and deterministic progress tests

### Subtasks
- [x] `terminal_job_rotates_to_previous_on_next_kickoff` — kickoff (capture T1.job_id), poll to Completed, kickoff (T2). Assert `analyze_slot.read().previous_terminal.as_ref().unwrap().job_id == T1.job_id` and `current.as_ref().unwrap().job_id == T2.job_id`.
- [x] `two_back_to_back_analyses_lose_oldest_terminal` — T1 → Completed → T2 → Completed → T3. Assert `previous_terminal.job_id == T2.job_id`; assert no slot reference holds T1's job_id anymore.
- [x] `failed_job_surfaces_error_in_get_status` — tempdir with `.code-graph.toml` content `[discovery\nmax_threads = nope\n`. Kickoff async. Poll until `status == "failed"`. Assert `analyze_job.error` starts with `"failed to parse .code-graph.toml"` byte-identical to today's sync error.
- [x] `failed_job_rotates_to_previous_terminal` — Failed counts as terminal for rotation. Kickoff failed T1, kickoff T2, assert `previous_terminal == T1` (status Failed, error preserved) and `current == T2`.
- [x] `progress_increments_during_indexing` — generate 20 trivial `.cpp` files in a tempdir, configure the test plugin with `SLEEP_PER_PARSE=10ms`. Kickoff async. In a loop with 30ms cadence, sample `analyze_job.progress` while `status == "running"`; record values into a `Vec<u32>`. Loop bound: 1s (200ms expected indexing duration × 5 safety margin). Assert: `values.windows(2).all(|w| w[0] <= w[1])` (monotonic non-decreasing) AND `values.iter().collect::<HashSet<_>>().len() >= 3` (≥ 3 distinct intermediate values, NOT just 0 → final).
- [x] All 5 tests pass. Run 10 consecutive times to confirm no flake.

### Notes
The progress test's 30ms cadence × 10ms per-file sleep × 20 files gives a deliberate ≥6 polls landing mid-run on average. The "≥ 3 distinct intermediate values" threshold leaves comfortable headroom for scheduler jitter while still catching the failure mode where the atomic isn't flushed until completion.

## 2.4: Integration test + get_status snapshot rebaseline

### Subtasks
- [x] Create `crates/code-graph-tools/tests/analyze_async_lifecycle.rs`. Use one of the existing small `testdata/` fixtures — search the workspace for a 5–10 file directory under `testdata/`. If none exist that are appropriate, generate inline via `tempfile::TempDir` + a few `.cpp` files (3 functions per file, mix of free functions and classes for realism).
- [x] Test body: build a `CodeGraphServer` with the full `LanguageRegistry`, call `handlers::analyze::analyze_codebase_async(...)` directly (the integration tests don't go through rmcp), capture the `job_id`. Loop `handlers::status::get_status(...)` with 50ms cadence (1s total bound) until terminal. On terminal, assert `analyze_job.result.files == expected`. Then call `handlers::symbols::get_file_symbols(...)` on one of the indexed files, parse the response, assert non-empty symbol list.
- [x] Run `cargo test -p code-graph-tools --test analyze_async_lifecycle` — passes.
- [x] **Per-tool snapshot for the new tool.** Open `crates/code-graph-tools/tests/snapshot_tools_list.rs`. Add `#[test] fn tools_list_analyze_codebase_async()` mirroring the existing per-tool functions (one `insta::assert_json_snapshot!` call against the tool descriptor). Run `cargo test -p code-graph-tools --test snapshot_tools_list tools_list_analyze_codebase_async` — fails first run with a `.snap.new` file. Run `cargo insta review` and accept; the new file `snapshot_tools_list__tools_list_analyze_codebase_async.snap` lands under `crates/code-graph-tools/tests/snapshots/`. Manually review the snapshot's description text per the "Documentation read cold" lens — operationally explains poll pattern, names response shape, names the grace-window semantic, names the `existing: true` semantic.
- [x] **Update the snapshot_tools_list module-level doc comment** from "18 registered tools" (or whatever the current count phrasing is) to "19". (Already "19" — no change needed; the count was bumped when Task 1.4 registered `analyze_codebase_async`.)
- [x] **Rebaseline `snapshot_tools_list__tools_list_get_status.snap`.** Run `cargo test -p code-graph-tools --test snapshot_tools_list tools_list_get_status` — should fail with a `.snap.new` showing the new optional fields in the tool description (if the description was updated in Task 1.5 to mention them) and/or the inputSchema unchanged. Review the diff, accept via `cargo insta review`. The diff should show only the new field mentions in the description text. (Already current — Task 1.5's commit `3252e87` baselined this; no `.snap.new` produced on re-run.)
- [x] **Rebaseline any `get_status` response snapshot** (if a test exists that snapshots `get_status` JSON output): run the full snapshot suite (`cargo test -p code-graph-tools`), inspect `.snap.new` files, accept additive changes (only `analyze_job: null` / `analyze_job_previous_terminal: null` for no-analyze fixtures, populated objects for after-analyze fixtures). (None exist — grepped `tests/`; only the tools-list snapshot references `get_status`.)
- [x] Run `make snapshot-clean` — must pass (no orphan `.snap.new` files).
- [x] Commit the rebaselined snapshots alongside the test additions in the same commit (per project convention — the pre-commit hook enforces snapshot-clean).

### Notes
If the project has a `tests/wire_format_snapshot.rs` or similar that snapshots all tool descriptions, that's the one to focus on for the description-text review. The description landed in Task 1.5 should already match the convention by the time Phase 2 starts; this task is the snapshot-acceptance ritual, not a fresh write.

## 2.5: Structural verification + full-workspace pass

### Subtasks
- [ ] `make lint` (= `cargo clippy --workspace --all-targets -- -D warnings`) — must be clean. Fix any clippy lints introduced by the new code.
- [ ] `make fmt-check` (= `cargo fmt --all --check`) — must be clean. Run `make fmt` (= `cargo fmt --all`) and commit if anything formats.
- [ ] `make test` (= `cargo test --workspace`) — all tests pass: 9 + 5 + 4 + 5 = 23 analyze-handler unit tests + 1 integration test + all existing workspace tests.
- [ ] `make snapshot-clean` — no orphan `*.snap.new`.
- [ ] Grep verification: `grep -rn 'unsafe' crates/code-graph-tools/src/analyze_job.rs crates/code-graph-tools/src/handlers/analyze.rs crates/code-graph-tools/src/handlers/status.rs` — zero matches. Per `shared/languages/rust.md`, miri is required only for new `unsafe`; we have none, so miri is N/A.
- [ ] Build release: `make build` (= `cargo build --release -p code-graph-mcp`) — clean.
- [ ] Manual smoke test: launch the release binary against a real codebase (use an `external/` submodule if initialized; otherwise the repo itself). Via raw stdio MCP requests (a one-liner with `jq` or a small Python script): (a) send `tools/call analyze_codebase_async` with `path=...` — verify response arrives in < 1s; (b) send `tools/call get_status` — verify `analyze_job.status == "running"` and `progress > 0` after a few seconds; (c) loop until `status == "completed"`; (d) send `tools/call get_file_symbols` for one of the indexed files — verify a real symbol list comes back; (e) verify the binary's stderr shows the per-phase eprintln logs (cache load, discover+parse, resolve, merge, save) — those are operator-visibility load-bearing.

### Notes
The release smoke test catches integration issues that unit tests miss: argument parsing for the new tool, the tool actually being registered in the rmcp router, the JSON wire format of the response being correct, and the per-phase eprintln still firing under the new code path. ~5 minutes of manual time; high value vs. cost.

## Acceptance Criteria
- [ ] All 14 new unit tests pass deterministically (10 consecutive runs each for the race tests in 2.2).
- [ ] The integration test in 2.4 passes.
- [ ] `make lint` / `make fmt-check` / `make test` / `make snapshot-clean` all green.
- [ ] No new `unsafe` introduced.
- [ ] Snapshot diffs reviewed and accepted; only the additive `analyze_job` / `analyze_job_previous_terminal` fields appear in `get_status` snapshots, and only the new `analyze_codebase_async` tool appears in any tool-descriptor snapshot.
- [ ] Release smoke test executes the agent's expected flow end-to-end against a real codebase.
- [ ] No regression in any pre-existing test.
