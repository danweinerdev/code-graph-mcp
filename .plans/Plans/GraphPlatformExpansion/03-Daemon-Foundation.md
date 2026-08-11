---
title: "Daemon Foundation"
type: phase
plan: GraphPlatformExpansion
phase: 3
status: in-progress
created: 2026-08-08
updated: 2026-08-10
deliverable: "A repository-local daemon holding one graph per project root, with the stdio binary attaching to it transparently, an idle timeout, and in-process fallback."
tasks:
  - id: "3.1"
    title: "[daemon] config section, parsed and inert"
    status: complete
    justifies: "FR-10, FR-07. A config surface that lands separately can be reviewed and shipped with zero behaviour change, which is what makes every later task in the phase individually revertible."
    verification: "cargo test -p code-graph-core config:: — a .code-graph.toml with [daemon] enabled and idle_timeout_secs parses; absent section yields documented defaults; idle_timeout_secs = 0 parses as the never-exit sentinel; an unknown key is ignored consistently with the existing sections; no behaviour changes anywhere."
  - id: "3.2"
    title: "Daemon mode: transport, metadata, lockfile, single-instance"
    status: complete
    justifies: "FR-06, FR-07, FR-13, FR-38, FR-39, FR-40, NFR-06, NFR-07, AC-05, AC-08, AC-47, AC-48, AC-49. Without an atomic single-instance protocol, concurrent sessions produce two daemons on one cache; without the transport hierarchy the daemon cannot satisfy NFR-06 on every platform."
    verification: "cargo test -p code-graph-mcp daemon:: plus process integration tests — --serve creates .code-graph/ with socket, daemon.json, and lock and nothing outside the repository (AC-05); named pipe or UDS is used by default and loopback TCP only on fallback, with the fallback reported (AC-47); the TCP transport-auth prelude refuses a client with no or stale secret and the secret file is owner-only (AC-48); a test client reads the transport from metadata and connects first try (AC-49); N simultaneous --serve contenders produce one daemon owner and no leaked contenders, run 20 times (task 3.3 completes AC-08 with real attaching clients); an orphaned socket inode does not block startup and a live one is never unlinked; Ctrl-C removes owned runtime files."
    depends_on: ["3.1"]
  - id: "3.3"
    title: "Proxy mode behind [daemon].enabled, default off"
    status: complete
    justifies: "FR-08, FR-09, FR-14, FR-15, FR-16, AC-04, AC-10, AC-30, AC-31. Shipping the proxy default-off is what lets the byte-pump and shared-state behaviour be exercised in the real harness before it becomes everyone's default path."
    verification: "Integration tests — with the flag on, two clients on one root share an index: one runs analyze_codebase and the other queries without indexing (AC-04); a file edit triggers exactly one watcher and both clients see it (AC-30); session B observes session A's analyze job in get_status including progress and terminal result (AC-31); with the daemon prevented from starting, every existing tool answers in-process and the fallback is reported (AC-10)."
    depends_on: ["3.2"]
  - id: "3.4"
    title: "Flip the default on, with binary-identity replacement"
    status: planned
    justifies: "FR-08, FR-12, NFR-01, AC-09. This is the commit where users get the feature; the binary-SHA gate has to land with it because a dirty SHA never changes between dev builds, so without it an edit-rebuild loop silently talks to the stale daemon."
    verification: "Integration tests plus the full snapshot suite — the existing snapshot suite passes unchanged against a daemon-backed server, proving the proxy is transparent (NFR-01); a client built from a different binary does not attach and the daemon is replaced (AC-09); the replaced daemon persists its cache before exiting and a signal mid-persist completes the write; a daemon that ignores the signal is hard-killed after a bounded grace period and the client falls back in-process."
    depends_on: ["3.3"]
  - id: "3.5"
    title: "Idle timeout and warm-attach measurement"
    status: planned
    justifies: "FR-10, FR-11, NFR-09, AC-06, AC-07, AC-25, AC-26, AC-42. An always-resident daemon per repository is a resource leak users will notice; the measurement is what turns NFR-09's claimed benefit into a verified one rather than an assumption."
    verification: "Integration tests with a short timeout — exits with no clients, does not exit with a client attached, does not exit with an analyze in flight and no clients (AC-06); the timer restarts from zero rather than resuming after an analyze terminates; the cache reflects the last index after idle exit and the next start loads rather than re-indexes (AC-07); the endpoint is unreachable from another machine and unusable by another local user (AC-25); warm-attach time-to-first-query measured on external/ripgrep and external/abseil-cpp shows no corpus-size scaling while cold start does, all four numbers recorded in notes/ (AC-26); Linux, macOS, and Windows each exercised, including the POSIX-only stale-inode path (AC-42)."
    depends_on: ["3.4"]
---

# Phase 3: Daemon Foundation

## Overview

One daemon per project root, discovered by the same upward walk that finds `.code-graph.toml`, with all runtime state under `<project_root>/.code-graph/`. The stdio binary becomes a byte proxy that attaches or spawns, and falls back in-process if it cannot. `ServerInner` is reused verbatim — repository-local scoping (D-0001) means a keyed workspace registry cannot arise.

Independent of phases 1, 2, and 5. Gates phases 4 and 7.

## 3.1: [daemon] config section, parsed and inert

### Subtasks
- [x] Add `DaemonConfig { enabled: bool, idle_timeout_secs: u64 }` to `RootConfig`
- [x] Default `enabled = **false**` in the type — the flip to true is task 3.4's job, and defaulting true here would make the daemon live as soon as 3.2 and 3.3 land, before the binary-identity guard exists
- [x] Document the section in `.code-graph.toml.example` and CLAUDE.md
- [x] Add `.code-graph/` to `.gitignore`
- [x] Config parsing tests

### Notes
Revision boundary: the config surface exists and is documented; no code reads it yet.

The default is `false` deliberately, and 3.4 is the only task that changes it. Defaulting to `true` here — even with the code path unused — means that the moment 3.2 adds `--serve` and 3.3 adds attachment, every session is on the daemon path with no binary-SHA replacement protection, which is precisely the stale-daemon hazard 3.4 exists to close.

Follow the existing section conventions exactly — `#[serde(default)]` on every field, no `deny_unknown_fields`. Note that the singular/plural trap already documented for `extra_ignore` applies here too: a misspelled key silently no-ops, so the example file is load-bearing documentation.

### Completion Evidence

- Verified: 2026-08-10
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `16497990b8acc564441efed3eeee9513e138f6a4`
- Identity recheck: `git rev-parse HEAD` at 2026-08-10T19:35:13Z, matching `16497990b8acc564441efed3eeee9513e138f6a4`
- Focused review: `git show 16497990b8acc564441efed3eeee9513e138f6a4`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `16497990b8acc564441efed3eeee9513e138f6a4`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-core config::` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 60 config tests passed, including absent-section defaults, explicit values, the `0` never-exit sentinel, unknown-key tolerance, and shipped-example parsing. |
| `git diff --check && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Diff whitespace, rustfmt, and workspace clippy with warnings denied all passed. |
| `tmp="/tmp/opencode/code-graph-testdata-cpp-cache-3.1.db"; mv "testdata/cpp/.code-graph-cache.db" "$tmp" && trap 'mv "$tmp" "testdata/cpp/.code-graph-cache.db"' EXIT && make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Full workspace verification, snapshots, and plugin-sync gate passed after temporarily relocating and then restoring a pre-existing orphan fixture cache. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git show 16497990b8acc564441efed3eeee9513e138f6a4` | Complete task commit | PASS | The commit is one bisectable schema-only slice: config type/defaults, public export, focused tests, docs, and ignore rule; no runtime daemon behavior was added. |
| Intent-blind quality review | Current tracked task diff before commit | PASS | No findings; Serde defaults, sentinel behavior, test coverage, and inert-runtime boundary were confirmed. |

## 3.2: Daemon mode: transport, metadata, lockfile, single-instance

### Subtasks
- [x] Add `--serve` mode to the binary; keep the no-arg invocation behaving exactly as today
- [x] Implement transport selection: Unix socket / named pipe first, loopback TCP with a per-instance secret on fallback
- [x] Write `daemon.json` (pid, transport, endpoint, binary SHA, started_at) after the listener is live
- [x] Implement exclusive-create `daemon.lock` with the pid inside; remove on clean exit
- [x] The daemon process acquires and owns the lock; losing `--serve` contenders exit cleanly rather than receiving transferred lock ownership from a parent
- [x] Stale-lock recovery: held `fs2` OS ownership plus pid/start-time/nonce identity; endpoint probing is reserved for safe POSIX socket-inode cleanup
- [x] POSIX: unlink a pre-existing socket inode only while holding the lock and only after a connection attempt is refused
- [x] Serve `CodeGraphServer` over the accepted stream via rmcp's async-rw transport
- [x] Add Ctrl-C cleanup for owned metadata, lock, secret, and local endpoint; task 3.4 extends this path for binary-replacement signals and cache-persist coordination
- [x] Tests per the verification field, including the 20x concurrency run

### Notes
Revision boundary: a daemon can be started explicitly and serves MCP over a socket. Nothing attaches to it automatically yet.

This is the largest task in the plan and the pieces are genuinely coupled — a lockfile without stale detection is unsafe, transport selection without metadata is unusable — so it is one bisectable unit rather than seven. If `make verify` cannot be kept green across the whole bundle in one sitting, land the subtasks as sub-commits within the task rather than splitting the task; the revision boundary is the working daemon, not each part of it.

The workspace's pinned rmcp enables `transport-io`, which provides the async read/write transport used here; no rmcp feature change is needed. Both stdio and socket transports frame through the same `JsonRpcMessageCodec`, which is what makes 3.3's byte pump safe.

Loopback TCP does **not** by itself restrict access to the invoking user — that is why the secret exists on that path and not on the others.

The amended implementation boundary uses daemon-owned locking, a pre-MCP TCP authentication prelude, binary-crate-only platform dependencies, tests under `code-graph-mcp`, Ctrl-C cleanup in this task, and contender-only concurrency here with real client convergence deferred to 3.3.

The amended spec pins the remaining platform details: `CG-AUTH <64 lowercase hex>\n`, a 73-byte cap, a two-second timeout, constant-time comparison, and the daemon's successful transport-auth acknowledgement `CG-OK\n` before MCP framing begins. These lines are transport authentication, not a graph protocol. It also pins safe `sysinfo` identity checks, crash-released `fs2` locking, default named-pipe ACLs, and built-in `icacls` for owner-only Windows token-file access. The binary uses `getrandom` / `sysinfo` / `fs2`, not direct unsafe `windows-sys` calls.

The Linux host has the Rust Windows target installed, but a cross-target check stops in the six tree-sitter grammar build scripts because native MSVC `lib.exe` is unavailable. Native Windows named-pipe and `icacls` runtime exercise remains assigned to task 3.5 / AC-42, matching the workspace's native-per-target build policy.

### Completion Evidence

- Verified: 2026-08-10
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `221b0184fd543f606d48e39880c3fee15c0a5c3b`
- Identity recheck: `git rev-parse HEAD` at 2026-08-10T21:21:25Z, matching `221b0184fd543f606d48e39880c3fee15c0a5c3b`
- Focused review: `git show 221b0184fd543f606d48e39880c3fee15c0a5c3b`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `221b0184fd543f606d48e39880c3fee15c0a5c3b`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-mcp daemon::` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 16 focused daemon tests passed: auth grammar, owner-only paths, symlink rejection, malformed/stale lock takeover, PID/start-time identity, stale-vs-live UDS, endpoint reuse, metadata/token cleanup, and bounded fallback behavior. |
| `cargo test -p code-graph-mcp --test daemon_serve` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 4 process tests passed: UDS MCP round-trip and cleanup, 20 repeated six-contender races with natural loser exit, simultaneous stale-lock recovery, and forced TCP fallback with rejection, MCP round-trip, crash recovery, and token rotation. |
| `cargo test -p code-graph-mcp` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | All 21 binary-crate tests passed, including the unchanged no-argument stdio smoke test advertising 22 tools. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting and workspace lint passed with warnings denied. |
| `tmp="/tmp/opencode/code-graph-testdata-cpp-cache-3.2.db"; mv "testdata/cpp/.code-graph-cache.db" "$tmp" && trap 'mv "$tmp" "testdata/cpp/.code-graph-cache.db"' EXIT && make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Full workspace tests, snapshot cleanliness, and plugin mirror synchronization passed after temporarily relocating and restoring the pre-existing orphan fixture cache. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git show 221b0184fd543f606d48e39880c3fee15c0a5c3b` | Complete task commit | PASS | One explicit-daemon slice: binary dispatch, repository-local transports/state, crash-released ownership, authentication, cleanup, build identity, and focused process coverage; proxy attachment remains absent for task 3.3. |
| Four-lane iterative task review plus final focused quality confirmation | Complete task diff | PASS | Lock TOCTOU, stale token/metadata poisoning, owner-only UDS/runtime paths, endpoint reuse, connection admission, symlink escape, process leaks, and build-SHA invalidation findings were fixed; final confirmation reported no findings. |

### Trap
Testing "is a daemon already running?" by checking whether the socket file exists. It doesn't work in either direction: a crashed daemon leaves the file behind (so existence is a false positive, and on POSIX the leftover inode makes `bind` fail with `EADDRINUSE` forever), and the file appears slightly after the process starts (so absence is a false negative). A connection attempt determines whether that socket inode is live; the held OS lock is the single-instance authority.

## 3.3: Proxy mode behind [daemon].enabled, default off

### Subtasks
- [x] Implement the attach sequence: discover root, read metadata, connect; on failure take the lock and spawn; on losing the lock retry with backoff, re-probing lock staleness each attempt
- [x] Implement the bidirectional byte pump between stdio and the socket
- [x] Implement in-process fallback with a reported diagnostic
- [x] Add `--no-daemon` to force today's behaviour
- [x] Integration tests for shared index, shared watcher, shared analyze slot, and forced fallback

### Notes
Revision boundary: attaching works end to end and is opt-in; the default path is unchanged, so this commit cannot regress anyone.

Watch and the analyze slot need no code change to become shared — they already live in `ServerInner`, and N rmcp services over one `Arc` is the shape `CodeGraphServer` was built for. What changes is that `watch_start`'s "already active" message now means "another session started it", which the tool description should say.

Re-probing lock staleness on each backoff attempt matters: if the winner dies after taking the lock but before writing metadata, a one-shot check strands every loser in the backoff window on in-process fallback.

### Completion Evidence

- Verified: 2026-08-11
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `5bae8b697f8e93c0b65d2f01afc79bbfafc6d92b`
- Identity recheck: `git rev-parse HEAD` at 2026-08-11T00:38:40Z, matching `5bae8b697f8e93c0b65d2f01afc79bbfafc6d92b`
- Focused review: `git show 5bae8b697f8e93c0b65d2f01afc79bbfafc6d92b`; complete task diff reviewed for correctness, scope, tests, maintainability, task boundary, process lifecycle, fallback safety, and transport authentication
- Reviewed candidate / final: `5bae8b697f8e93c0b65d2f01afc79bbfafc6d92b`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-mcp --test daemon_proxy -- --test-threads=2` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 6 process tests passed: default-off/`--no-daemon` preservation, shared index/watch/analyze state across root and nested clients, authenticated TCP metadata attachment plus in-process fallback, slow-contender termination before fallback, established-daemon EOF handling, and simultaneous real-client convergence on one owner with no loser children. The concurrency run was also repeated three times during review. |
| `cargo test -p code-graph-mcp` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 17 daemon unit tests, 6 proxy process tests, 4 explicit-daemon process tests, and the unchanged 22-tool stdio smoke test passed. |
| `cargo test --release -p code-graph-mcp --test daemon_proxy --no-run` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | The release-profile proxy integration target compiled; debug-only slow-start instrumentation is paired with a debug-only test. |
| `cargo test -p code-graph-tools --test snapshot_tools_list && make snapshot-clean` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | All 29 tool-description snapshots passed, including the shared-watch description; no pending snapshots remained. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting and workspace lint passed with warnings denied. |
| `tmp="/tmp/opencode/code-graph-testdata-cpp-cache-3.3.db"; mv "testdata/cpp/.code-graph-cache.db" "$tmp" && trap 'mv "$tmp" "testdata/cpp/.code-graph-cache.db"' EXIT && make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Full workspace tests, response/tool snapshots, snapshot cleanliness, and plugin mirror synchronization passed after temporarily relocating and restoring the pre-existing orphan fixture cache. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git show 5bae8b697f8e93c0b65d2f01afc79bbfafc6d92b` | Complete task commit | PASS | One default-off proxy slice: root discovery, attach/spawn/backoff, UDS/pipe/TCP clients, authenticated TCP acknowledgement, byte pumping, `--no-daemon`, bounded safe fallback, shared-state process coverage, and the agent-facing watch description. Binary replacement and the default-on flip remain absent for task 3.4. |
| Four-lane iterative review plus final focused quality/spec confirmation | Complete task diff | PASS | Initial findings around daemon EOF hangs, slow-start kill races, unacknowledged TCP auth, contender/zombie leaks, fallback timing, snapshot drift, weak concurrency, and default-path regressions were fixed. Final actionable quality findings were closed before the full gate. |

## 3.4: Flip the default on, with binary-identity replacement

### Subtasks
- [ ] Flip the `[daemon].enabled` default from false to true — the single line that makes the daemon everyone's path
- [ ] Compare the client's build SHA against `daemon.json`; treat any `-dirty` SHA as always mismatching
- [ ] On mismatch, signal the daemon and wait for exit, then respawn
- [ ] Install a signal handler in the daemon routing to the same graceful shutdown as idle exit
- [ ] Finish an in-flight cache persist before exiting; hard-kill after a bounded grace period
- [ ] Run the full snapshot suite against a daemon-backed server

### Notes
Revision boundary: the daemon is the default path, with a working replacement protocol.

The shutdown mechanism is a signal, not an MCP call — a hidden control tool would contradict both the no-new-protocol decision and the unchanged-tool-surface guarantee. Because the dirty-SHA rule makes replacement fire on *every* rebuild during development, a bare kill would corrupt the cache routinely rather than rarely.

### Completion Evidence

Pending — not complete.

### Trap
Treating a `-dirty` SHA as matching itself. It looks correct — the strings are equal — but two different dirty builds share a SHA, so the client attaches to a daemon running code it no longer has. This is the highest-frequency failure in the whole phase and it manifests as "my change didn't take effect".

## 3.5: Idle timeout and warm-attach measurement

### Subtasks
- [ ] Implement the idle timer: runs only at zero connections and no analyze in flight
- [ ] Cancel on new attachment; restart from zero after an analyze terminates
- [ ] Persist cache, remove metadata and lock, close the listener before exit
- [ ] Add the security tests for endpoint reachability and permissions
- [ ] Run and record the warm-attach benchmark on two corpora
- [ ] Exercise Linux, macOS, and Windows including the POSIX stale-inode path

### Notes
Revision boundary: the daemon has a complete lifecycle — start, serve, idle out, restart warm.

Restart-from-zero rather than resume is the rule that is easy to get wrong: resuming a partial count means an analyze finishing at T-1s leaves one second of grace, and the next client attaches to a corpse.

AC-26 is a recorded metric, not an automated gate. The pass condition is the *absence of corpus-size scaling* in warm attach, not a millisecond target — a fixed threshold would be flaky across machines.

### Completion Evidence

Pending — not complete.

## Acceptance Criteria

- [x] **AC-04**: Two clients on one daemon share an index; one indexes, the other queries without re-indexing (FR-09).
- [x] **AC-05**: Spawning creates `.code-graph/` and nothing outside the repository (FR-06, FR-07, FR-08).
- [ ] **AC-06**: Idle exit fires with no clients; not with a client attached; not with an analyze in flight (FR-10, FR-11).
- [ ] **AC-07**: The cache reflects the last index after idle exit; the next start loads it (FR-11).
- [x] **AC-08**: Simultaneous starts converge on one daemon with no orphans (FR-13).
- [ ] **AC-09**: A differently-built client does not attach; the daemon is replaced (FR-12).
- [x] **AC-10**: With the daemon unavailable, every tool answers in-process and the fallback is reported (FR-16).
- [ ] **AC-25**: The endpoint is unreachable remotely and unusable by another local user (NFR-06).
- [ ] **AC-26**: Warm attach does not scale with corpus size; measured on two corpora and recorded (NFR-09).
- [x] **AC-30**: One watcher serves all attached clients (FR-14).
- [x] **AC-31**: The analyze job started by one session is observable by another (FR-15).
- [ ] **AC-42**: Linux, macOS, and Windows each exercised, per-platform transport covered (NFR-07).
- [x] **AC-47**: Named-pipe/UDS default with reported loopback-TCP fallback (FR-38).
- [x] **AC-48**: TCP fallback requires a per-instance secret; file is owner-only (FR-39).
- [x] **AC-49**: Clients read the transport from metadata and connect first try (FR-40).
- [ ] **AC-27**: `make verify` passes (NFR-04).
- [ ] FR-06 through FR-16 and FR-38 through FR-40 realized; NFR-06, NFR-07, NFR-09 satisfied; NFR-01 preserved.

## Phase Completion Evidence

Pending — not complete.
