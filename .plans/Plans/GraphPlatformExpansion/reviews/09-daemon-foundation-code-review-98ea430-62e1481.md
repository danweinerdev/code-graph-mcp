---
title: "Code Review: Daemon Foundation Control-Publication Gate"
type: review
status: open
created: 2026-08-11
updated: 2026-08-11
tags: [review, daemon, phase-3, persistence]
related: [Plans/GraphPlatformExpansion]
review_of: "Plans/GraphPlatformExpansion/03-Daemon-Foundation.md"
verdict: Needs changes
reviewed_planning_revision: "cc4eb03e19b3688402080e8eeceffe06682eed00"
review_mode: independent
rev: "98ea430b69d733726b9f7f02aae93c48ac5c5336..62e14811a184730f928316d19e349992e1a8adc1"
findings:
  - id: F-01
    severity: major
    title: "Abandoned unique cache temps accumulate across crashes"
    status: open
  - id: F-02
    severity: major
    title: "Windows pipe attachment lacks admission acknowledgement"
    status: rejected
followups:
  - id: FU-01
    finding: F-01
    summary: "Serialize saves and scavenge reserved unique cache temps before each save."
    tracked_in: "3.16"
---

# Code Review: Daemon Foundation Control-Publication Gate

**Reviewed state:** `98ea430b69d733726b9f7f02aae93c48ac5c5336..62e14811a184730f928316d19e349992e1a8adc1`
**Review mode:** Independent lanes

## Findings

### F-01 — Major: Abandoned unique cache temps accumulate across crashes
**Impugns:** NFR-06, AC-07; `crates/code-graph-graph/src/persist/mod.rs:171-262`
**Scenario:** A process dies after creating `.code-graph-cache.db.tmp.<pid>.<sequence>`. Normal unwinding never removes it, and later saves only clean the legacy fixed `.tmp` entry before choosing another unique name.
**Why it matters:** Repeated crashes or hard-kills during large cache writes can consume unbounded project disk space.
**Recommendation:** Serialize same-process saves and remove reserved unique temp siblings before allocating the next temp, preserving non-regular and external sentinels.

### F-02 — Major: Windows pipe attachment lacks admission acknowledgement
**Impugns:** Deferred Windows transport seam; `crates/code-graph-mcp/src/daemon.rs`
**Scenario:** Native named-pipe clients can open before server-side lifecycle admission succeeds.
**Why it matters:** Windows saturation/idle races can yield a dead proxy stream.
**Recommendation:** Implement and natively test post-admission pipe acknowledgement in Phase 11.

## Resolution Log

### F-02 — rejected (2026-08-11)
The governing Phase 3 gate is the Linux MVP; native Windows transport semantics and acceptance are explicitly assigned to Phase 11. Linux UDS/TCP admission is acknowledged and tested.
