---
title: "Code Review: Daemon Foundation Post-Hardening Gate"
type: review
status: resolved
created: 2026-08-11
updated: 2026-08-11
tags: [review, daemon, phase-3, lifecycle]
related: [Plans/GraphPlatformExpansion]
review_of: "Plans/GraphPlatformExpansion/03-Daemon-Foundation.md"
verdict: Needs changes
reviewed_planning_revision: "b7a5d6459957bea6611eaadc2bc480c8251f4d85"
review_mode: independent
rev: "98ea430b69d733726b9f7f02aae93c48ac5c5336..5dfb170c82c89d1fd5988235e9b41cd6e475c880"
findings:
  - id: F-01
    severity: minor
    title: "Plan README Phase 3 status is stale"
    status: fixed
  - id: F-02
    severity: major
    title: "Partial shutdown-record publication can wedge replacement"
    status: fixed
  - id: F-03
    severity: major
    title: "Windows pipe attachment lacks admission acknowledgement"
    status: rejected
  - id: F-04
    severity: major
    title: "Windows lock cleanup has no native handoff guarantee"
    status: rejected
  - id: F-05
    severity: minor
    title: "Same-UID UDS leaf substitution can race chmod"
    status: rejected
followups:
  - id: FU-01
    finding: F-02
    summary: "Atomically publish owner control records and recover malformed stale entries."
    tracked_in: "3.15"
---

# Code Review: Daemon Foundation Post-Hardening Gate

**Reviewed state:** `98ea430b69d733726b9f7f02aae93c48ac5c5336..5dfb170c82c89d1fd5988235e9b41cd6e475c880`
**Review mode:** Independent lanes

## Findings

### F-01 — Minor: Plan README Phase 3 status is stale
**Impugns:** `Plans/GraphPlatformExpansion/README.md:87-94`
**Scenario:** The cold-start summary still names task 3.5 revision `73c332f` and says the next action is the final review although tasks 3.6-3.14 are complete.
**Why it matters:** A future session can miss the security and reliability hardening already delivered.
**Recommendation:** Update the current-state row to the latest implementation and actual next gate.

### F-02 — Major: Partial shutdown-record publication can wedge replacement
**Impugns:** FR-12, FR-16; `crates/code-graph-mcp/src/daemon.rs:1408-1427`
**Scenario:** A failed direct write or sync leaves a truncated final `shutdown.request` or `shutdown.ack`. The daemon ignores the malformed record, while later publishers see the existing name and fail parsing it.
**Why it matters:** Binary replacement repeatedly falls back in-process until daemon exit or manual cleanup.
**Recommendation:** Publish owner records through create-new temporary entries and descriptor-relative atomic rename, and recover malformed records only when ownership checks prove they are stale.

### F-03 — Major: Windows pipe attachment lacks admission acknowledgement
**Impugns:** Deferred Windows transport seam; `crates/code-graph-mcp/src/daemon.rs:887-891,2101-2113`
**Scenario:** A named-pipe connection can open and then lose semaphore/lifecycle admission without a prelude, so the proxy treats a dropped stream as attached.
**Why it matters:** Native Windows saturation would produce missing MCP responses.
**Recommendation:** Add the same post-admission prelude and native saturation coverage in Phase 11.

### F-04 — Major: Windows lock cleanup has no native handoff guarantee
**Impugns:** Deferred Windows process/lock seam; `crates/code-graph-mcp/src/daemon.rs:1174-1184`
**Scenario:** The old owner releases before pathname removal, permitting successor ownership to race cleanup under Windows file-sharing semantics.
**Why it matters:** Native Windows could lose authoritative lock-path ownership.
**Recommendation:** Resolve and test native Windows lock handoff in Phase 11.

### F-05 — Minor: Same-UID UDS leaf substitution can race chmod
**Impugns:** `crates/code-graph-mcp/src/daemon.rs:1742-1755`
**Scenario:** A same-UID process replaces the socket leaf between no-follow validation and pathname chmod.
**Why it matters:** Chmod can touch the replacement before publication rejects the inode change.
**Recommendation:** No Phase 3 change; another local UID cannot mutate the 0700 runtime directory, while the same UID already owns and can chmod the target. Preserve inode rejection and revisit only if the threat model expands.

## Resolution Log

### F-01 — fixed (2026-08-11)
The plan README now identifies the post-task-3.14 implementation and task 3.15/fresh gate as the remaining Phase 3 work.

### F-03 — rejected (2026-08-11)
The governing Linux-MVP scope explicitly defers native Windows transport behavior and acceptance to Phase 11. The finding is valid platform work but not a Phase 3 acceptance failure; Phase 11 already owns named-pipe admission and saturation semantics.

### F-04 — rejected (2026-08-11)
The source already marks Windows handoff as deferred, and Phase 11 owns native process/lock behavior. Linux unlink-before-release remains covered and aligned for Phase 3.

### F-05 — rejected (2026-08-11)
Phase 3's local-user boundary is cross-UID. The runtime directory is descriptor-chmodded 0700 before socket bind, preventing the relevant other-UID leaf race. A same-UID actor has equivalent direct authority over the alleged target; post-chmod inode comparison still refuses publication after replacement.

### F-02 — fixed (2026-08-11)
Task 3.15 commit `62e14811a184730f928316d19e349992e1a8adc1` serializes all Linux request/ack mutations through persistent owner-only `shutdown.control.lock`. Publishers write and sync unique descriptor-relative temps before atomic rename, revalidate the active target owner under the control lock, and scavenge crash temps after every main-lock acquisition. Truncated request/ack, concurrent publisher, owner-transition, symlink/hardlink sentinel, fresh-lock scavenging, and end-to-end binary-replacement regressions pass.
