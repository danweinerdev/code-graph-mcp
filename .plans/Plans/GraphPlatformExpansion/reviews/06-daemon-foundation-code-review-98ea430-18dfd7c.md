---
title: "Code Review: Daemon Foundation Definitive Gate"
type: review
status: resolved
created: 2026-08-11
updated: 2026-08-11
tags: [review, daemon, phase-3, persistence, security]
related: [Plans/GraphPlatformExpansion]
review_of: "Plans/GraphPlatformExpansion/03-Daemon-Foundation.md"
rev: "98ea430b69d733726b9f7f02aae93c48ac5c5336..18dfd7cd4dc98d278eeef68edb53ce011fbcf1da"
findings:
  - id: F-01
    severity: critical
    title: "Cache temp persistence follows repository-planted links"
    status: fixed
  - id: F-02
    severity: major
    title: "Lock cleanup can unlink a successor's active lock"
    status: fixed
  - id: F-03
    severity: major
    title: "Delayed-persist replacement test begins after persistence completes"
    status: fixed
  - id: F-04
    severity: question
    title: "Shared-watch tool description changes the tools-list payload"
    status: rejected
followups:
  - id: FU-01
    finding: F-01
    summary: "Use safe cache temp replacement and create-new semantics."
    tracked_in: "3.10"
  - id: FU-02
    finding: F-02
    summary: "Unlink owned lock while exclusive ownership is retained."
    tracked_in: "3.10"
  - id: FU-03
    finding: F-03
    summary: "Start replacement from a persistence-admitted marker."
    tracked_in: "3.10"
---

# Code Review: Daemon Foundation Definitive Gate

**Reviewed state:** `98ea430b69d733726b9f7f02aae93c48ac5c5336..18dfd7cd4dc98d278eeef68edb53ce011fbcf1da`
**Review mode:** Independent lanes

## Findings

### F-01 — Critical: Cache temp persistence follows repository-planted links
**Impugns:** NFR-06, AC-07; `crates/code-graph-graph/src/persist/mod.rs:156-187`
**Scenario:** A static `.code-graph-cache.db.tmp` symlink or hardlink exists in the repository root. Final daemon persistence uses `File::create`, truncating the external target before rename.
**Why it matters:** Idle/replacement shutdown can overwrite an arbitrary same-UID file outside the repository.
**Recommendation:** Safely remove the temp directory entry itself, reject directories, and recreate with `create_new` before writing; pin symlink/hardlink sentinels.

### F-02 — Major: Lock cleanup can unlink a successor's active lock
**Impugns:** AC-08; `crates/code-graph-mcp/src/daemon.rs:800-806`
**Scenario:** The exiting owner checks contents, unlocks, then removes the pathname. A contender can acquire and rewrite the old inode between unlock and remove; cleanup then unlinks its active lock and a third contender creates another lock inode.
**Why it matters:** Two daemon owners can coexist and write one cache.
**Recommendation:** On Linux, unlink the still-owned pathname while retaining the exclusive lock, then release; add a deterministic handoff regression.

### F-03 — Major: Delayed-persist replacement test begins after persistence completes
**Impugns:** AC-07, task 3.4; `crates/code-graph-mcp/tests/daemon_proxy.rs:957-1015`; `crates/code-graph-tools/src/core/analyze.rs:829-852`
**Scenario:** The marker is written after `Graph::save`; the test waits for it before spawning replacement.
**Why it matters:** The test cannot detect regressions in replacement waiting for an admitted in-flight cache save.
**Recommendation:** Add a before-delay admission marker distinct from the completion marker and start replacement when admission is observed.

### F-04 — Question: Shared-watch tool description changes the tools-list payload
**Impugns:** NFR-01, task 3.3; `crates/code-graph-tools/src/server.rs:2125`
**Scenario:** The watch description names shared daemon semantics and its snapshot changed.
**Why it matters:** Tool descriptions are a production wire contract.
**Recommendation:** Reconcile whether this is intentional.

## Resolution Log

### F-04 — rejected (2026-08-11)
The wording change is intentional production behavior required by task 3.3: once one `ServerInner` serves multiple sessions, the previous description is misleading and must explain that another session may already own the shared watch. The tool name, arguments, response, and behavior remain compatible; the agent-facing snapshot was deliberately updated and reviewed.

### F-01 — fixed (2026-08-11)
Task 3.10 commit `6d1bbc71230fcc32d77cb9050af7da03eedf906f` safely removes the legacy fixed temp entry, writes through unique create-new sibling temps, cleans only owned candidates, and proves symlink/hardlink sentinel preservation plus concurrent save/loadability (NFR-06, AC-07).

### F-02 — fixed (2026-08-11)
Task 3.10 unlinks the still-owned Unix lock pathname while retaining the exclusive inode lock, then releases it; handoff and non-owner replacement regressions pin successor preservation (AC-08).

### F-03 — fixed (2026-08-11)
Task 3.10 adds a distinct persistence-admitted marker before delay/save while preserving the post-save completion marker. Replacement now starts during the admitted delayed save and proves drain/cache currency (AC-07).

All findings are terminal. A fresh frozen four-lane review is required against the post-task-3.10 range.
