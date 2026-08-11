---
title: "Code Review: Daemon Foundation Post-Fix"
type: review
status: resolved
created: 2026-08-11
updated: 2026-08-11
tags: [review, daemon, phase-3, security]
related: [Plans/GraphPlatformExpansion]
review_of: "Plans/GraphPlatformExpansion/03-Daemon-Foundation.md"
rev: "98ea430b69d733726b9f7f02aae93c48ac5c5336..ae2a3d3b202be25de6049000dd13085ddeb6b6db"
findings:
  - id: F-01
    severity: critical
    title: "Existing daemon lock follows symlinks before truncation"
    status: fixed
  - id: F-02
    severity: major
    title: "Unlocked stale live-process identity blocks authoritative recovery"
    status: fixed
followups:
  - id: FU-01
    finding: F-01
    summary: "Refuse redirected/non-regular existing lock inodes before rewrite."
    tracked_in: "3.8"
  - id: FU-02
    finding: F-02
    summary: "Make acquired OS lock authoritative over stale identity text."
    tracked_in: "3.8"
---

# Code Review: Daemon Foundation Post-Fix

**Reviewed state:** `98ea430b69d733726b9f7f02aae93c48ac5c5336..ae2a3d3b202be25de6049000dd13085ddeb6b6db`
**Review mode:** Independent lanes

## Findings

### F-01 — Critical: Existing daemon lock follows symlinks before truncation
**Impugns:** NFR-06, task 3.2; `crates/code-graph-mcp/src/daemon.rs:1113-1149`
**Scenario:** An untrusted repository contains `.code-graph/daemon.lock` as a symlink to another user-owned file. Recovery opens through the symlink, obtains a lock, and `write_identity` truncates the target.
**Why it matters:** Default daemon startup can overwrite files outside the repository.
**Recommendation:** Open existing Linux lockfiles with no-follow semantics, validate the opened inode as a single-link regular file, restore owner-only mode, and pin sentinel preservation.

### F-02 — Major: Unlocked stale live-process identity blocks authoritative recovery
**Impugns:** AC-08, task 3.2; `crates/code-graph-mcp/src/daemon.rs:1122-1136,1184-1195`
**Scenario:** A crashed daemon leaves an unlocked lock/metadata identity whose pid/start-time now names a live unrelated process. The contender successfully obtains the OS lock but returns no owner because stale text appears alive.
**Why it matters:** Explicit daemon startup remains unavailable until the unrelated process exits; default clients repeatedly delay then fall back.
**Recommendation:** Once exclusive lock acquisition succeeds, stale identity text must not veto takeover. Preserve the actually-held-lock `WouldBlock` check as the single-instance authority.

## Resolution Log

### F-01 — fixed (2026-08-11)
Task 3.8 commit `36c83ec2893850f0e8ae559cf978c653070072ad` opens existing Linux lockfiles with `O_NOFOLLOW`, rejects non-regular and multiply-linked inodes, repairs mode only after exclusive acquisition, and proves symlink/hardlink sentinels remain unchanged (NFR-06).

### F-02 — fixed (2026-08-11)
Task 3.8 removes stale pid/start-time vetoes after exclusive lock acquisition and clears current-looking stale metadata; a genuinely held OS lock still returns no owner (AC-08).

All findings are fixed. A fresh frozen four-lane review is required against the post-task-3.8 range.
