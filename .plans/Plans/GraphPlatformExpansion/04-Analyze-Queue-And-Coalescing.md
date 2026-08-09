---
title: "Analyze Queue and Coalescing"
type: phase
plan: GraphPlatformExpansion
phase: 4
status: planned
created: 2026-08-08
updated: 2026-08-09
deliverable: "Analyze requests queue and coalesce by path containment instead of failing on contention, the wire format evolves additively, and the job slot generalizes to cover long-running whole-graph queries."
tasks:
  - id: "4.1"
    title: "Pending queue in AnalyzeSlot and the queued job status"
    status: planned
    justifies: "FR-41, AC-50. AnalyzeSlot holds exactly one current job and one previous_terminal — there is nowhere to put 'admitted but not started', so FR-41's queueing requirement cannot be met by reinterpreting the existing fields."
    verification: "cargo test -p code-graph-tools analyze_job:: — an analyze issued while another is in flight is queued and eventually runs rather than returning 'indexing already in progress' (AC-50); get_status.analyze_job reports status queued for a pending job with progress absent until it starts; the head of pending is promoted on termination under the existing rotation; previous_terminal still preserves exactly one prior job."
  - id: "4.2"
    title: "Path-containment coverage rule with force asymmetry"
    status: planned
    justifies: "FR-42, AC-51. Containment alone is not sufficient — a forced request absorbed into a non-forcing one silently skips the invalidation the caller asked for, which surfaces only as 'force didn't work' long after the fact."
    verification: "cargo test -p code-graph-tools coalesce:: — exhaustive over the four cases: non-forcing nested under queued non-forcing coalesces; non-forcing nested under queued forcing coalesces; forcing nested under queued non-forcing does NOT coalesce and runs in its own right; disjoint paths never coalesce (AC-51). Also covers the reverse-containment direction where the new request is broader than a queued one."
    depends_on: ["4.1"]
  - id: "4.3"
    title: "Coalesced-caller reporting and sync non-blocking rule"
    status: planned
    justifies: "FR-43, NFR-01, AC-52. A caller whose request vanished into another needs its outcome, and a sync caller queued behind N jobs would hit MCP_TOOL_TIMEOUT on any corpus — turning a documented large-repo hazard into an everyday one."
    verification: "cargo test -p code-graph-tools analyze:: plus the snapshot suite — a coalesced caller receives the covering request's outcome with an additive optional field naming it (AC-52); that field is absent, not null, when no coalescing occurred, so non-coalesced bodies stay byte-identical (NFR-01); analyze_codebase's body and analyze_job.result remain structurally identical under one deserializer; a sync request admitted behind pending jobs returns immediately with job_id and status queued rather than blocking."
    depends_on: ["4.2"]
  - id: "4.4"
    title: "Generalize the job slot to long-running queries and add detect_communities_async"
    status: planned
    justifies: "FR-49, AC-58, review follow-up FU-01. detect_communities runs whole-graph label propagation while holding the read lock, and the client tool timeout is wall-clock — spawn_blocking does not extend it, so at UE4 scale the server can finish and the caller still see a timeout with no way to recover the result. Landing it here reuses the slot being reshaped by 4.1 rather than touching AnalyzeSlot and AnalyzeJobView a second time."
    verification: "cargo test -p code-graph-tools job:: — a whole-graph query started asynchronously returns a job id sub-second and reports progress; get_status exposes it under the same polling vocabulary as an analyze job; the terminal result is retrievable and byte-identical to the synchronous response; every individual call is short enough that a wall-clock timeout cannot fire; the synchronous detect_communities still works unchanged for small graphs."
    depends_on: ["4.1"]

---

# Phase 4: Analyze Queue and Coalescing

## Overview

The largest behaviour change in the plan. Today `analyze_codebase` returns `"indexing already in progress"` on contention — rare with one session, routine with a shared daemon. This phase replaces that with a queue that coalesces requests by path containment, and evolves the wire format additively to report it.

Depends on phase 3. Separated from it so a bisect through daemon bring-up does not also cross this change.

## 4.1: Pending queue in AnalyzeSlot and the queued job status

### Subtasks
- [ ] Add `pending: Vec<Arc<AnalyzeJob>>` to `AnalyzeSlot`, ordered by admission
- [ ] Add a `Queued` variant to the job status and map it to the wire string `"queued"`
- [ ] Promote the head of `pending` to `current` when the running job terminates, reusing the existing rotation
- [ ] Expose pending entries as a count plus ids rather than widening `analyze_job` into a list
- [ ] Tests for admission, promotion, rotation, and the queued view

### Notes
Revision boundary: the queue exists and jobs move through it; the coverage rule is not applied yet, so every admitted request runs.

Keeping `analyze_job` a single job matters — every current client reads it as one object, and widening it into a list would break them for no benefit. A count plus ids is enough for a caller to understand the backlog.

`AnalyzeJobView.status` gaining a fourth value is an additive change to a documented enum. Clients matching exhaustively on three values exist by assumption, so CLAUDE.md and the tool description must call it out in this phase, not later.

### Completion Evidence

Pending — not complete.

## 4.2: Path-containment coverage rule with force asymmetry

### Subtasks
- [ ] Implement `covers(x, y)` as a pure function over `(path, force)` pairs
- [ ] Run admission against the running job and every pending entry
- [ ] Attach a coalesced request to its coverer rather than appending it
- [ ] Exhaustive unit tests over the four force/containment cases plus disjoint and reverse-containment

### Notes
Revision boundary: coalescing is live and correct; how a coalesced caller learns about it lands in 4.3.

The rule: **X covers Y when Y's path is at or under X's path, and (X forces or Y does not force).** Writing it as a pure function over pairs is deliberate — it makes the four cases trivially testable without constructing jobs or a daemon.

### Completion Evidence

Pending — not complete.

### Trap
Implementing coverage as path containment alone. It reads as obviously correct and passes any test that does not vary the force flag. The failure it produces — a forced re-index silently absorbed into a plain one, so stale entries survive — appears much later and looks like a cache bug, not a queue bug.

## 4.3: Coalesced-caller reporting and sync non-blocking rule

### Subtasks
- [ ] Add the optional coalescing field to the shared analyze result shape with `skip_serializing_if`
- [ ] Return the coverer's outcome to the coalesced caller
- [ ] Make a sync `analyze_codebase` admitted behind pending jobs return immediately with `job_id` and queued status
- [ ] Preserve immediate-start and coalesced sync behaviour unchanged
- [ ] Update CLAUDE.md and the tool descriptions: retired error, new status value, new field, changed sync blocking
- [ ] Snapshot verification that non-coalesced bodies are byte-identical

### Notes
Revision boundary: the queue is fully observable and the phase's wire evolution is complete and documented.

The field is *absent*, not `null`, when coalescing did not occur — deliberately unlike `analyze_job`/`analyze_job_previous_terminal`, which serialize explicit `null` so clients can distinguish "never happened" from "old server". Here absence and "not coalesced" are the same fact, so an explicit null would add a field to every response to convey nothing.

Adding it to the *shared* shape keeps `analyze_codebase`'s body and `analyze_job.result` structurally identical, preserving the single-deserializer property CLAUDE.md documents.

### Completion Evidence

Pending — not complete.

### Trap
Letting sync `analyze_codebase` block behind the queue because "that's what a queue means". CLAUDE.md already documents `MCP_TOOL_TIMEOUT` killing long sync analyses on large trees; queueing makes wall-clock depend on other sessions' work, so a small repo can now time out because someone else started an index. Return queued instead.

## 4.4: Generalize the job slot to long-running queries and add detect_communities_async

### Subtasks
- [ ] Widen the job slot from analyze-specific to a job kind that covers long-running queries
- [ ] Keep `analyze_job` in `get_status` reporting analyze jobs, so no existing client breaks
- [ ] Add an async form of `detect_communities` on that machinery
- [ ] Ensure the async result is byte-identical to the synchronous response for the same inputs
- [ ] Leave synchronous `detect_communities` working unchanged — it is the right call on a small graph
- [ ] Document both forms and when to reach for each (NFR-11)

### Notes
Revision boundary: any whole-graph query can run as a job, and `detect_communities` uses it.

This arrives from phase 1's review as FU-01. The measured cost is 177 ms on 841 files, which is fine; the concern is UE4/LLVM scale, which nothing has measured. CLAUDE.md already documents the failure mode for `analyze_codebase`: the client gives up on wall-clock while the server runs to completion, and the result is unrecoverable because there is no polling path. `spawn_blocking` does not help — it protects the tokio scheduler, not the client's timer.

Generalize rather than special-case. A second job mechanism beside the analyze one means two vocabularies for the same concept and two things to keep in sync.

### Completion Evidence

Pending — not complete.

### Trap
Reaching for `spawn_blocking` and considering it solved. It moves the work off the async worker and does nothing about the client's wall-clock timeout, which is the actual failure. Only a return-immediately-and-poll shape fixes that.

## Acceptance Criteria

- [ ] **AC-50**: An analyze issued during another is queued and runs, rather than returning the contention error (FR-41).
- [ ] **AC-51**: The coverage rule holds across all four force/containment cases and never coalesces disjoint paths (FR-42).
- [ ] **AC-52**: A coalesced caller receives the covering request's outcome, and the response identifies the coalescing (FR-43).
- [ ] Non-coalesced analyze bodies remain byte-identical; one deserializer still covers both shapes (NFR-01).
- [ ] CLAUDE.md and tool descriptions updated for the retired error, the `"queued"` status, the new optional field, and the sync blocking change (NFR-11).
- [ ] **AC-27**: `make verify` passes (NFR-04).
- [ ] **AC-58**: A whole-graph query runs asynchronously with sub-second calls throughout, so a wall-clock client timeout cannot fire (FR-49).
- [ ] FR-41, FR-42, FR-43, FR-49 realized.

## Phase Completion Evidence

Pending — not complete.
