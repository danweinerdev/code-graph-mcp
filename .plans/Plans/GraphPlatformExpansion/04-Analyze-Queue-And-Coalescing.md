---
title: "Analyze Queue and Coalescing"
type: phase
plan: GraphPlatformExpansion
phase: 4
status: in-progress
created: 2026-08-08
updated: 2026-08-14
deliverable: "An analyze-only, path-compacting FIFO: a running scan remains unchanged; pending paths compact and followers receive the satisfying scan result."
waivers:
  - code: SDD075
    reason: "Tasks 4.5 through 4.12 preserve historical completion evidence from the superseded implementation; their original records lack the later timestamp format and must not be rewritten as fresh verification."
    accepted: "2026-08-13"
  - code: SDD169
    reason: "Tasks 4.5 through 4.12 preserve historical completion evidence from the superseded implementation; their original reviewed-candidate form predates the current exact-identity rule and must remain historical."
    accepted: "2026-08-13"
tasks:
  - id: "4.1"
    title: "Pending queue in AnalyzeSlot and the queued job status"
    status: complete
    justifies: "FR-41, AC-50. AnalyzeSlot holds exactly one current job and one previous_terminal — there is nowhere to put 'admitted but not started', so FR-41's queueing requirement cannot be met by reinterpreting the existing fields."
    verification: "cargo test -p code-graph-tools analyze_job:: — an analyze issued while another is in flight is queued and eventually runs rather than returning 'indexing already in progress' (AC-50); get_status keeps analyze_job as the single running job and exposes pending count plus FIFO ids, while queued kickoff responses report status queued; get_job_status(job_id) retrieves queued, running, and retained terminal jobs so a fast FIFO cannot rotate an async caller's result out of reach; the head of pending is promoted on termination under the existing rotation; previous_terminal still preserves exactly one prior job."
  - id: "4.2"
    title: "Admitted-identity, path-containment coverage rule with force asymmetry"
    status: complete
    justifies: "FR-42, AC-51. Containment alone is not sufficient — a forced request absorbed into a non-forcing one silently skips the invalidation the caller asked for, which surfaces only as 'force didn't work' long after the fact."
    verification: "cargo test -p code-graph-tools coalesce:: && cargo test -p code-graph-tools config_identity:: — requests coalesce only after same discovered project root plus admitted effective-config identity/provenance; then the four force/containment cases apply: non-forcing nested under queued non-forcing coalesces; non-forcing nested under queued forcing coalesces; forcing nested under queued non-forcing does NOT coalesce and runs in its own right; disjoint paths never coalesce (AC-51). Config creation, removal, and replacement make a later request distinct even when path/force otherwise cover; matching identity still coalesces. Also covers the reverse-containment direction where the new request is broader than a queued one."
    depends_on: ["4.1"]
  - id: "4.3"
    title: "Coalesced-caller reporting and sync non-blocking rule"
    status: complete
    justifies: "FR-43, NFR-01, AC-52. A caller whose request vanished into another needs its outcome, and a sync caller queued behind N jobs would hit MCP_TOOL_TIMEOUT on any corpus — turning a documented large-repo hazard into an everyday one."
    verification: "cargo test -p code-graph-tools analyze:: plus the snapshot suite — a coalesced caller receives the covering request's outcome with an additive optional field naming it (AC-52); that field is absent, not null, when no coalescing occurred, so non-coalesced bodies stay byte-identical (NFR-01); analyze_codebase's body and analyze_job.result remain structurally identical under one deserializer; a sync request admitted behind pending jobs returns immediately with job_id and status queued rather than blocking."
    depends_on: ["4.2"]
  - id: "4.4"
    title: "Generalize the job slot to long-running queries and add detect_communities_async"
    status: complete
    justifies: "FR-49, AC-58, review follow-up FU-01. detect_communities runs whole-graph label propagation while holding the read lock, and the client tool timeout is wall-clock — spawn_blocking does not extend it, so at UE4 scale the server can finish and the caller still see a timeout with no way to recover the result. Landing it here reuses the slot being reshaped by 4.1 rather than touching AnalyzeSlot and AnalyzeJobView a second time."
    verification: "cargo test -p code-graph-tools job:: — a whole-graph query started asynchronously returns a job id sub-second and reports progress; get_status exposes it under the same polling vocabulary as an analyze job; the terminal result has the same DetectCommunitiesResponse shape and semantics as sync, though the JobView wrapper reserve can reduce rows or change next_offset (a nonbinding budget can match sync); every individual call after admission is short enough that a wall-clock timeout cannot fire; the synchronous detect_communities still works unchanged for small graphs."
    depends_on: ["4.1"]
  - id: "4.5"
    title: "Freeze admitted analyze config identity through execution"
    status: complete
    justifies: "Phase-review blind spot. A queued request can be admitted under one nested-config/project-root boundary and execute under another if `.code-graph.toml` changes while it waits, invalidating the identity used for coalescing."
    verification: "cargo test -p code-graph-tools config_identity:: — queued and covered analyzes retain or revalidate the admitted project/config identity across nested config creation, removal, and replacement, so no coalesced caller receives a result from a different project boundary."
    depends_on: ["4.4"]
  - id: "4.6"
    title: "Budget async community job envelopes"
    status: complete
    justifies: "Phase-review blind spot. The community result currently consumes the full response budget before JobView metadata wraps it, allowing get_job_status to exceed [response].max_bytes."
    verification: "cargo test -p code-graph-tools async_community_budget:: — terminal get_job_status serialization, including JobView metadata and request fields, stays within the configured response budget while preserving resumable community pagination."
    depends_on: ["4.4"]
  - id: "4.7"
    title: "Move analyze admission filesystem probes off Tokio workers"
    status: complete
    justifies: "Phase-review blind spot. Canonicalization and config discovery currently run synchronously before async kickoff detaches, so slow network filesystems can violate the sub-second-call contract and block a Tokio worker."
    verification: "cargo test -p code-graph-tools admission_probe:: — blocking dispatch protects the Tokio scheduler while preserving validation/coalescing behavior and unrelated Tokio-task responsiveness; analyze kickoff can still wait during admission when filesystem canonicalization/config discovery is slow."
    depends_on: ["4.5"]
  - id: "4.8"
    title: "Bound shared pending FIFO"
    status: complete
    justifies: "D-0010. An unbounded shared queue lets disconnected or bursty clients retain arbitrary shutdown-drain guards and makes daemon shutdown latency unbounded."
    verification: "cargo test -p code-graph-tools pending_limit_rejects_distinct_jobs_but_keeps_covered_analyzes && cargo test -p code-graph-tools queue_full_rejection_does_not_extend_shutdown_drain — the shared FIFO holds at most 32 pending jobs, covered analyzes still coalesce at capacity, distinct analyze/community overflow is rejected without an ID, guard, or queue mutation, promotion frees one slot, and rejected work does not extend shutdown drain."
    depends_on: ["4.4"]
  - id: "4.9"
    title: "Reject zero response byte budgets"
    status: complete
    justifies: "Phase-review follow-up. A zero `[response].max_bytes` budget lets generic paginated tools return empty, non-progressing pages forever."
    verification: "Prospective: cargo test -p code-graph-core; cargo test -p code-graph-tools async_community_budget; cargo test -p code-graph-tools; cargo fmt --all --check; cargo clippy --workspace --all-targets -- -D warnings; make snapshot-clean; make plugin-sync-check; git diff --check — zero is rejected with the established message, negative and non-integer values remain rejected, positive values remain accepted, and positive irreducibly tiny async-community budgets retain their start-fresh behavior."
    depends_on: ["4.6"]
  - id: "4.10"
    title: "Reconcile coalescing contract with admitted config identity"
    status: complete
    justifies: "Phase 4 review follow-up. Path-and-force coverage wording omits the already-reviewed admitted effective-config identity/provenance gate; removing config_identity/config_present would reintroduce a wrong-result TOCTOU."
    verification: "cargo test -p code-graph-tools config_identity::; cargo test -p code-graph-tools coalesce::; cargo test -p code-graph-core; cargo fmt --all --check; cargo clippy --workspace --all-targets -- -D warnings; make plugin-sync-check; make snapshot-clean; git diff --check — config creation, removal, and replacement prevent coalescing even when path/force otherwise cover; same admitted identity still coalesces; a config-distinct request at capacity is queue-full rather than wrong-result coalesced; response.max_bytes rejects zero and documents tiny positive start-fresh pages."
    depends_on: ["4.5"]
  - id: "4.11"
    title: "Preserve continuation at the count limit"
    status: complete
    justifies: "Frozen phase-review follow-up. A full count-limited page currently reports natural completion without checking whether another result exists, making later rows unreachable through next_offset."
    verification: "Prospective: cargo test -p code-graph-tools byte_budget_take; cargo test -p code-graph-tools async_community_budget; cargo test -p code-graph-tools; cargo fmt --all --check; cargo clippy --workspace --all-targets -- -D warnings; make snapshot-clean; make plugin-sync-check; git diff --check — exact-limit pages end naturally, limit-plus-one pages expose a strict continuation, and resumed pages recover all rows without gaps or duplicates."
    depends_on: ["4.6"]
  - id: "4.12"
    title: "Report the applied config provenance in status"
    status: complete
    justifies: "Frozen phase-review follow-up. get_status currently probes config-path existence at query time, so config creation/removal after admission can misreport which configuration produced the active index."
    verification: "Prospective: cargo test -p code-graph-tools config_path; cargo test -p code-graph-tools; cargo fmt --all --check; cargo clippy --workspace --all-targets -- -D warnings; make snapshot-clean; make plugin-sync-check; git diff --check — status config_path derives from the admitted/applied config provenance and remains stable across post-admission config creation/removal."
    depends_on: ["4.5"]
  - id: "4.15"
    title: "Rollback superseded Phase 4 implementation"
    status: in-progress
    justifies: "FR-41, FR-42, FR-43, AC-50, AC-51, AC-52. The generic scheduler, async-community route, configuration-provenance coalescing, and coalesced_by surface contradict the replacement scope and must be removed first."
    verification: "cargo test -p code-graph-tools; cargo test -p code-graph-mcp; cargo fmt --all --check; cargo clippy --workspace --all-targets -- -D warnings; make plugin-sync-check; make snapshot-clean; git diff --check — Phase 3 remains intact, pre-Phase-4 contention behavior is restored, and generic jobs, detect_communities_async, config-provenance coalescing, and coalesced_by are absent."
  - id: "4.16"
    title: "Implement analyze-only path-compacting FIFO"
    status: in-progress
    justifies: "FR-41, FR-42, FR-43, AC-50, AC-51, AC-52. Concurrent daemon clients need serialized analysis without redundant queued descendants or loss of requested force."
    verification: "cargo test -p code-graph-tools analyze_job::; cargo test -p code-graph-tools analyze::; cargo test -p code-graph-mcp; cargo fmt --all --check; cargo clippy --workspace --all-targets -- -D warnings; make verify — tests prove immutable running work, follower absorption, earliest-position ancestor replacement, disjoint FIFO order, result/error propagation, force OR, post-compaction cap, and distinct-33rd rejection."
  - id: "4.17"
    title: "Exercise queue semantics through live daemon clients"
    status: complete
    justifies: "FR-41, FR-42, FR-43, AC-50, AC-51, AC-52. The replacement queue is currently covered only through in-process handlers and unit state; real proxy clients must prove they share the daemon slot and observe compaction, terminal attribution, capacity, and shutdown behavior across the transport boundary."
    verification: "cargo test -p code-graph-mcp --test daemon_proxy queue; cargo test -p code-graph-mcp; cargo fmt --all --check; cargo clippy --workspace --all-targets -- -D warnings; make verify — isolated live clients prove pending absorption, ancestor replacement at earliest FIFO position, force OR, success/error follower completion, post-compaction 32-entry rejection, and daemon shutdown drain without leaked processes."
---

# Analyze Queue and Coalescing

## Overview
The prior Phase 4 implementation is historical work that will be cleanly rolled back; its completion evidence below is not evidence for this replacement. The active deliverable is an **analyze-only** FIFO. A running scan never changes. Only pending canonical paths compact: an equal or ancestor pending path absorbs an incoming follower; an incoming ancestor replaces pending descendants at the earliest displaced FIFO position; disjoint entries retain FIFO order. Followers receive the satisfying scan result or error, and each compacted scan uses `force = OR`. Generic jobs, `detect_communities_async`, community admission, configuration identity/provenance coalescing, and `coalesced_by` are outside this phase.

Depends on phase 3. Separated from it so a bisect through daemon bring-up does not also cross this change.

## 4.1: Pending queue in AnalyzeSlot and the queued job status
### Subtasks
- [x] Add `pending: Vec<Arc<AnalyzeJob>>` to `AnalyzeSlot`, ordered by admission
- [x] Add a `Queued` variant to the job status and map it to the wire string `"queued"`
- [x] Promote the head of `pending` to `current` when the running job terminates, reusing the existing rotation
- [x] Expose pending entries as a count plus ids rather than widening `analyze_job` into a list
- [x] Add job-addressable status/result lookup so queued async outcomes remain retrievable after slot rotation
- [x] Tests for admission, promotion, rotation, and the queued view

### Notes
Revision boundary: the queue exists and jobs move through it; the coverage rule is not applied yet, so every admitted request runs.

Keeping `analyze_job` a single job matters — every current client reads it as one object, and widening it into a list would break them for no benefit. A count plus ids is enough for a caller to understand the backlog.

Job-addressable lookup is the retrieval counterpart: pending IDs would otherwise become dead handles once fast jobs rotate beyond the one-entry `previous_terminal` grace window. `get_job_status(job_id)` preserves the existing `get_status` shape while making each issued ID actionable; terminal retention is bounded and oldest-terminal-only eviction never removes queued or running work.

`AnalyzeJobView.status` gaining a fourth value is an additive change to a documented enum. Clients matching exhaustively on three values exist by assumption, so CLAUDE.md and the tool description must call it out in this phase, not later.

### Completion Evidence

- Verified: 2026-08-12
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `6b5302ee40a700bdb0372b1475833d8bc1676806`
- Identity recheck: `git show 6b5302ee40a700bdb0372b1475833d8bc1676806 >/dev/null && git rev-parse HEAD`, 2026-08-12T00:19:21-07:00; matched `6b5302ee40a700bdb0372b1475833d8bc1676806`
- Focused review: `git show 6b5302ee40a700bdb0372b1475833d8bc1676806`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `6b5302ee40a700bdb0372b1475833d8bc1676806`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools analyze_job:: && cargo test -p code-graph-tools --test integration && cargo test -p code-graph-tools --test snapshot_tools_list && cargo test -p code-graph-mcp --test smoke && make plugin-sync-check && make snapshot-clean && make fmt-check && make lint` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Focused queue/status, integration, tool-snapshot, 23-tool smoke, mirror, snapshot, formatting, and deny-warnings lint gates passed. |
| `cargo test -p code-graph-mcp --test daemon_proxy && cargo test -p code-graph-mcp --test daemon_serve` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Process-level proxy and serve routes accepted all 23 tools, including `get_job_status`. |
| `make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Full workspace structural gate passed after removing the generated ignored project cache that polluted scoped baseline state. |
| `make fmt-check && make lint && cargo test -p code-graph-tools analyze_job:: && cargo test -p code-graph-tools --test integration && git diff --check` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Post-review comment/documentation fixes remained formatted, warning-free, behaviorally covered, and whitespace-clean. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Focused intent-blind quality review | Complete task 4.1 diff excluding lifecycle-only plan files | PASS | Queue, promotion, cancellation/panic supervision, bounded terminal retention, polling route, tests, and documentation were judged correct, scoped, maintainable, and bisectable after stale-comment fixes. |

## 4.2: Admitted-identity, path-containment coverage rule with force asymmetry
### Subtasks
- [x] Implement `covers(x, y)` as a pure function over admitted project/config identity plus `(path, force)`
- [x] Run admission against the running job and every pending entry
- [x] Attach a coalesced request to its coverer rather than appending it
- [x] Unit tests over same-identity force/containment plus disjoint and reverse-containment, and config-identity tests for creation, removal, replacement, and same-identity coalescing

### Notes
Revision boundary: coalescing is live and correct; how a coalesced caller learns about it lands in 4.3.

The rule: **X covers Y only when both requests have the same discovered project root and the same admitted effective-config identity and provenance; then Y's path is at or under X's path, and (X forces or Y does not force).** A configuration created, removed, or replaced between admissions makes the later request distinct even if containment and force would otherwise cover it; at capacity that distinct request receives queue-full rather than a wrong-result coalescing. Writing the final containment/force predicate as a pure function is deliberate — it makes the four force cases trivially testable without constructing jobs or a daemon, while the admitted-identity tests pin the TOCTOU guard.

### Completion Evidence

- Verified: 2026-08-12
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `6144a9fc640adb9786e32862f675cc753fd174e5`
- Identity recheck: `git rev-parse HEAD && git show --quiet --format=%H 6144a9fc640adb9786e32862f675cc753fd174e5`, 2026-08-12T12:49:48-07:00; both matched `6144a9fc640adb9786e32862f675cc753fd174e5`
- Focused review: `git show 6144a9fc640adb9786e32862f675cc753fd174e5`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `6144a9fc640adb9786e32862f675cc753fd174e5`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools coalesce` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Pure component-aware force matrix and admission-level running, pending, invalid-path, reverse-containment, project-boundary, symlink-retarget, and daemon-root coalescing regressions passed. |
| `cargo test -p code-graph-tools --lib && cargo test -p code-graph-tools --test integration && cargo test -p code-graph-tools --test analyze_async_lifecycle` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Library queue/promotion/cancellation coverage, handler integration, and async kickoff-poll-query lifecycle passed. |
| `make fmt-check && make lint && make snapshot-clean && make plugin-sync-check && git diff --check` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting, deny-warnings clippy, snapshot hygiene, generated plugin parity, and whitespace checks passed. |
| `make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Full workspace structural gate passed on the final reviewed implementation. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Focused intent-blind quality review | Complete task 4.2 implementation diff | PASS | Coverage and force semantics, canonical execution identity, nested project boundaries, daemon lifecycle checks, guard ownership, FIFO regressions, async reporting, and task boundary were judged correct and bisectable. |

### Trap
Implementing coverage as path containment alone. It reads as obviously correct and passes any test that does not vary the force flag or configuration snapshot. The failures it produces are a forced re-index silently absorbed into a plain one, so stale entries survive, or a caller admitted after a config change receiving an older job's result; both appear much later and look like cache bugs, not queue bugs.

## 4.3: Coalesced-caller reporting and sync non-blocking rule
### Subtasks
- [x] Add the optional coalescing field to the shared analyze result shape with `skip_serializing_if`
- [x] Return the coverer's outcome to the coalesced caller
- [x] Make a sync `analyze_codebase` admitted behind pending jobs return immediately with `job_id` and queued status
- [x] Preserve immediate-start and coalesced sync behaviour unchanged
- [x] Update CLAUDE.md and the tool descriptions: retired error, new status value, new field, changed sync blocking
- [x] Snapshot verification that non-coalesced bodies are byte-identical

### Notes
Revision boundary: the queue is fully observable and the phase's wire evolution is complete and documented.

The field is *absent*, not `null`, when coalescing did not occur — deliberately unlike `analyze_job`/`analyze_job_previous_terminal`, which serialize explicit `null` so clients can distinguish "never happened" from "old server". Here absence and "not coalesced" are the same fact, so an explicit null would add a field to every response to convey nothing.

Adding it to the *shared* shape keeps `analyze_codebase`'s body and `analyze_job.result` structurally identical, preserving the single-deserializer property CLAUDE.md documents.

### Completion Evidence

- Verified: 2026-08-12
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `0a72bab32a230bb2a60e5f3e17dd1f992079fe7d`
- Identity recheck: `git rev-parse HEAD && git show --quiet --format=%H 0a72bab32a230bb2a60e5f3e17dd1f992079fe7d`, 2026-08-12T13:44:17-07:00; both matched `0a72bab32a230bb2a60e5f3e17dd1f992079fe7d`
- Focused review: `git show 0a72bab32a230bb2a60e5f3e17dd1f992079fe7d`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `0a72bab32a230bb2a60e5f3e17dd1f992079fe7d`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools analyze::` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Covered success/failure attribution, first queued sync blocking, already-pending immediate return, cancellation, FIFO, and stored-result compatibility passed. |
| `cargo test -p code-graph-tools --test integration && cargo test -p code-graph-tools --test snapshot_responses && cargo test -p code-graph-tools --test snapshot_tools_list` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Integration and response/tool-description snapshots passed; ordinary analyze bodies remained unchanged and omit `coalesced_by`. |
| `make fmt-check && make lint && make plugin-sync-check && make snapshot-clean && git diff --check` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting, deny-warnings clippy, generated plugin parity, snapshot hygiene, and whitespace checks passed. |
| `make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Full workspace gate passed after one isolated daemon idle-timer retry succeeded. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Focused intent-blind quality review | Complete task 4.3 diff | PASS | Additive wire compatibility, coalesced success/failure attribution, exact sync blocking boundary, sink ownership, descriptions, and task boundary were aligned. |

### Trap
Letting sync `analyze_codebase` block behind the queue because "that's what a queue means". CLAUDE.md already documents `MCP_TOOL_TIMEOUT` killing long sync analyses on large trees; queueing makes wall-clock depend on other sessions' work, so a small repo can now time out because someone else started an index. Return queued instead.

## 4.4: Generalize the job slot to long-running queries and add detect_communities_async
### Subtasks
- [x] Widen the job slot from analyze-specific to a job kind that covers long-running queries
- [x] Keep `analyze_job` in `get_status` reporting analyze jobs, so no existing client breaks
- [x] Add an async form of `detect_communities` on that machinery
- [x] Ensure the async terminal result has the same `DetectCommunitiesResponse` shape and semantics as sync; the JobView wrapper reserve can reduce rows or change `next_offset`, while a nonbinding budget can match sync
- [x] Leave synchronous `detect_communities` working unchanged — it is the right call on a small graph
- [x] Document both forms and when to reach for each (NFR-11)

### Notes
Revision boundary: any whole-graph query can run as a job, and `detect_communities` uses it.

This arrives from phase 1's review as FU-01. The measured cost is 177 ms on 841 files, which is fine; the concern is UE4/LLVM scale, which nothing has measured. CLAUDE.md already documents the failure mode for `analyze_codebase`: the client gives up on wall-clock while the server runs to completion, and the result is unrecoverable because there is no polling path. `spawn_blocking` does not help — it protects the tokio scheduler, not the client's timer.

Generalize rather than special-case. A second job mechanism beside the analyze one means two vocabularies for the same concept and two things to keep in sync.

`get_status` exposes the generalized slot through additive `job`, `job_previous_terminal`, `job_pending_count`, and `job_pending_ids` fields. Existing `analyze_job*` fields remain analyze-only compatibility projections (D-0009).

### Completion Evidence

- Verified: 2026-08-12
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `8aaac0a02dd9e95eb21ea3b91d1abf231cd9c7bc`
- Identity recheck: `git rev-parse HEAD && git show --quiet --format=%H 8aaac0a02dd9e95eb21ea3b91d1abf231cd9c7bc`, 2026-08-12T16:53:17-07:00; both matched `8aaac0a02dd9e95eb21ea3b91d1abf231cd9c7bc`
- Focused review: `git show 8aaac0a02dd9e95eb21ea3b91d1abf231cd9c7bc`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `8aaac0a02dd9e95eb21ea3b91d1abf231cd9c7bc`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 498 library/integration tests passed with one timing benchmark ignored; async community result equality, progress, mixed FIFO, panic promotion, retention, and compatibility projections passed. |
| `cargo test -p code-graph-mcp` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Binary, daemon, proxy, serve, and 24-tool smoke tests passed. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && make plugin-sync-check && make snapshot-clean && git diff --check` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting, deny-warnings lint, plugin mirrors, snapshots, and whitespace were clean. |
| `make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Full workspace gate passed after isolated confirmation of the known flaky daemon idle-future test. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Focused intent-blind quality review | Complete task 4.4 diff | PASS | Generic job compatibility, D-0009 projections, mixed-kind lifecycle, result identity, progress, descriptions, and task boundary were aligned. |

### Trap
Reaching for `spawn_blocking` and considering it solved. It moves the work off the async worker and does nothing about the client's wall-clock timeout, which is the actual failure. Only a return-immediately-and-poll shape fixes that.

## 4.5: Freeze admitted analyze config identity through execution
### Subtasks
- [x] Carry the admitted project/config identity into queued and running jobs
- [x] Prevent execution from silently crossing a changed nested-config boundary
- [x] Cover nested config creation, removal, and replacement while queued

### Notes

Historical evidence for the superseded identity-gated implementation; task 4.15 removes this behavior rather than reopening or backfilling this completed task.

### Completion Evidence

- Verified: 2026-08-13
- Repository: `~/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `b2f48f6f972a3c4488571668f1e8448992791cdf`
- Identity recheck: `git rev-parse b2f48f6f972a3c4488571668f1e8448992791cdf`, 2026-08-13; matched `b2f48f6f972a3c4488571668f1e8448992791cdf`
- Focused review: `git show b2f48f6f972a3c4488571668f1e8448992791cdf`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `b2f48f6f972a3c4488571668f1e8448992791cdf` / `b2f48f6f972a3c4488571668f1e8448992791cdf`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools config_identity::` | `.` | PASS (`exit 0`) | Queued and covered analyzes retain or revalidate admitted project/config identity across nested-config creation, removal, and replacement. |
| `make verify` | `.` | PASS (`exit 0`) | Latest full workspace gate was green for the committed implementation. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Four-lane focused review | Complete task 4.5 diff | PASS/Aligned | Inspection evidence recorded for correctness, scope, tests, maintainability, and task boundary; this is not a frozen phase gate. |

## 4.6: Budget async community job envelopes
### Subtasks
- [x] Reserve generic JobView wrapper overhead before community result budgeting
- [x] Verify terminal polling stays within `[response].max_bytes`
- [x] Preserve pagination resume semantics under the reduced nested-result budget

### Notes

The async terminal result has the same `DetectCommunitiesResponse` shape and semantics as sync, but the JobView wrapper reserve can reduce rows or change `next_offset`; a nonbinding budget can match sync. Under a pathological tiny async budget, an empty `truncated` page with an unchanged `next_offset` is a start-fresh marker, not pagination progress: do not retry unchanged; raise `[response].max_bytes`, rerun `analyze_codebase` to refresh cached config, then retry.

### Completion Evidence

- Verified: 2026-08-13
- Repository: `~/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `b2f48f6f972a3c4488571668f1e8448992791cdf`
- Identity recheck: `git rev-parse b2f48f6f972a3c4488571668f1e8448992791cdf`, 2026-08-13; matched `b2f48f6f972a3c4488571668f1e8448992791cdf`
- Focused review: `git show b2f48f6f972a3c4488571668f1e8448992791cdf`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `b2f48f6f972a3c4488571668f1e8448992791cdf` / `b2f48f6f972a3c4488571668f1e8448992791cdf`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools async_community_budget` | `.` | PASS (`exit 0`) | Terminal `get_job_status` serialization, including JobView metadata and request fields, stayed within the configured response budget with resumable pagination preserved. |
| `make verify` | `.` | PASS (`exit 0`) | Latest full workspace gate was green for the committed implementation. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Four-lane focused review | Complete task 4.6 diff | PASS/Aligned | Inspection evidence recorded for correctness, scope, tests, maintainability, and task boundary; this is not a frozen phase gate. |

## 4.7: Move analyze admission filesystem probes off Tokio workers
### Subtasks
- [x] Dispatch canonicalization and config discovery through blocking execution
- [x] Preserve sync and async validation/coalescing semantics
- [x] Add scheduler-responsiveness regression coverage

### Notes

Historical evidence for the superseded admission path. The replacement queue uses canonical paths but does not retain the configuration-identity admission contract.

### Completion Evidence

- Verified: 2026-08-13
- Repository: `~/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `b2f48f6f972a3c4488571668f1e8448992791cdf`
- Identity recheck: `git rev-parse b2f48f6f972a3c4488571668f1e8448992791cdf`, 2026-08-13; matched `b2f48f6f972a3c4488571668f1e8448992791cdf`
- Focused review: `git show b2f48f6f972a3c4488571668f1e8448992791cdf`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `b2f48f6f972a3c4488571668f1e8448992791cdf` / `b2f48f6f972a3c4488571668f1e8448992791cdf`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools admission_probe::` | `.` | PASS (`exit 0`) | Focused admission-probe coverage confirmed blocking dispatch preserves admission behavior and Tokio responsiveness. |
| `cargo test -p code-graph-tools` | `.` | PASS (`exit 0`) | Full tools suite passed for the committed implementation. |
| `make verify` | `.` | PASS (`exit 0`) | Latest full workspace gate was green for the committed implementation. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Four-lane focused review | Complete task 4.7 diff | PASS/Aligned | Inspection evidence recorded for correctness, scope, tests, maintainability, and task boundary; this is not a frozen phase gate. |

## 4.8: Bound shared pending FIFO
### Subtasks
- [x] Cap the shared pending FIFO at 32 entries, excluding the current job and retained terminal history
- [x] Evaluate analyze coverage before rejecting a distinct overflow, so covered analyzes still coalesce at capacity
- [x] Reject distinct analyze and community overflow without issuing an ID, retaining a guard, or mutating the queue
- [x] Verify promotion frees exactly one pending slot and rejected work does not extend shutdown drain

### Notes

Historical evidence for the former shared generic queue. Task 4.16 retains only the numeric 32-entry bound after analyze-only path compaction.

### Completion Evidence

- Verified: 2026-08-13
- Repository: `~/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `b2f48f6f972a3c4488571668f1e8448992791cdf`
- Identity recheck: `git rev-parse b2f48f6f972a3c4488571668f1e8448992791cdf`, 2026-08-13; matched `b2f48f6f972a3c4488571668f1e8448992791cdf`
- Focused review: `git show b2f48f6f972a3c4488571668f1e8448992791cdf`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `b2f48f6f972a3c4488571668f1e8448992791cdf` / `b2f48f6f972a3c4488571668f1e8448992791cdf`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools pending_limit_rejects_distinct_jobs_but_keeps_covered_analyzes` | `.` | PASS (`exit 0`) | Prospective verification confirmed the 32-entry cap rejects distinct work while covered analyzes still coalesce. |
| `cargo test -p code-graph-tools queue_full_rejection_does_not_extend_shutdown_drain` | `.` | PASS (`exit 0`) | Prospective verification confirmed rejected work does not extend shutdown drain. |
| `make verify` | `.` | PASS (`exit 0`) | Latest full workspace gate was green for the committed implementation. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Four-lane focused review | Complete task 4.8 diff | PASS/Aligned | Inspection evidence recorded for correctness, scope, tests, maintainability, and task boundary; this is not a frozen phase gate. |

## 4.9: Reject zero response byte budgets
### Subtasks

- [x] Restore `[response].max_bytes > 0` validation in the core configuration deserializer
- [x] Preserve rejection of negative and non-integer values and acceptance of positive values
- [x] Retain the positive tiny-budget async-community start-fresh behavior
- [x] Run the prospective focused, full, and repository hygiene verification

### Notes

Historical independent configuration validation; its async-community references are superseded, while the completed validation evidence remains intact.

### Completion Evidence

- Verified: 2026-08-13
- Repository: `~/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `5886dc732e85207e4b21df9ecfeb0f08b3241733`
- Identity recheck: `git rev-parse 5886dc732e85207e4b21df9ecfeb0f08b3241733`, 2026-08-13; matched `5886dc732e85207e4b21df9ecfeb0f08b3241733`
- Focused review: `git show 5886dc732e85207e4b21df9ecfeb0f08b3241733`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `5886dc732e85207e4b21df9ecfeb0f08b3241733` / `5886dc732e85207e4b21df9ecfeb0f08b3241733`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-core` | `.` | PASS (`exit 0`) | Core configuration validation tests passed: zero is rejected; negative and non-integer values remain rejected; positive values remain accepted. |
| `cargo test -p code-graph-tools async_community_budget` | `.` | PASS (`exit 0`) | Async-community budget coverage passed, including positive irreducibly tiny-budget start-fresh behavior. |
| `cargo test -p code-graph-tools` | `.` | PASS (`exit 0`) | Full tools suite passed for the committed implementation. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && make snapshot-clean && make plugin-sync-check && git diff --check` | `.` | PASS (`exit 0`) | Formatting, deny-warnings lint, snapshot hygiene, plugin parity, and whitespace checks passed. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Focused quality review | Complete task 4.9 diff | PASS/Aligned | Complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary; zero budgets are rejected and positive tiny budgets remain unchanged. |

## 4.10: Reconcile coalescing contract with admitted config identity
### Subtasks

- [x] Reconcile FR-42, AC-51, and Decision 7 with the admitted effective-config identity/provenance gate
- [x] Update task 4.2 wording and verification to name identity before containment and force
- [x] Document that config creation, removal, or replacement makes a later request distinct and can yield queue-full at capacity
- [x] Document `[response].max_bytes > 0` and the tiny-positive start-fresh recovery in the example configuration
- [x] Add/retain `config_identity::` and `coalesce::` coverage proving config differences do not coalesce, matching identity does coalesce, and a config-distinct request is queue-full at capacity
- [x] Run the prospective verification

### Notes

This review follow-up corrects governing-document wording; it does not remove `config_identity` or `config_present`. Those admission-snapshot checks prevent a caller admitted after a configuration transition from receiving a coverer's stale project/config result.

### Completion Evidence

- Verified: 2026-08-13
- Repository: `~/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `b6716a04f430fa9a7cdea4a4d5402d645f9b1007`
- Identity recheck: `git rev-parse b6716a04f430fa9a7cdea4a4d5402d645f9b1007`, 2026-08-13; matched `b6716a04f430fa9a7cdea4a4d5402d645f9b1007`
- Focused review: `git show b6716a04f430fa9a7cdea4a4d5402d645f9b1007`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `b6716a04f430fa9a7cdea4a4d5402d645f9b1007` / `b6716a04f430fa9a7cdea4a4d5402d645f9b1007`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools config_identity::` | `.` | PASS (`exit 0`) | Config creation, removal, and replacement prevent coalescing; matching admitted identity still coalesces. |
| `cargo test -p code-graph-tools coalesce::` | `.` | PASS (`exit 0`) | Same-identity containment and force coverage passed; a config-distinct request at capacity is queue-full rather than wrong-result coalesced. |
| `cargo test -p code-graph-core` | `.` | PASS (`exit 0`) | Core configuration validation passed: `response.max_bytes` rejects zero while positive values remain accepted. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && make plugin-sync-check && make snapshot-clean && git diff --check` | `.` | PASS (`exit 0`) | Formatting, deny-warnings lint, plugin parity, snapshot hygiene, and whitespace checks passed. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Focused spec-compliance review | Complete task 4.10 diff | PASS/Aligned | The admitted effective-config identity/provenance gate precedes containment and force; config transitions remain distinct, capacity rejects distinct work, and positive tiny-budget start-fresh recovery remains documented. |

## 4.11: Preserve continuation at the count limit
### Subtasks

- [x] Distinguish exact-limit natural completion from limit-plus-one continuation
- [x] Return a strict `next_offset` when unreturned rows remain
- [x] Prove resumed sync and async-community pages recover all rows without gaps or duplicates
- [x] Correct agent-facing config-aware coalescing wording found by the same frozen review
- [x] Document the generic empty non-advancing start-fresh marker for byte-starved pages

### Notes

Historical independent pagination fix. Its evidence is preserved but it is not part of the replacement queue's behavior.

### Completion Evidence

- Verified: 2026-08-13
- Repository: `~/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `4eaccaad0a46e6e3ebf9c41f9ebdea3875bc96dc`
- Identity recheck: `git rev-parse 4eaccaad0a46e6e3ebf9c41f9ebdea3875bc96dc`, 2026-08-13; matched `4eaccaad0a46e6e3ebf9c41f9ebdea3875bc96dc`
- Focused review: `git show 4eaccaad0a46e6e3ebf9c41f9ebdea3875bc96dc`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `4eaccaad0a46e6e3ebf9c41f9ebdea3875bc96dc` / `4eaccaad0a46e6e3ebf9c41f9ebdea3875bc96dc`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools byte_budget_take` | `.` | PASS (`exit 0`) | Count-limit pagination distinguishes exact-limit completion from limit-plus-one continuation and emits a strict continuation offset when rows remain. |
| `cargo test -p code-graph-tools async_community_budget` | `.` | PASS (`exit 0`) | Async-community pagination preserves continuation and the byte-starved start-fresh marker without gaps or duplicates. |
| `cargo test -p code-graph-tools` | `.` | PASS (`exit 0`) | Full tools suite passed for the committed implementation. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && make snapshot-clean && make plugin-sync-check && git diff --check` | `.` | PASS (`exit 0`) | Formatting, deny-warnings lint, snapshot hygiene, plugin parity, and whitespace checks passed. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Focused quality review | Complete task 4.11 diff | PASS/Aligned | Count-limit continuation, search pagination, and byte-starved start-fresh recovery were reviewed for correctness, scope, tests, maintainability, and task boundary. |

## 4.12: Report the applied config provenance in status
### Subtasks

- [x] Store the applied config path/provenance with indexed server state
- [x] Make `get_status.config_path` report applied state rather than current filesystem existence
- [x] Cover config creation and removal after admission/indexing

### Notes

Historical status-reporting fix. It does not reintroduce configuration provenance as a Phase 4 queue-compaction condition.

### Completion Evidence

- Verified: 2026-08-13
- Repository: `~/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `4eaccaad0a46e6e3ebf9c41f9ebdea3875bc96dc`
- Identity recheck: `git rev-parse 4eaccaad0a46e6e3ebf9c41f9ebdea3875bc96dc`, 2026-08-13; matched `4eaccaad0a46e6e3ebf9c41f9ebdea3875bc96dc`
- Focused review: `git show 4eaccaad0a46e6e3ebf9c41f9ebdea3875bc96dc`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `4eaccaad0a46e6e3ebf9c41f9ebdea3875bc96dc` / `4eaccaad0a46e6e3ebf9c41f9ebdea3875bc96dc`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools config_path` | `.` | PASS (`exit 0`) | Applied config provenance remains stable when configuration files are created or removed after admission/indexing. |
| `cargo test -p code-graph-tools status::` | `.` | PASS (`exit 0`) | Status publication reports the applied config path from indexed server state. |
| `cargo test -p code-graph-tools` | `.` | PASS (`exit 0`) | Full tools suite passed for the committed implementation. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && make snapshot-clean && make plugin-sync-check && git diff --check` | `.` | PASS (`exit 0`) | Formatting, deny-warnings lint, snapshot hygiene, plugin parity, and whitespace checks passed. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| Focused quality review | Complete task 4.12 diff | PASS/Aligned | Applied-config publication locking and ordering were reviewed for correctness, scope, tests, maintainability, and task boundary. |

## Historical Implementation Note
Tasks 4.1–4.12 and their evidence record the superseded implementation through `4eaccaad0a46e6e3ebf9c41f9ebdea3875bc96dc`. They do not satisfy the replacement acceptance criteria below; task 4.15 removes their queue-related behavior before task 4.16 builds the smaller design.

## 4.15: Rollback superseded Phase 4 implementation
### Subtasks

- [ ] Remove the uncommitted Phase 4.13/4.14 work and the committed generic scheduler, async-community route, generic polling/projections, config-provenance coalescing, and `coalesced_by` behavior through a clean implementation revision; do not rewrite published history.
- [ ] Restore the pre-Phase-4 analyze contention behavior while retaining all Phase 3 daemon functionality.
- [ ] Remove documentation and snapshots that advertise the superseded Phase 4 surface.
- [ ] Verify Phase 3 daemon/proxy behavior remains intact and record only commands actually run.

### Notes

Revision boundary: a clean, buildable Phase-3-plus-pre-Phase-4 baseline. This task deliberately does not introduce replacement queue behavior; isolating rollback keeps the simplification reviewable and reversible.

### Completion Evidence

Pending — not complete.

## 4.16: Implement analyze-only path-compacting FIFO
### Subtasks

- [ ] Add an analyze-only pending FIFO with at most 32 entries **after** compaction; never alter, attach to, upgrade, or replace the running scan.
- [ ] Canonicalize invocation paths. Make an equal or ancestor pending path absorb the new request as a follower; make an incoming ancestor replace all queued descendants at the earliest displaced FIFO position; leave disjoint entries in FIFO order.
- [ ] Preserve followers for every absorbed or replaced request. Retain absorbed/displaced asynchronous IDs as internal aliases of the compacted scan; synchronous callers wait and return its ordinary terminal success or error, while asynchronous callers keep their original handle and poll the existing shared analyze-job status.
- [ ] Compute effective force as logical OR across the compacted entry and every attached or replaced request.
- [ ] Add deterministic tests for running-scan immutability, follower absorption, ancestor replacement/order, success/error propagation, force OR, post-compaction cap, and retryable rejection of a distinct 33rd pending analyze.

### Notes

Revision boundary: an analyze-only queue satisfying FR-41–FR-43 and AC-50–AC-52. It must be a complete green revision after task 4.15. Do not reintroduce generic job kinds, `detect_communities_async`, community work in this queue, configuration identity/provenance gating, generic status projections, or a `coalesced_by` wire field.

### Trap

Do not compact against the running scan. It has already chosen its path and force; mutating it makes active work and terminal attribution nondeterministic. Compact only pending entries before applying the cap.

### Completion Evidence

Pending — not complete.

## 4.17: Exercise queue semantics through live daemon clients
### Subtasks

- [x] Extend the daemon-proxy test harness with deterministic slow-analysis coordination and independent live client requests, preserving process cleanup on every assertion path.
- [x] Drive pending follower absorption and incoming-ancestor replacement from separate clients; assert the earliest-displaced FIFO position, force OR, and the satisfying terminal result or error for every synchronous follower.
- [x] Fill the pending queue through live clients, prove post-compaction followers do not consume a slot, and assert the distinct 33rd pending analyze receives the retryable queue-full error without starting a worker.
- [x] Start shutdown with queued work and prove the daemon drains the canonical pending scans before runtime cleanup, without orphaned daemon processes.
- [x] Run focused daemon-proxy, full MCP, workspace structural, and hygiene verification.

### Notes

Revision boundary: transport-level acceptance coverage for the committed analyze-only queue, without changing its queue algorithm, MCP response shapes, or daemon lifecycle implementation. The tests must use distinct live proxy clients against one repository-local daemon; direct `ServerInner` tests do not prove the proxy/daemon boundary shares the slot.

### Trap

Do not make these tests pass by issuing requests serially from one client or by inspecting internal queue state. The regression risk is cross-client admission through the daemon transport, so assertions must be driven by observable MCP results, status snapshots, runtime cleanup, and process ownership.

### Completion Evidence

- Verified: 2026-08-14
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `69850ab1a6af0531cbf2d1a0a876ef7a407b72a2`
- Identity recheck: `git rev-parse HEAD` at 2026-08-14 00:00 matched `69850ab1a6af0531cbf2d1a0a876ef7a407b72a2`
- Focused review: `git show 69850ab1a6af0531cbf2d1a0a876ef7a407b72a2`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `69850ab1a6af0531cbf2d1a0a876ef7a407b72a2`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-mcp --test daemon_proxy && cargo test -p code-graph-mcp && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && make verify && make snapshot-clean && git diff --check` | `.` | PASS (`exit 0`) | `PASS: 18 daemon-proxy tests (including all three live queue tests) and the full code-graph-mcp suite passed; formatting and warning-deny clippy passed; verify confirmed clean snapshots and synchronized plugin mirrors; git diff --check reported no whitespace errors. The known query-perf and server doctests remained ignored.` |

### Subtasks

- [ ] Extend the daemon-proxy test harness with deterministic slow-analysis coordination and independent live client requests, preserving process cleanup on every assertion path.
- [ ] Drive pending follower absorption and incoming-ancestor replacement from separate clients; assert the earliest-displaced FIFO position, force OR, and the satisfying terminal result or error for every synchronous follower.
- [ ] Fill the pending queue through live clients, prove post-compaction followers do not consume a slot, and assert the distinct 33rd pending analyze receives the retryable queue-full error without starting a worker.
- [ ] Start shutdown with queued work and prove the daemon drains the canonical pending scans before runtime cleanup, without orphaned daemon processes.
- [ ] Run focused daemon-proxy, full MCP, workspace structural, and hygiene verification.

### Notes

Revision boundary: transport-level acceptance coverage for the committed analyze-only queue, without changing its queue algorithm, MCP response shapes, or daemon lifecycle implementation. The tests must use distinct live proxy clients against one repository-local daemon; direct `ServerInner` tests do not prove the proxy/daemon boundary shares the slot.

### Trap

Do not make these tests pass by issuing requests serially from one client or by inspecting internal queue state. The regression risk is cross-client admission through the daemon transport, so assertions must be driven by observable MCP results, status snapshots, runtime cleanup, and process ownership.

### Completion Evidence

- Verified: 2026-08-14
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `69850ab1a6af0531cbf2d1a0a876ef7a407b72a2`
- Identity recheck: `git rev-parse HEAD` at 2026-08-14 00:00 matched `69850ab1a6af0531cbf2d1a0a876ef7a407b72a2`
- Focused review: `git show 69850ab1a6af0531cbf2d1a0a876ef7a407b72a2`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `69850ab1a6af0531cbf2d1a0a876ef7a407b72a2`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `git show --check 69850ab1a6af0531cbf2d1a0a876ef7a407b72a2` | `.` | PASS (`exit 0`) | `PASS: committed task diff is limited to daemon-proxy live queue coverage and debug-only test coordination, with no whitespace errors; reviewed candidate/final is 69850ab1a6af0531cbf2d1a0a876ef7a407b72a2.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git status --short` | `Source-identity recheck after implementation commit` | PASS | `Only the task lifecycle artifact remains modified; implementation commit is 69850ab1a6af0531cbf2d1a0a876ef7a407b72a2.` |

## Acceptance Criteria
- [ ] **AC-50**: Analyze requests queue without concurrent execution, with at most 32 pending analyze entries after compaction. The running scan is unchanged and a distinct 33rd pending analyze gets a retryable queue-full error. (FR-41)
- [ ] **AC-51**: Canonical-path compaction absorbs a request under an equal/ancestor pending path, replaces queued descendants with an incoming ancestor at the earliest displaced FIFO position, and preserves disjoint FIFO order; configuration identity/provenance does not participate. (FR-42)
- [ ] **AC-52**: Every follower receives the satisfying compacted scan terminal result or error, and no merged force request is lost because effective force is logical OR. No generic projection or `coalesced_by` field is required. (FR-43)
- [ ] **AC-58**: **Deferred.** Generic asynchronous whole-graph jobs, including `detect_communities_async`, are excluded from this phase; a later initiative must specify and plan them. (FR-49)

- [ ] **AC-27**: `make verify` passes. (NFR-04)

## Phase Completion Evidence
Pending — not complete.
