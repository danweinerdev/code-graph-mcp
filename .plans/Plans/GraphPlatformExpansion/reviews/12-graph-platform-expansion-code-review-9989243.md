---
title: "Code Review: GraphPlatformExpansion Phases 1–3 — Graph Queries, Typed Core Layering, Daemon Foundation (adversarial full-range)"
type: review
status: resolved
created: 2026-08-12
updated: 2026-08-12
tags: [review, phase-1, phase-2, phase-3, adversarial, full-range]
related:
  - Plans/GraphPlatformExpansion
  - Specs/GraphPlatformExpansion
  - Designs/GraphQueries
  - Designs/TypedCoreLayering
  - Designs/RepoLocalDaemon
review_of: "Plans/GraphPlatformExpansion"
rev: "9989243"
review_scope: "phases 1-3, frozen range 1db21d6a2e676ddec7f74b265179f5fb95eb25db..99892431e4253f132010753bb0af46a1ac64251a"
findings:
  - id: F-01
    severity: major
    title: "Client and daemon canonicalize the project root with different functions — latent Windows-only daemon-attach failure with no diagnostic"
    status: fixed
  - id: F-02
    severity: major
    title: "finish_proxy swallows mid-session pump errors and exits 0 — daemon death mid-conversation is indistinguishable from clean shutdown"
    status: fixed
  - id: F-03
    severity: minor
    title: "macOS deep-path UDS bind falls back to TCP routinely (no procfd short-alias equivalent); caveat undocumented"
    status: fixed
  - id: F-04
    severity: minor
    title: "fs2 dependency redundant — rustix::fs::flock already available in-tree"
    status: fixed
  - id: F-05
    severity: minor
    title: "CLAUDE.md has no repository-local-daemon section (runtime files, transport fallback, binary-compatibility gate)"
    status: fixed
  - id: F-06
    severity: minor
    title: "build.rs git-SHA generation logic duplicated near-verbatim between code-graph-tools and code-graph-mcp"
    status: fixed
  - id: F-07
    severity: minor
    title: "Unreachable guard `if cap == 0 { continue; }` in Graph::search (queries.rs:446-448)"
    status: fixed
  - id: F-08
    severity: minor
    title: "Dead conjunct `node_count > 1` in detect_degeneracy (community.rs:368)"
    status: fixed
  - id: F-09
    severity: minor
    title: "Fixed-sleep negative assertions in daemon_serve.rs carry 300-500ms margins (informational; act only on observed CI flake)"
    status: rejected
  - id: F-10
    severity: minor
    title: "Scope creep: .claude/router-config.json committed mid-range with no plan task (harmless tooling config)"
    status: rejected
followups: []
---

# Code Review: GraphPlatformExpansion Phases 1–3 — adversarial full-range review

**Reviewed state:** 9989243 (range 1db21d6a2e676ddec7f74b265179f5fb95eb25db..99892431e4253f132010753bb0af46a1ac64251a, 83 commits, 130 files, clean worktree on `feature/shared-process`)
**Lanes:** sdd-planner:drift-detector, sdd-planner:quality-scanner, sdd-planner:spec-compliance, sdd-planner:blind-spot-finder (no project lanes declared). Lane status: OK — all four ran.

## Overall Verdict

**Alignment: Moderate** (worst-of mapping: drift **Strong**, quality **Strong**, spec-compliance **Strong**, blind-spot hidden-risk **Elevated**). Zero Critical findings. Phases 1–3 are faithfully and completely implemented against plan, spec, and designs — the risk that remains sits at the daemon's process boundaries, found only by the diff-only adversarial lane.

## Findings

### F-01 — Major — Client/daemon root canonicalization divergence (Windows)
**Caught by:** blind-spot-finder (unique).
`crates/code-graph-mcp/src/main.rs:33` resolves cwd with raw `std::fs::canonicalize` (can return `\\?\`-verbatim form on Windows) and feeds it through `RootConfig::load` (returns the walked-to ancestor verbatim) into `DaemonPaths::for_root`. The daemon's own startup (`daemon.rs:1567`) canonicalizes via `code_graph_core::paths::canonicalize` (dunce-stripped). When the two forms diverge, client and daemon compute different `.code-graph` runtime directories: the proxy never finds `daemon.json`, exhausts its 4s attach loop, and silently falls back to a fresh in-process server on every invocation — the daemon feature is defeated with only a generic "daemon unavailable" note. Windows-only; the Linux test suite cannot catch it; `tests/daemon_proxy.rs` has no dunce/canonicalize coverage.
**Recommendation:** route `main.rs`'s pre-daemon cwd resolution through `code_graph_core::paths::canonicalize` so both sides agree on `root` bit-for-bit.

### F-02 — Major — Silent exit 0 on mid-session daemon death
**Caught by:** blind-spot-finder (unique).
`finish_proxy` (`daemon.rs:1120-1126`) discards the byte-pump result (`let _ = pump_connection(...)`) and returns `Ok(())`; `main.rs:41` maps that to `exit(0)`. A daemon killed mid-conversation (OOM, kill -9, idle-timeout racing an in-flight call) produces a clean-looking exit with zero stderr breadcrumb — indistinguishable from graceful shutdown for anyone debugging "code-graph went silent mid-task." The design decision not to attempt mid-session fallback is documented in-code; the observability gap it creates is not.
**Recommendation:** eprintln! the swallowed pump error before returning (matching every other daemon failure path, e.g. `daemon.rs:2739`, `:2781`, `:2863`).

### F-03 — Minor — macOS UDS path-length fallback undocumented
**Caught by:** blind-spot-finder (unique).
The `/proc/<pid>/fd` short-alias workaround for the 108-byte `sun_path` limit is `#[cfg(target_os = "linux")]`-only (`daemon.rs:581-588`). Deeply nested macOS checkouts will routinely fail UDS bind and degrade to loopback TCP (logged, so not fully silent) — a routine outcome on macOS that the code elsewhere treats as exceptional. Either extend a short-path technique to macOS or document the caveat in CLAUDE.md's platform limitations. Relevant to the planned Phase 10 (macOS completion).

### F-04 — Minor — Drop `fs2` for `rustix::fs::flock`
**Caught by:** quality-scanner. `fs2 = "0.4"` added solely for advisory locking (13 call sites in `daemon.rs`); `rustix` (already a direct dep with `"fs"`) provides the identical `flock` primitive. Mechanical substitution, same syscall.

### F-05 — Minor — CLAUDE.md missing daemon section
**Caught by:** quality-scanner. Beyond the two-field `[daemon]` TOML table, CLAUDE.md documents nothing about the `.code-graph/` runtime-file inventory, UDS/named-pipe → TCP+auth transport fallback, or the binary-fingerprint compatibility gate (`metadata_compatible`/`sha_compatible_with`) governing attach-vs-replace. A future agent reading cold has no surface short of 4,929 lines of `daemon.rs`. F-03's caveat can land in the same section.

### F-06 — Minor — Duplicated build.rs git-SHA logic
**Caught by:** quality-scanner. `crates/code-graph-tools/build.rs` and `crates/code-graph-mcp/build.rs` carry near-byte-identical SHA/dirty-detection generation; a future fix to one can silently miss the other. Extract into a shared build-time helper.

### F-07 — Minor — Unreachable guard in Graph::search
**Caught by:** quality-scanner. `queries.rs:446-448`: `if cap == 0 { continue; }` is dead — `limit` is normalized 0→20 before the loop and the only limit-0 path (`count_only`) returns earlier. Remove or replace with `debug_assert_ne!`.

### F-08 — Minor — Dead conjunct in detect_degeneracy
**Caught by:** quality-scanner. `community.rs:368`: `node_count > 1` is unconditionally true after the `node_count < 10` early return at 356-358. Drop the conjunct.

### F-09 — Minor (informational) — Tight fixed-sleep negative assertions
**Caught by:** quality-scanner. `daemon_serve.rs:526,545,548,587,631` prove "daemon did NOT idle-exit" via fixed sleeps with 300-500ms margins against 1-2s timeouts (no positive edge exists to poll). Deliberate, isolated trade-off; widen margins only if CI flake is observed.

### F-10 — Minor — Scope creep: router config
**Caught by:** drift-detector. Commit `98ea430` adds `.claude/router-config.json` (Claude Code model-routing config) with no plan task. Harmless tooling interleaved in the range; flagged for completeness only.

## Disagreements

None — no lane contradicted another. One tension surfaced as a question rather than a contradiction (see Open Questions: the `core::` pub-widening vs. zero cross-crate consumers).

## Blind spots only blind-spot-finder caught

F-01, F-02, F-03 — all three of its findings were unique. The three intent-aware lanes independently rated the daemon internals Strong (the lock/replacement/idle machinery survived deliberate adversarial attempts by two lanes); the residual risk is concentrated at the boundaries `main.rs` shares with `daemon.rs` and in the silent-failure observability of the proxy path.

## Open Questions

- **`core::` widening not yet exercised cross-crate** (quality-scanner): `crates/code-graph-mcp` has zero `code_graph_tools::core` references at `9989243`; commit `4690da5` made core entry points `pub` for anticipated daemon-proxy reuse. Paired trap: the hardcoded `indexed = true` in `core::{structure,symbols,query}` adapters is safe only while `server.rs` remains the sole caller gating via `require_indexed()`. Revisit when daemon-proxy wiring lands.
- **`--serve --no-daemon` precedence** (quality-scanner): `--no-daemon` silently wins; contradictory-flag contract undocumented, only reachable by manual invocation.
- **NFR-04 per-commit `make verify`** (spec-compliance): final tree verified green (clippy -D warnings, fmt, snapshots, 464/464 + 177/177 + daemon suites); per-commit history not re-verified.
- **`binary_sha = "unknown"` on git-less builds** (blind-spot-finder): `sha_compatible_with` still discriminates via the content fingerprint (64-bit non-cryptographic hash, documented collision-prone at `daemon.rs:1349-1351`); likely benign, unverified corner.
- **AC-26/AC-43 recorded benchmarks** (spec-compliance): recorded-metric criteria in the debriefs were not independently re-derived; the designs define human-read of the recorded number as the check.

## Resolution Log

All dispositions applied 2026-08-12 on `feature/shared-process` (working tree atop 9989243). Verified: `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo fmt --all --check` clean, `make snapshot-clean` clean, `cargo test -p code-graph-tools` 665/0, `cargo test -p code-graph-graph` 177/0, `cargo test -p code-graph-mcp` all green except one environment-only failure (see New observations).

### F-01 — fixed (2026-08-12)
`main.rs` pre-daemon cwd resolution now routes through `code_graph_core::paths::canonicalize` (same function as `daemon::run`), with a comment pinning the must-match constraint.

### F-02 — fixed (2026-08-12)
`finish_proxy` now `eprintln!`s the swallowed `pump_connection` error ("daemon connection ended mid-session") before returning `Ok(())`; exit code stays 0 by design.

### F-03 — fixed (2026-08-12)
The macOS deep-path UDS→TCP caveat is documented in the new CLAUDE.md daemon section and routed to Phase 10 for any code-level mitigation.

### F-04 — fixed (2026-08-12)
The recommended `rustix::fs::flock` was unavailable cross-platform. The fix uses std file locking instead, dropping `fs2` without a cfg split and documenting the required MSRV change.

### F-05 — fixed (2026-08-12)
CLAUDE.md gained the repository-local daemon runtime model section: runtime files, transport fallback, binary compatibility, failure surface, and path discipline.

### F-06 — fixed (2026-08-12)
Git-SHA build logic is single-sourced in `build-support/git-identity.rs` and included from both build scripts.

### F-07 — fixed (2026-08-12)
The unreachable `cap == 0` branch became an invariant `debug_assert_ne!`.

### F-08 — fixed (2026-08-12)
The dead `node_count > 1` conjunct was removed.

### F-09 — rejected (2026-08-12)
The finding itself directs action only on an observed CI flake; no flake was observed, so widening timing margins is intentionally not planned.

### F-10 — rejected (2026-08-12)
`.claude/router-config.json` is user-owned tooling configuration; its harmless historical scope deviation remains documented and does not require code work.

### New observations from the fix pass (environment, not code defects)

1. `daemon_proxy::ignored_replacement_request_is_hard_killed_and_client_falls_back` **fails in this sandbox at clean HEAD too** (verified via isolated worktree): the hard-kill works, but the killed daemon is orphaned to PID 1 — the agent harness, not a reaping init — so it lingers as a zombie that both `identity_is_alive` and the test's `process_is_alive` read as alive. Passes on any system with a reaping init; matches the known pre-`--init` container issue. No code change made.
2. `testdata_cpp_baseline` fails when a leftover `/src/.code-graph-cache.db` from a previous test run exists: the in-range root `.code-graph.toml` (commit 012bd08) makes `/src` the shared project root, so scoped test invocations merge counts from a polluted shared cache. Passes from a clean state (full suite 665/0). Pre-existing test-isolation hazard worth a future task (e.g. baseline tests setting an isolated project root).
