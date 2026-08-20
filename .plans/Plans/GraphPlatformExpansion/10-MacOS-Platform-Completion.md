---
title: "macOS Platform Completion"
type: phase
plan: GraphPlatformExpansion
phase: 10
status: deferred
created: 2026-08-11
updated: 2026-08-11
deliverable: "Native macOS support across the completed Linux-MVP seams, with daemon security/lifecycle and CLI parity exercised rather than inferred."
tasks:
  - id: "10.1"
    title: "Activate and repair macOS platform seams"
    status: deferred
    justifies: "NFR-12, AC-59. Linux MVP code keeps platform-dependent boundaries explicit, but native macOS compilation and runtime behavior are not accepted until this task exercises and repairs them."
    verification: "On a native macOS runner, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo fmt --all --check` pass; shared `cfg(unix)` code is exercised under macOS semantics, any macOS-specific branches compile, and fixes remain isolated from Linux behavior."
  - id: "10.2"
    title: "Validate macOS daemon transport, security, and lifecycle"
    status: deferred
    depends_on: ["10.1"]
    justifies: "NFR-12, NFR-06, AC-59. UDS behavior on Linux cannot prove macOS permissions, process lifecycle, watcher integration, or stale-inode cleanup."
    verification: "Native macOS process tests exercise UDS publication and owner restrictions, simultaneous attachment, replacement, idle exit/cache reuse, authenticated TCP fallback, stale/live socket-inode handling, and mandatory denial from a separately provisioned local account."
  - id: "10.3"
    title: "Certify macOS feature and CLI parity"
    status: deferred
    depends_on: ["10.2"]
    justifies: "NFR-12, AC-59. Platform completion covers the entire GraphPlatformExpansion surface, not only daemon startup."
    verification: "After phases 1–9 are complete, a checked matrix maps every completed task and acceptance criterion to a native macOS command/evidence row or an explicit not-applicable rationale. It must include daemon, watcher, cache, analyze queue/job, graph-tool, history, and CLI contracts; all applicable rows and `make verify` pass, and AC-59 receives persisted evidence."
---

# Phase 10: macOS Platform Completion

## Overview

Deferred until after the Linux MVP. This phase turns the existing POSIX/transport/path/process seams into a supported macOS product surface. Linux results are inputs, not substitutes for native evidence.

**Preparation (2026-08-20, done ahead of hardware):** the complete touch-point map lives at [`notes/10-platform-seams.md`](notes/10-platform-seams.md) — the 8 in-code `SEAM(phase10-macos)` decision markers (greppable), the three-tier platform model, the Linux-only mechanism inventory, the shared-`cfg(unix)` compile-first surface, and the platform-gated test census. The test-side scaffold is `crates/code-graph-mcp/tests/daemon_macos.rs`: 9 `#[ignore]`d stubs gated `#![cfg(target_os = "macos")]`, one per 10.2 verification bullet, each doc-commented with the existing suite whose assertions it mirrors. Note: the 10.2 second-account-denial bullet predates D-0014 (single-local-user scope) — reconcile with the ledger before implementing that stub.

## 10.1: Activate and repair macOS platform seams

### Subtasks
- [ ] Provision or select a native macOS runner with the workspace's required C toolchain.
- [ ] Run the complete workspace build/test/lint gates without suppressing cfg-gated failures.
- [ ] Repair macOS-specific transport, filesystem, process, and permission behavior behind existing seams.
- [ ] Confirm Linux gates remain unchanged after every repair.

### Notes

Revision boundary: one native-macOS-buildable workspace whose existing Linux behavior remains green. The revision implements no new graph feature; it validates shared POSIX code under macOS semantics and repairs any macOS-specific branches as a complete internal capability. Daemon runtime acceptance remains task 10.2.

### Completion Evidence

Pending — not complete.

### Trap

Do not treat a Linux cross-compile as macOS validation. Filesystem permissions, UDS behavior, watcher backends, and process signals require a native runner.

## 10.2: Validate macOS daemon transport, security, and lifecycle

### Subtasks
- [ ] Port the Linux daemon process harness to native macOS without weakening assertions.
- [ ] Exercise UDS modes, owner boundary, TCP fallback authentication, and repository-local state.
- [ ] Exercise replacement and idle shutdown through final cache persistence and warm restart.
- [ ] Exercise both orphaned and live POSIX socket-inode cases.
- [ ] Provision a second local account and prove that it cannot use the first account's daemon endpoint or fallback credential.

### Notes

Revision boundary: native macOS daemon transport and lifecycle are fully supported and regression-tested. Tests must observe the real OS transport and permission behavior; mocked path or socket tests do not complete this task.

### Completion Evidence

Pending — not complete.

## 10.3: Certify macOS feature and CLI parity

### Subtasks
- [ ] Build and check an acceptance matrix mapping every completed phases 1–9 task and acceptance criterion to native evidence or a justified not-applicable row.
- [ ] Run all applicable daemon, watcher, cache, analyze queue/job, graph-tool, history, and CLI rows natively.
- [ ] Compare representative CLI machine output with MCP payloads on macOS.
- [ ] Run full `make verify` and persist AC-59 evidence.
- [ ] Reconcile macOS-specific operational documentation and known limitations.

### Notes

Revision boundary: the complete GraphPlatformExpansion surface is certified on macOS, not merely compilable. This task is intentionally deferred until the Linux MVP and CLI phases it certifies are implemented.

### Completion Evidence

Pending — not complete.

## Acceptance Criteria

- [ ] **AC-59**: Native macOS workspace, daemon, security/lifecycle, and CLI parity evidence is complete (NFR-12).
- [ ] Linux acceptance suites remain in the workspace set with their gates intact after the macOS repairs — verified by diff review (no `#[cfg(unix)]`/`#[cfg(target_os = "linux")]`-gated code block deleted; any gate restructurings and shared-code changes disclosed in the task evidence) — plus a post-repair Linux `make verify` run recorded from a Linux host, or an explicit deferral naming that run as a follow-up. *[Reworded 2026-08-20 before this phase opens, applying phase 11's gate lesson (artifact 22): the original "remain unchanged and green" wording claims a Linux execution a macOS-hosted phase cannot witness; state the diff-reviewed basis and the Linux run separately so each is honestly checkable.]*
- [ ] `make verify` passes on the native macOS runner.

## Phase Completion Evidence

Pending — not complete.
