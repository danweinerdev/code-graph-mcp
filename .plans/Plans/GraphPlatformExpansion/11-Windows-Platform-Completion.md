---
title: "Windows Platform Completion"
type: phase
plan: GraphPlatformExpansion
phase: 11
status: complete
created: 2026-08-11
updated: 2026-08-20
deliverable: "Native Windows support across the completed Linux-MVP seams, including named pipes, ACLs, Windows paths, daemon lifecycle, and CLI parity."
tasks:
  - id: "11.1"
    title: "Activate and repair Windows platform seams"
    status: complete
    justifies: "NFR-13, AC-60. The Linux MVP retains cfg-gated Windows seams, but cross-compilation cannot establish native tree-sitter toolchain, filesystem, process, or transport correctness."
    verification: "On a native Windows runner with the required MSVC C toolchain, `cargo test --workspace`, workspace clippy with warnings denied, and rustfmt pass; named-pipe/path/process/ACL branches compile and Linux gates remain unchanged."
  - id: "11.2"
    title: "Validate Windows daemon transport, ACL, and lifecycle"
    status: complete
    depends_on: ["11.1"]
    justifies: "NFR-13, NFR-06, AC-60. Named-pipe ACLs, `icacls`, TCP fallback, replacement, and idle behavior cannot be inferred from Linux UDS tests."
    verification: "Native Windows process tests exercise named-pipe attachment, forced loopback-TCP fallback and credential rotation, simultaneous startup, binary replacement, idle exit/cache reuse, and repository-local cleanup. Deterministic security-descriptor inspection must prove the runtime state is restricted to the invoking user (owner-only DACL, no inherited ACEs). Cross-account checks — including second-local-account denial — are out of scope per D-0014: the daemon serves one local user's sessions in one local project."
  - id: "11.3"
    title: "Certify Windows paths, feature, and CLI parity"
    status: complete
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
- [x] Provision a native Windows runner with Rust and the MSVC tools needed by all six tree-sitter grammars.
- [x] Run the complete workspace build/test/lint gates without bypassing native build scripts.
- [x] Repair cfg-gated named-pipe, process, filesystem, and ACL code behind existing seams.
- [x] Confirm Linux gates remain unchanged after every repair.

### Notes

Revision boundary: one natively buildable Windows workspace with all platform branches compiling and Linux behavior preserved. This is a complete internal platform capability; daemon runtime acceptance remains task 11.2.

The work landed in the pull-forward series `ccd9e11..89a2af2` (2026-08-18, detailed in the pull-forward note above); every phase implemented since (5–9) was BUILT on this runner, so the repairs have been continuously re-verified by every subsequent `make verify`. Linux status, stated precisely (gate artifact 22 corrected the first draft's "cfg-additive throughout" overclaim): no `#[cfg(unix)]` block was deleted; two test suites were deliberately un-gated; `daemon.rs` RESTRUCTURED some unix gates (statement-level → function-level, the UDS bind arm extracted into a cfg'd helper); and two shared-code changes touch Linux behavior — the `gix_tree_path` forward-slash join (a correctness fix on both platforms) and the ~30 shared test-file ports. All diff-visible in the series; a post-repair Linux run has not been executed from this host and is a recorded follow-up.

### Completion Evidence

- Verified: 2026-08-20
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `89a2af2217bb5d87139fd57d27128908deefad66` (pull-forward series `ccd9e115c8b6242b07d00c21f734c808a23e9baa..89a2af2217bb5d87139fd57d27128908deefad66`); continuously re-verified through `ca2e7a847fe64986e56473bb3fee402d629713dd`
- Identity recheck: `git rev-parse HEAD` at 2026-08-20 11:19 matched `ca2e7a847fe64986e56473bb3fee402d629713dd`, which contains the full pull-forward series
- Focused review: the pull-forward series was reviewed as part of the full-branch adversarial review (artifact 14) and re-exercised by every phase 5–9 gate since
- Reviewed candidate / final: `ca2e7a847fe64986e56473bb3fee402d629713dd`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `make verify` (native Windows, MSVC) | `.` | PASS (`exit 0`) | `clippy --workspace --all-targets -- -D warnings, cargo fmt --all --check, full workspace tests (1,935 passed, 0 failed across all test binaries at ca2e7a8), pending-snapshot check, and plugin-mirror sync — all natively, no cross-compilation, all six tree-sitter grammar build scripts compiled by the native MSVC toolchain (rustc 1.94.1, Windows 10.0.26100.9106).` |

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

The work landed in the pull-forward series `ccd9e11..89a2af2` (CG-OK pipe admission, pipe EOF drain, handle-inheritance seal, captured `icacls`, SID-preferring grants, the DACL inspection test, process-level TCP fallback/auth/rotation via the debug-only forcing seam) and has been continuously re-exercised since: phase 7 added the `--attach-only` proxy mode on the same seams, and the CLI's daemon-parity test spawns a real `--serve` daemon over native named pipes.

### Completion Evidence

- Verified: 2026-08-20
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `89a2af2217bb5d87139fd57d27128908deefad66` (pull-forward series); continuously re-verified through `ca2e7a847fe64986e56473bb3fee402d629713dd`
- Identity recheck: `git rev-parse HEAD` at 2026-08-20 11:19 matched `ca2e7a847fe64986e56473bb3fee402d629713dd`
- Focused review: pull-forward series reviewed under artifact 14's full-branch adversarial review; the daemon suites re-run green natively at the current identity
- Reviewed candidate / final: `ca2e7a847fe64986e56473bb3fee402d629713dd`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-mcp` (native Windows) | `.` | PASS (`exit 0`) | `23 unit + 15 daemon_proxy + 9 daemon_serve + 1 smoke, all green natively at ca2e7a8: named-pipe publication/attachment/multi-client/cleanup, CG-OK admission, queue-through-proxy, replacement (grace→drain→hard-kill with owner revalidation), idle exit + final cache save + warm restart, contender convergence, stale-lock recovery via mandatory-lock semantics, graceful stop via shutdown.request, and runtime_directory_dacl_is_restricted_to_the_invoking_user (no inherited ACEs, no broad built-in principals, exactly one grant naming the invoking user). TCP fallback/auth/rotation run at process level via CODE_GRAPH_TEST_FORCE_TCP_ROOT. Cross-account checks out of scope per D-0014.` |

## 11.3: Certify Windows paths, feature, and CLI parity

### Subtasks
- [x] Run Windows-only verbatim-disk-prefix and UNC boundary tests.
- [x] Exercise watch-event path normalization through real Windows notifications.
- [x] Build and check an acceptance matrix mapping every completed phases 1–9 task and acceptance criterion to native evidence or a justified not-applicable row.
- [x] Run all applicable daemon, watcher, cache, analyze queue/job, graph-tool, history, CLI, and Windows-path rows natively.
- [x] Compare representative CLI machine output with MCP payloads on Windows.
- [x] Run full `make verify` and persist AC-60 evidence.

### Notes

Revision boundary: the complete GraphPlatformExpansion surface is certified on Windows, including native path and CLI behavior. This task waits for the Linux MVP and CLI phases it certifies.

The matrix is persisted at `notes/11-windows-certification-matrix.md`: runner/toolchain/workspace identity, the 1,935-test native umbrella, dedicated rows for the Windows-path contracts (verbatim-disk strip, verbatim-UNC passthrough, PathTrie key semantics, watch dispatch boundary, real-notification watch suites, `normalize_user_path`), daemon transport/security/lifecycle, cache v11, queries, history, fingerprints, and CLI parity (the six-test parity suite mechanizes the "representative CLI output vs MCP payloads" comparison — byte equality through the real adapter on the same fixtures), plus a phase-by-phase task-coverage summary and the N/A rows with rationale: the two task-mapping N/As (second-local-account denial per D-0014; design task 7.1 as an artifact) and, added at the gate's cycle-2 repair, the four Linux-scoped acceptance-criteria N/As (AC-25, AC-42, AC-47, AC-48).

### Completion Evidence

- Verified: 2026-08-20
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `ca2e7a847fe64986e56473bb3fee402d629713dd`
- Identity recheck: `git rev-parse HEAD` at 2026-08-20 11:19 matched `ca2e7a847fe64986e56473bb3fee402d629713dd`
- Focused review: the certification matrix (`notes/11-windows-certification-matrix.md`) cross-checked row by row against the native runs recorded in it, all executed on this runner at this identity
- Reviewed candidate / final: `ca2e7a847fe64986e56473bb3fee402d629713dd`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `make verify` (native Windows) | `.` | PASS (`exit 0`) | `1,935 tests passed, 0 failed across all workspace test binaries; clippy -D warnings, fmt, snapshots, plugin mirrors all green (AC-60 umbrella).` |
| Windows-path rows | `.` | PASS | `cargo test -p code-graph-core simplify_ (3, incl. both #[cfg(windows)] pins); -p code-graph-path-trie windows_ (2); -p code-graph-tools canonicalize_event_path (4, incl. the verbatim-strip pin); --test watch_cpp_macro_strip --test watch_race (the two real-ReadDirectoryChangesW suites — both start the production watcher via watch_start; watch_dangling_edges passes natively too but calls try_reindex_file directly and rides the umbrella, per the matrix's corrected citation); --test path_normalization (2).` |
| Feature rows | `.` | PASS | `daemon: 23+15+9+1; vcs-git: 14; blame_symbol: 9; symbol_history: 13; candidate_count: 4; persist: 29; analyze_async_lifecycle: 1; fingerprints: 7+6+4+4+4+4.` |
| CLI parity rows | `.` | PASS | `cargo test -p code-graph-cli: cli (5 — incl. AC-40 byte-identity against a real named-pipe daemon and the attach-only no-contender pin) + parity (6 — AC-11's five response shapes byte-compared between the built code-graph.exe --json output and the real to_call_tool_result adapter path).` |

## Acceptance Criteria

- [x] **AC-60**: Native Windows workspace, daemon named-pipe/ACL/lifecycle, path, and CLI parity evidence is complete (NFR-13). — as amended 2026-08-20 per D-0014 (the amendment note in the spec records both deltas); persisted evidence is the certification matrix at `notes/11-windows-certification-matrix.md`, whose rows the gate's quality lane re-executed and statically verified across both cycles
- [x] Linux acceptance suites remain in the workspace set with their gates intact after the Windows repairs — verified by diff review (no `#[cfg(unix)]`-gated code block deleted; the two deliberate test-suite un-gatings — `#![cfg(unix)]` inner attributes removed so those suites run on both platforms, widening Linux coverage — plus the gate restructurings and the two shared-code changes are all disclosed in 11.1's evidence); a post-repair Linux `make verify` re-run is a recorded follow-up for the next Linux runner session (gate artifact 22), since "green on Linux" cannot be witnessed from this host. *[Reworded 2026-08-20 at the phase gate: the original "remain unchanged and green" claimed a Linux execution this phase never performed.]* — diff-review basis verified by the gate's blind-spots lane against the pull-forward series; the follow-up sits first in artifact 22's list
- [x] `make verify` passes on the native Windows runner. — PASS (`exit 0`), 1,935 passed / 0 failed at `ca2e7a8` (only docs plus one comment-only edit follow it)

## Phase Completion Evidence

- Verified: 2026-08-20
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `385cc31ded8320dd54d0e094299e847bcaba1730` (certification endpoint; code identity = pull-forward series `ccd9e115c8b6242b07d00c21f734c808a23e9baa..89a2af2217bb5d87139fd57d27128908deefad66`)
- Identity recheck: `git rev-parse 385cc31` at 2026-08-20 11:47 matched `385cc31ded8320dd54d0e094299e847bcaba1730`
- Focused review: four-lane frozen gate over `3dc41a9b0a6dc4ca61bcab8cf66dfc5fdb255d7f..385cc31ded8320dd54d0e094299e847bcaba1730` (two cycles; cycle-1 spec-lane findings — the AC-60/D-0014 spec drift, the pipe-SD wording, the Linux-AC honesty — resolved in `385cc31`; cycle-2 sub-material findings repaired at the planning revision `1f819e4`)
- Reviewed candidate / final: `385cc31ded8320dd54d0e094299e847bcaba1730`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `make verify` (native Windows, MSVC) | `.` | PASS (`exit 0`) | `1,935 passed, 0 failed across all workspace test binaries at ca2e7a8 on Windows 10.0.26100.9106 / rustc 1.94.1; clippy -D warnings, fmt, snapshots, plugin mirrors all green. Certification rows re-executed and statically count-verified by the gate's quality lane in both cycles.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `four-lane phase gate` | `frozen 3dc41a9..385cc31, planning 1f819e4, code identity ccd9e11..89a2af2` | PASS | `All four independent lanes returned PASS/Aligned on cycle 2; the AC-60 amendment judged a legitimate reconciliation against D-0014's ledger fields; residual observations recorded as accepted follow-ups in the review artifact, led by the Linux re-verification.` |

### Completed task identities
- `11.1`: `89a2af2217bb5d87139fd57d27128908deefad66` (pull-forward series endpoint)
- `11.2`: `89a2af2217bb5d87139fd57d27128908deefad66` (pull-forward series endpoint)
- `11.3`: `089db2c88ec56fada6ee6a97d6840c3590912e69` (matrix + evidence)

- Final aligned review: `reviews/22-windows-platform-completion-final-review-3dc41a9-385cc31.md`; frozen: `3dc41a9b0a6dc4ca61bcab8cf66dfc5fdb255d7f..385cc31ded8320dd54d0e094299e847bcaba1730`
