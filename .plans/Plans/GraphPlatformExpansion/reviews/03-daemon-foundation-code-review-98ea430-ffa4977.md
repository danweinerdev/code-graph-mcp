---
title: "Code Review: Daemon Foundation"
type: review
status: resolved
created: 2026-08-11
updated: 2026-08-11
tags: [review, daemon, phase-3]
related: [Plans/GraphPlatformExpansion]
review_of: "Plans/GraphPlatformExpansion/03-Daemon-Foundation.md"
rev: "98ea430b69d733726b9f7f02aae93c48ac5c5336..ffa497717ae237ab9ef27498eabd9a6ee6ddefa4"
findings:
  - id: F-01
    severity: critical
    title: "Daemon can pivot shared state across project roots"
    status: fixed
  - id: F-02
    severity: major
    title: "Default-on daemon leaks from the direct stdio smoke test"
    status: fixed
  - id: F-03
    severity: major
    title: "Unrelated recycled endpoint can preserve dead daemon metadata"
    status: fixed
  - id: F-04
    severity: major
    title: "Forced fallback does not exercise every advertised tool route"
    status: fixed
  - id: F-05
    severity: major
    title: "Equivalent daemon roots can use incompatible Windows path forms"
    status: deferred
  - id: F-06
    severity: major
    title: "Cross-root rejection reads foreign config first"
    status: fixed
followups:
  - id: FU-01
    finding: F-01
    summary: "Bind daemon analyses to the daemon project root and add a two-project regression."
    tracked_in: "3.6"
  - id: FU-02
    finding: F-02
    summary: "Keep direct stdio smoke in-process and verify cleanup."
    tracked_in: "3.6"
  - id: FU-03
    finding: F-03
    summary: "Discard dead-owner metadata despite unrelated endpoint liveness."
    tracked_in: "3.6"
  - id: FU-04
    finding: F-04
    summary: "Exercise all advertised tool routes under forced fallback."
    tracked_in: "3.6"
  - id: FU-05
    finding: F-05
    summary: "Normalize daemon and analyze roots through the same helper."
    tracked_in: "11.1"
  - id: FU-06
    finding: F-06
    summary: "Reject outside-root requests before foreign config discovery."
    tracked_in: "3.7"
---

# Code Review: Daemon Foundation

**Reviewed state:** `98ea430b69d733726b9f7f02aae93c48ac5c5336..ffa497717ae237ab9ef27498eabd9a6ee6ddefa4`
**Review mode:** Independent lanes

## Findings

### F-01 — Critical: Daemon can pivot shared state across project roots
**Impugns:** AC-05, NFR-06, task 3.2; `crates/code-graph-mcp/src/daemon.rs:813`; `crates/code-graph-tools/src/core/analyze.rs:123-156,519-568`
**Scenario:** A daemon rooted at project A accepts `analyze_codebase` for project B. The shared graph/config/cache pointers pivot to B while B's own daemon can write the same cache concurrently.
**Why it matters:** Repository isolation and single-writer cache ownership are lost; competing daemons can persist stale last-writer-wins state.
**Recommendation:** Bind daemon-mode server state to its discovered project root, reject a different discovered root before mutation, and add a two-project process regression.

### F-02 — Major: Default-on daemon leaks from the direct stdio smoke test
**Impugns:** NFR-04, task 3.4; `crates/code-graph-mcp/tests/smoke.rs:93-143`
**Scenario:** The smoke test launches the no-argument binary after daemon mode became default, then kills only the proxy.
**Why it matters:** The daemon and `.code-graph/` state can outlive the test for the default 1,800-second idle period, polluting later tests and developer machines.
**Recommendation:** Launch this direct-wire smoke with `--no-daemon` and assert no daemon runtime is created.

### F-03 — Major: Unrelated recycled endpoint can preserve dead daemon metadata
**Impugns:** AC-08, NFR-06, task 3.2; `crates/code-graph-mcp/src/daemon.rs:1169-1188`
**Scenario:** After a contender acquires the authoritative lock, metadata names a dead owner but its TCP port or UDS pathname now belongs to an unrelated listener. Raw endpoint liveness makes startup abandon ownership repeatedly.
**Why it matters:** Daemon recovery can remain unavailable indefinitely and clients repeatedly incur startup delay before in-process fallback.
**Recommendation:** Dead owner identity must beat raw endpoint liveness after lock acquisition; retain live-owner protection and add the missing post-lock regression.

### F-04 — Major: Forced fallback does not exercise every advertised tool route
**Impugns:** AC-10, task 3.3; `crates/code-graph-mcp/tests/daemon_proxy.rs:500-548`
**Scenario:** Forced fallback checks `tools/list`, `analyze_codebase`, and one symbol query but does not call the other advertised routes.
**Why it matters:** A route-specific adapter or registration regression could leave some tools unavailable while AC-10 remains checked complete.
**Recommendation:** Under forced fallback, invoke every advertised tool name with route-valid arguments and reject any method-not-found response.

### F-05 — Major: Equivalent daemon roots can use incompatible Windows path forms
**Impugns:** NFR-07, task 3.6; `crates/code-graph-mcp/src/daemon.rs:815-816`; `crates/code-graph-tools/src/core/analyze.rs:123,157-168`
**Scenario:** Daemon startup uses `std::fs::canonicalize`, which may retain a Windows verbatim prefix, while analyze uses the workspace `dunce`-based helper that strips a verbatim-disk prefix.
**Why it matters:** The same project can compare unequal and reject every daemon-backed analyze on Windows.
**Recommendation:** Normalize both sides through `code_graph_core::paths::canonicalize`; retain native Windows execution in Phase 11.

### F-06 — Major: Cross-root rejection reads foreign config first
**Impugns:** NFR-06, task 3.6; `crates/code-graph-tools/src/core/analyze.rs:123-168`
**Scenario:** Root equality is checked only after `RootConfig::load` walks and parses the requested path, so a foreign malformed/inaccessible config changes the response before isolation is applied.
**Why it matters:** A root-bound daemon still reads outside its ownership boundary and leaks foreign parse/access behavior.
**Recommendation:** Reject canonical paths outside the bound root before config discovery, then retain the discovered-root equality check for nested project configs.

## Resolution Log

### F-01 through F-04 — fixed (2026-08-11)
Task 3.6 commit `6599c8bc1bd5347fd4843855b6aba93e13cce9aa` binds daemon state to one root, isolates the direct smoke, makes dead owner identity authoritative over recycled endpoints, and invokes all advertised routes in forced fallback. Governing criteria: AC-05, AC-08, AC-10, NFR-06.

### F-01 — fixed (2026-08-11)
Task 3.6 binds daemon-mode `ServerInner` to one project root and rejects foreign sync/async analysis before graph or cache mutation; the two-project process regression proves independent cache ownership (AC-05, NFR-06).

### F-02 — fixed (2026-08-11)
Task 3.6 runs the direct wire smoke with `--no-daemon` in an isolated temporary cwd and asserts no `.code-graph/` runtime remains (NFR-04).

### F-03 — fixed (2026-08-11)
Task 3.6 makes dead recorded owner identity authoritative after OS-lock acquisition and adds a live unrelated TCP-endpoint regression while preserving genuinely-live-owner metadata (AC-08, NFR-06).

### F-04 — fixed (2026-08-11)
Task 3.6 derives all 22 names from `tools/list`, supplies an explicit argument fixture for each, and invokes every route through forced in-process fallback (AC-10).

The focused task review then found F-05/F-06; task 3.7 closed the Linux-relevant ordering defect while the residual native Windows long-path case moved to task 11.1.

### F-05 — deferred (2026-08-11)
Task 3.7 makes ordinary daemon/analyze roots use the same canonicalizer, but Linux cannot validate the remaining Windows-only case where a long descendant retains a verbatim prefix while its shorter ancestor does not. Native path-form repair and evidence are tracked in task 11.1 under NFR-13/AC-60; Phase 3 makes no Windows support claim.

### F-06 — fixed (2026-08-11)
Task 3.7 commit `ae2a3d3b202be25de6049000dd13085ddeb6b6db` rejects canonical paths outside the daemon root before `RootConfig::load`, retains the post-discovery nested-project check, and adds focused no-read/no-mutation plus owned-scope regressions (NFR-06).

All findings are terminal. A fresh frozen four-lane review is still required against the post-fix Phase 3 range.
