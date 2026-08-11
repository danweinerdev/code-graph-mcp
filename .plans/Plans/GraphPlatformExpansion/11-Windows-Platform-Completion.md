---
title: "Windows Platform Completion"
type: phase
plan: GraphPlatformExpansion
phase: 11
status: deferred
created: 2026-08-11
updated: 2026-08-11
deliverable: "Native Windows support across the completed Linux-MVP seams, including named pipes, ACLs, Windows paths, daemon lifecycle, and CLI parity."
tasks:
  - id: "11.1"
    title: "Activate and repair Windows platform seams"
    status: deferred
    justifies: "NFR-13, AC-60. The Linux MVP retains cfg-gated Windows seams, but cross-compilation cannot establish native tree-sitter toolchain, filesystem, process, or transport correctness."
    verification: "On a native Windows runner with the required MSVC C toolchain, `cargo test --workspace`, workspace clippy with warnings denied, and rustfmt pass; named-pipe/path/process/ACL branches compile and Linux gates remain unchanged."
  - id: "11.2"
    title: "Validate Windows daemon transport, ACL, and lifecycle"
    status: deferred
    depends_on: ["11.1"]
    justifies: "NFR-13, NFR-06, AC-60. Named-pipe ACLs, `icacls`, TCP fallback, replacement, and idle behavior cannot be inferred from Linux UDS tests."
    verification: "Native Windows process tests exercise named-pipe attachment, forced loopback-TCP fallback and credential rotation, simultaneous startup, binary replacement, idle exit/cache reuse, and repository-local cleanup. Deterministic security-descriptor inspection must prove the pipe DACL is restricted to the invoking user/system as intended, and a separately provisioned local account must be denied."
  - id: "11.3"
    title: "Certify Windows paths, feature, and CLI parity"
    status: deferred
    depends_on: ["11.2"]
    justifies: "NFR-13, AC-60. Windows completion must include path normalization and the entire GraphPlatformExpansion surface, not only daemon startup."
    verification: "After phases 1–9 are complete, a checked matrix maps every completed task and acceptance criterion to a native Windows command/evidence row or an explicit not-applicable rationale. It must include daemon, watcher, cache, analyze queue/job, graph-tool, history, CLI, and Windows-path contracts; all applicable rows and `make verify` pass, and AC-60 receives persisted evidence."
---

# Phase 11: Windows Platform Completion

## Overview

Deferred until after the Linux MVP. This phase activates the existing named-pipe, path, process, and ACL seams and makes Windows a supported platform through native evidence. Linux and cross-target results do not substitute for Windows execution.

## 11.1: Activate and repair Windows platform seams

### Subtasks
- [ ] Provision a native Windows runner with Rust and the MSVC tools needed by all six tree-sitter grammars.
- [ ] Run the complete workspace build/test/lint gates without bypassing native build scripts.
- [ ] Repair cfg-gated named-pipe, process, filesystem, and ACL code behind existing seams.
- [ ] Confirm Linux gates remain unchanged after every repair.

### Notes

Revision boundary: one natively buildable Windows workspace with all platform branches compiling and Linux behavior preserved. This is a complete internal platform capability; daemon runtime acceptance remains task 11.2.

### Completion Evidence

Pending — not complete.

### Trap

Do not claim success from `cargo check --target ...` on Linux. The grammar build scripts require native MSVC tooling, and named pipes, ACLs, and Windows path semantics are runtime properties.

## 11.2: Validate Windows daemon transport, ACL, and lifecycle

### Subtasks
- [ ] Exercise named-pipe publication, attachment, multiple clients, and cleanup.
- [ ] Inspect the live pipe security descriptor and assert its owner/DACL excludes unrelated local users.
- [ ] Provision a second local account and prove it cannot attach to the first account's pipe or use its TCP fallback credential.
- [ ] Force named-pipe failure and exercise loopback TCP authentication and credential rotation.
- [ ] Exercise replacement, idle shutdown, final cache save, and warm restart.
- [ ] Pin contender, stale-lock, and runtime-file cleanup behavior on Windows.

### Notes

Revision boundary: native Windows daemon transport/security/lifecycle is fully supported and regression-tested. Keep platform behavior behind the existing listener/client and permission seams rather than branching graph semantics.

### Completion Evidence

Pending — not complete.

## 11.3: Certify Windows paths, feature, and CLI parity

### Subtasks
- [ ] Run Windows-only verbatim-disk-prefix and UNC boundary tests.
- [ ] Exercise watch-event path normalization through real Windows notifications.
- [ ] Build and check an acceptance matrix mapping every completed phases 1–9 task and acceptance criterion to native evidence or a justified not-applicable row.
- [ ] Run all applicable daemon, watcher, cache, analyze queue/job, graph-tool, history, CLI, and Windows-path rows natively.
- [ ] Compare representative CLI machine output with MCP payloads on Windows.
- [ ] Run full `make verify` and persist AC-60 evidence.

### Notes

Revision boundary: the complete GraphPlatformExpansion surface is certified on Windows, including native path and CLI behavior. This task waits for the Linux MVP and CLI phases it certifies.

### Completion Evidence

Pending — not complete.

## Acceptance Criteria

- [ ] **AC-60**: Native Windows workspace, daemon named-pipe/ACL/lifecycle, path, and CLI parity evidence is complete (NFR-13).
- [ ] Linux acceptance suites remain unchanged and green after Windows repairs.
- [ ] `make verify` passes on the native Windows runner.

## Phase Completion Evidence

Pending — not complete.
