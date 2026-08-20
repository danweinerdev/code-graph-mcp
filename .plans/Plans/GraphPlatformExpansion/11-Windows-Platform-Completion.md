---
title: "Windows Platform Completion"
type: phase
plan: GraphPlatformExpansion
phase: 11
status: in-progress
created: 2026-08-11
updated: 2026-08-18
deliverable: "Native Windows support across the completed Linux-MVP seams, including named pipes, ACLs, Windows paths, daemon lifecycle, and CLI parity."
tasks:
  - id: "11.1"
    title: "Activate and repair Windows platform seams"
    status: in-progress
    justifies: "NFR-13, AC-60. The Linux MVP retains cfg-gated Windows seams, but cross-compilation cannot establish native tree-sitter toolchain, filesystem, process, or transport correctness."
    verification: "On a native Windows runner with the required MSVC C toolchain, `cargo test --workspace`, workspace clippy with warnings denied, and rustfmt pass; named-pipe/path/process/ACL branches compile and Linux gates remain unchanged."
  - id: "11.2"
    title: "Validate Windows daemon transport, ACL, and lifecycle"
    status: in-progress
    depends_on: ["11.1"]
    justifies: "NFR-13, NFR-06, AC-60. Named-pipe ACLs, `icacls`, TCP fallback, replacement, and idle behavior cannot be inferred from Linux UDS tests."
    verification: "Native Windows process tests exercise named-pipe attachment, forced loopback-TCP fallback and credential rotation, simultaneous startup, binary replacement, idle exit/cache reuse, and repository-local cleanup. Deterministic security-descriptor inspection must prove the runtime state is restricted to the invoking user (owner-only DACL, no inherited ACEs). Cross-account checks — including second-local-account denial — are out of scope per D-0014: the daemon serves one local user's sessions in one local project."
  - id: "11.3"
    title: "Certify Windows paths, feature, and CLI parity"
    status: in-progress
    depends_on: ["11.2"]
    justifies: "NFR-13, AC-60. Windows completion must include path normalization and the entire GraphPlatformExpansion surface, not only daemon startup."
    verification: "After phases 1–9 are complete, a checked matrix maps every completed task and acceptance criterion to a native Windows command/evidence row or an explicit not-applicable rationale. It must include daemon, watcher, cache, analyze queue/job, graph-tool, history, CLI, and Windows-path contracts; all applicable rows and `make verify` pass, and AC-60 receives persisted evidence."
---

# Phase 11: Windows Platform Completion

## Overview

Originally deferred until after the Linux MVP; pulled forward by an explicit
user decision on 2026-08-18 ("daemon mode on Windows needs to be a thing so
that large workspaces can share a graph instance"). This phase activates the
existing named-pipe, path, process, and ACL seams and makes Windows a
supported platform through native evidence. Linux and cross-target results do
not substitute for Windows execution.

### Pull-forward state (2026-08-18; committed as `ccd9e11..89a2af2`)

Landed natively on Windows (task evidence stays pending until this phase's
own gate; the commit series is the durable identity):

- **11.1 substantially done.** `cargo clippy --workspace --all-targets -- -D
  warnings`, `cargo fmt --all --check`, and the full `cargo test --workspace`
  (78 test binaries) pass natively. Repairs: five cfg-orphaned clippy errors
  in `daemon.rs`; Windows lock-recovery `NotFound` retry; mandatory-lock
  semantics (`is_lock_violation`, `still_owned`/`metadata_owner_is_active`/
  `active_lock_identity` forks — a held `daemon.lock` is unreadable on
  Windows, so the lock-violation read failure is the liveness proof and owner
  identity rides `daemon.json`); `gix_tree_path` separator fix in
  `code-graph-vcs-git::blame`; ~30 test files ported off unix-only fixtures
  (verbatim-path canonicalize, 8.3 short-form TEMP, `/`-separator literals,
  drive-colon symbol-ID assumptions, snapshot separator normalization).
- **11.2 partially done.** Named-pipe transport gained the `CG-OK` admission
  prelude (client + server; saturation/idle-race peers now fail attach
  instead of byte-pumping a dead pipe); pipe sessions end on stdin EOF via
  flush + bounded 500ms drain (pipes cannot half-close); the proxy seals
  `HANDLE_FLAG_INHERIT` on its stdio handles before spawning the contender
  (scoped-unsafe `SetHandleInformation`, new windows-only `windows-sys` dep)
  so the daemon no longer retains host↔proxy pipe ends; `icacls` runs with
  captured output (inherited stdio was injecting "processed file" chatter
  into the MCP stdout stream). `daemon_serve` (6) and `daemon_proxy` (15)
  suites un-gated and green natively, including queue-through-proxy,
  replacement, idle lifecycle, stale-lock recovery, and contender
  convergence; Windows graceful stop rides the `shutdown.request` protocol.
- **Also landed (follow-up hardening):** `icacls` grants prefer the current
  user's SID (`whoami /user`, captured; `USERNAME` fallback) — closes the
  domain/AzureAD bare-name ambiguity, verified natively against a
  domain-joined account; a native `daemon_serve` test inspects the runtime
  directory DACL (no inherited ACEs, no broad built-in principals, exactly
  one grant naming the invoking user); liveness probes refresh only the
  target PID instead of full-system scans inside the 50ms poll loops.
- **11.2 in-scope items are now covered:** the loopback-TCP fallback,
  authentication matrix, and credential rotation run at process level on
  both platforms (Unix by occupying the UDS pathname, Windows via the
  debug-only `CODE_GRAPH_TEST_FORCE_TCP_ROOT` seam — per-PID pipe names
  cannot be occupied externally). The second-local-account denial check was
  removed from scope by D-0014 (single local user, local project, multiple
  sessions; no scope expansion without explicit user approval).
  **11.3 untouched** (certification matrix waits on phases 5–9).

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
- [x] Exercise named-pipe publication, attachment, multiple clients, and cleanup.
- [x] Inspect the daemon's runtime-state DACL and assert it is owner-only: no inherited ACEs, no broad built-in principals, exactly one grant naming the invoking user (`runtime_directory_dacl_is_restricted_to_the_invoking_user`).
- ~~Provision a second local account and prove it cannot attach to the first account's pipe or use its TCP fallback credential.~~ Out of scope per D-0014: the daemon serves one local user's sessions in one local project; cross-account isolation is not a claimed guarantee and must not be expanded without explicit user approval.
- [x] Force the loopback-TCP fallback and exercise TCP authentication and credential rotation at process level (Unix occupies the UDS pathname; Windows uses the debug-only `CODE_GRAPH_TEST_FORCE_TCP_ROOT` seam — per-PID pipe names cannot be occupied externally; unit-level pipe-occupation coverage also exists).
- [x] Exercise replacement, idle shutdown, final cache save, and warm restart.
- [x] Pin contender, stale-lock, and runtime-file cleanup behavior on Windows.

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
