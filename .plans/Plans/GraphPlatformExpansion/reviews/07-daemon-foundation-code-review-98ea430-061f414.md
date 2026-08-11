---
title: "Code Review: Daemon Foundation Final Gate"
type: review
status: open
created: 2026-08-11
updated: 2026-08-11
tags: [review, daemon, phase-3, security, concurrency]
related: [Plans/GraphPlatformExpansion]
review_of: "Plans/GraphPlatformExpansion/03-Daemon-Foundation.md"
verdict: Needs changes
reviewed_planning_revision: "a8b48cabebc62d64ce8ec3839152c79c4278f0a5"
review_mode: independent
rev: "98ea430b69d733726b9f7f02aae93c48ac5c5336..061f414916835ecbfa664f59f8dc087181088586"
lane_results:
  - lane: review_plan_drift
    result: PASS/Aligned
    reviewed_identity: "98ea430b69d733726b9f7f02aae93c48ac5c5336..061f414916835ecbfa664f59f8dc087181088586"
    evidence: "Tasks 3.1-3.11, their completion evidence, Linux acceptance boundary, and deferred phases 10/11 match the frozen implementation range."
  - lane: review_quality
    result: FAIL/Needs changes
    reviewed_identity: "98ea430b69d733726b9f7f02aae93c48ac5c5336..061f414916835ecbfa664f59f8dc087181088586"
    evidence: "UDS admission can silently drop a connected 129th client; the lane also raised cache-race and stale-test-root concerns recorded below."
  - lane: review_spec_compliance
    result: PASS/Aligned
    reviewed_identity: "98ea430b69d733726b9f7f02aae93c48ac5c5336..061f414916835ecbfa664f59f8dc087181088586"
    evidence: "Linux UDS/TCP transport, shared state, idle/replacement lifecycle, fallback, permissions, metadata discovery, and deferred native-platform seams satisfy the reviewed FR/NFR/AC set."
  - lane: review_blind_spots
    result: FAIL/Needs changes
    reviewed_identity: "98ea430b69d733726b9f7f02aae93c48ac5c5336..061f414916835ecbfa664f59f8dc087181088586"
    evidence: "Runtime-directory pathname substitution and unbounded path-reopened lock/control records remain exploitable availability and redirection gaps."
findings:
  - id: F-01
    severity: major
    title: "Runtime-directory validation is pathname-TOCTOU vulnerable"
    status: open
  - id: F-02
    severity: major
    title: "Lock and shutdown records use unbounded path-reopened reads"
    status: open
  - id: F-03
    severity: major
    title: "UDS admission saturation masquerades as a successful attachment"
    status: open
  - id: F-04
    severity: major
    title: "In-process fallback can race a live daemon cache writer"
    status: rejected
  - id: F-05
    severity: minor
    title: "Process tests can reuse stale predictable roots after interruption"
    status: open
followups:
  - id: FU-01
    finding: F-01
    summary: "Anchor runtime operations to a verified directory handle."
    tracked_in: "3.12"
  - id: FU-02
    finding: F-02
    summary: "Use one bounded descriptor reader for lock and control records."
    tracked_in: "3.13"
  - id: FU-03
    finding: F-03
    summary: "Require post-admission acknowledgement before proxy attachment succeeds."
    tracked_in: "3.14"
  - id: FU-04
    finding: F-05
    summary: "Make process-test roots collision-safe and fresh."
    tracked_in: "3.14"
---

# Code Review: Daemon Foundation Final Gate

**Reviewed state:** `98ea430b69d733726b9f7f02aae93c48ac5c5336..061f414916835ecbfa664f59f8dc087181088586`
**Review mode:** Independent lanes

## Findings

### F-01 — Major: Runtime-directory validation is pathname-TOCTOU vulnerable
**Impugns:** NFR-06; `crates/code-graph-mcp/src/daemon.rs:80-131`
**Scenario:** In a writable shared project, an actor replaces `.code-graph` after canonical validation but before permission or child-path operations; those operations follow the substituted pathname.
**Why it matters:** The daemon can chmod or mutate fixed-name entries in a directory outside the repository and loses its repository-local isolation boundary.
**Recommendation:** Hold a no-follow verified runtime-directory handle and perform permission and child-entry operations relative to it, with substitution regressions.

### F-02 — Major: Lock and shutdown records use unbounded path-reopened reads
**Impugns:** NFR-06, AC-08; `crates/code-graph-mcp/src/daemon.rs:359-366,457-471,1110-1203,1311-1316`
**Scenario:** A huge sparse regular lock/control record exhausts memory, or a regular entry is replaced by a FIFO between `symlink_metadata` and `fs::read`, blocking startup, attachment, or replacement.
**Why it matters:** Opening a repository or replacing its daemon can hang indefinitely or consume unbounded memory despite task 3.11's metadata bounds.
**Recommendation:** Route every runtime JSON record through a bounded descriptor-based no-follow/nonblocking reader and test oversized and substituted entries.

### F-03 — Major: UDS admission saturation masquerades as a successful attachment
**Impugns:** FR-16; `crates/code-graph-mcp/src/daemon.rs:1648-1669,1769-1797,508-513`
**Scenario:** With 128 admitted services, a 129th UDS connection completes at transport level but is dropped when no semaphore permit exists. The proxy treats connect as success, suppresses pump failure, and exits without serving MCP or falling back.
**Why it matters:** A bounded-load condition becomes a silent client outage instead of bounded retry/fallback behavior.
**Recommendation:** Add a transport admission acknowledgement emitted only after a permit and lifecycle connection guard are secured; require it before proxy success and pin saturation behavior.

### F-04 — Major: In-process fallback can race a live daemon cache writer
**Impugns:** `crates/code-graph-mcp/src/main.rs:35-57`; `crates/code-graph-graph/src/persist/mod.rs`
**Scenario:** A compatible active daemon is temporarily unattachable, so the proxy falls back in-process and both processes can persist distinct snapshots to the same cache.
**Why it matters:** Last-writer-wins persistence can discard one process's scoped graph updates.
**Recommendation:** Disable fallback cache writes while an active owner holds the daemon lock, or coordinate persistence.

### F-05 — Minor: Process tests can reuse stale predictable roots after interruption
**Impugns:** `crates/code-graph-mcp/tests/daemon_proxy.rs:30-70`; `crates/code-graph-mcp/tests/daemon_serve.rs:103-130`
**Scenario:** An interrupted test leaves a PID-and-sequence directory; eventual PID reuse lets a later test reuse stale runtime files and fixtures.
**Why it matters:** Daemon process tests can become environment-dependent and exercise stale state unintentionally.
**Recommendation:** Atomically create randomized roots or remove/refuse a pre-existing candidate before setup while retaining cleanup guards.

## Resolution Log

### F-04 — rejected (2026-08-11)
The quality lane correctly identified a possible last-writer-wins race, but the approved daemon design explicitly excludes shared cache-file locking between daemon and direct/in-process processes. `--no-daemon` already permits the same coexistence, and Phase 3 did not introduce a second cache-ownership contract. Changing fallback persistence semantics would expand scope beyond the governing non-goal rather than repair drift in this phase.
