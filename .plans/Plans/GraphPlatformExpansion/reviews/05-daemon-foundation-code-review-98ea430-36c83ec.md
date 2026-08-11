---
title: "Code Review: Daemon Foundation Verification Stability"
type: review
status: resolved
created: 2026-08-11
updated: 2026-08-11
tags: [review, daemon, phase-3, testing]
related: [Plans/GraphPlatformExpansion]
review_of: "Plans/GraphPlatformExpansion/03-Daemon-Foundation.md"
rev: "98ea430b69d733726b9f7f02aae93c48ac5c5336..36c83ec2893850f0e8ae559cf978c653070072ad"
findings:
  - id: F-01
    severity: major
    title: "Daemon process suites are flaky under Cargo's default parallel runner"
    status: fixed
followups:
  - id: FU-01
    finding: F-01
    summary: "Serialize process-heavy integration scenarios within each test binary."
    tracked_in: "3.9"
---

# Code Review: Daemon Foundation Verification Stability

**Reviewed state:** `98ea430b69d733726b9f7f02aae93c48ac5c5336..36c83ec2893850f0e8ae559cf978c653070072ad`
**Review mode:** Independent lanes

## Findings

### F-01 — Major: Daemon process suites are flaky under Cargo's default parallel runner
**Impugns:** NFR-04, AC-27; `crates/code-graph-mcp/tests/daemon_proxy.rs`; `crates/code-graph-mcp/tests/daemon_serve.rs`
**Scenario:** Process-heavy tests run concurrently, each spawning/replacing multiple daemon binaries. Under load, metadata readiness exceeds fixed deadlines or a sibling scenario observes transient owner churn. The same cases pass alone and with `--test-threads=1`.
**Why it matters:** Standard `cargo test` and `make verify` can fail nondeterministically despite correct product behavior.
**Recommendation:** Serialize scenarios within each process integration binary using a dependency-free global mutex and prove the default runner stable repeatedly.

## Resolution Log

### F-01 — fixed (2026-08-11)
Task 3.9 commit `18dfd7cd4dc98d278eeef68edb53ce011fbcf1da` adds a poison-tolerant per-binary mutex and acquires it first in all 13 proxy and 8 serve process scenarios. Both binaries passed three default-runner repetitions, the package suite passed, and two consecutive `make verify` runs passed (NFR-04, AC-27).

All findings are fixed. A fresh frozen four-lane review is required against the post-task-3.9 range.
