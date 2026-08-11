---
title: "Code Review: Daemon Foundation Cache-Scavenging Gate"
type: review
status: open
created: 2026-08-11
updated: 2026-08-11
tags: [review, daemon, phase-3, ownership]
related: [Plans/GraphPlatformExpansion]
review_of: "Plans/GraphPlatformExpansion/03-Daemon-Foundation.md"
verdict: Needs changes
reviewed_planning_revision: "881e0bf843431dbbdfdb6ad52558f6c37078cbbe"
review_mode: independent
rev: "98ea430b69d733726b9f7f02aae93c48ac5c5336..fc82fd00f3fd38663f9d09614d6ac3285c5999ff"
findings:
  - id: F-01
    severity: critical
    title: "Replacing the runtime directory permits split daemon ownership"
    status: open
  - id: F-02
    severity: major
    title: "Abandoned metadata publication temps are not scavenged"
    status: open
  - id: F-03
    severity: major
    title: "Windows lock and pipe admission remain incomplete"
    status: rejected
  - id: F-04
    severity: major
    title: "Cross-process direct-mode save scavenging can interfere"
    status: rejected
followups:
  - id: FU-01
    finding: F-01
    summary: "Hold immutable project-root ownership independent of `.code-graph`."
    tracked_in: "3.17"
  - id: FU-02
    finding: F-02
    summary: "Scavenge safe metadata publication temps under lock ownership."
    tracked_in: "3.17"
---

# Code Review: Daemon Foundation Cache-Scavenging Gate

**Reviewed state:** `98ea430b69d733726b9f7f02aae93c48ac5c5336..fc82fd00f3fd38663f9d09614d6ac3285c5999ff`
**Review mode:** Independent lanes

## Findings

### F-01 — Critical: Replacing the runtime directory permits split daemon ownership
**Impugns:** FR-13, AC-08; `crates/code-graph-mcp/src/daemon.rs`
**Scenario:** The current daemon retains the original `.code-graph` descriptor. If that directory is renamed/recreated, a contender opens the replacement and acquires a new `daemon.lock` while the old daemon still owns and serves through the detached directory.
**Why it matters:** Two daemons for one project can mutate and persist competing graph snapshots.
**Recommendation:** Add crash-released ownership anchored to the immutable project-root inode, independent of the replaceable runtime child, and pin runtime replacement convergence.

### F-02 — Major: Abandoned metadata publication temps are not scavenged
**Impugns:** NFR-06; `crates/code-graph-mcp/src/daemon.rs`
**Scenario:** A crash between metadata temp creation and rename leaves `.daemon-<pid>-<nonce>.tmp`; successor cleanup scans only owner-control temps.
**Why it matters:** Repeated publication crashes accumulate repository runtime files.
**Recommendation:** Under authoritative daemon ownership, scavenge exact metadata-temp names while preserving links/directories/unrelated entries.

### F-03 — Major: Windows lock and pipe admission remain incomplete
**Impugns:** Deferred Windows seams
**Scenario:** Native Windows still lacks lock handoff and named-pipe admission acknowledgement.
**Why it matters:** Windows single-instance and saturation behavior are incomplete.
**Recommendation:** Complete and natively validate in Phase 11.

### F-04 — Major: Cross-process direct-mode save scavenging can interfere
**Impugns:** Explicit cache-locking non-goal
**Scenario:** A direct process can scavenge another process's active unique temp.
**Why it matters:** One cross-process save can fail.
**Recommendation:** No Phase 3 change; shared daemon/direct cache locking is explicitly outside the approved design.

## Resolution Log

### F-03 — rejected (2026-08-11)
Native Windows transport, ACL, process, lock-handoff, and acceptance behavior is explicitly deferred to Phase 11. Linux acceptance remains unaffected.

### F-04 — rejected (2026-08-11)
The approved daemon design explicitly excludes cache-file locking between daemon and direct-mode processes. Task 3.16 intentionally serializes only same-process saves; extending ownership across direct processes would reverse that non-goal.
