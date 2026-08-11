---
title: "Daemon Foundation"
type: phase
plan: GraphPlatformExpansion
phase: 3
status: in-progress
created: 2026-08-08
updated: 2026-08-11
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
    justifies: "FR-06, FR-07, FR-13, FR-38, FR-39, FR-40, NFR-06, NFR-07, AC-05, AC-08, AC-47, AC-48, AC-49. Without atomic single-instance ownership, concurrent sessions corrupt one cache; without a Linux UDS/TCP transport seam, the endpoint cannot remain local and user-restricted."
    verification: "cargo test -p code-graph-mcp daemon:: plus Linux process integration tests — --serve creates .code-graph/ with UDS, daemon.json, and lock and nothing outside the repository (AC-05); UDS is used by default and loopback TCP only on forced fallback, with fallback reported (AC-47); TCP transport authentication refuses absent/stale credentials and owner files are restricted (AC-48); a client reads transport metadata and connects first try (AC-49); simultaneous --serve contenders converge with no leaks; stale/live UDS inode behavior and Ctrl-C cleanup are covered. Deferred platform branches stay behind the same listener/client seam."
    depends_on: ["3.1"]
  - id: "3.3"
    title: "Proxy mode behind [daemon].enabled, default off"
    status: complete
    justifies: "FR-08, FR-09, FR-14, FR-15, FR-16, AC-04, AC-10, AC-30, AC-31. Shipping the proxy default-off is what lets the byte-pump and shared-state behaviour be exercised in the real harness before it becomes everyone's default path."
    verification: "Integration tests — with the flag on, two clients on one root share an index: one runs analyze_codebase and the other queries without indexing (AC-04); a file edit triggers exactly one watcher and both clients see it (AC-30); session B observes session A's analyze job in get_status including progress and terminal result (AC-31); with the daemon prevented from starting, every existing tool answers in-process and the fallback is reported (AC-10)."
    depends_on: ["3.2"]
  - id: "3.4"
    title: "Flip the default on, with binary-identity replacement"
    status: complete
    justifies: "FR-08, FR-12, NFR-01, AC-09. This is the commit where users get the feature; the binary-identity gate has to land with it because a dirty SHA never changes between dev builds, so without the executable fingerprint an edit-rebuild loop silently talks to the stale daemon."
    verification: "Integration tests plus the full snapshot suite — the existing snapshot suite passes unchanged with daemon mode default-on, proving the proxy does not change tool behavior (NFR-01); a client running a byte-distinct executable does not attach and the daemon is replaced (AC-09); the replaced daemon persists its active graph before exiting and an owner-bound control signal mid-persist completes the write; a daemon that ignores the signal is hard-killed after a bounded grace period and the client falls back in-process."
    depends_on: ["3.3"]
  - id: "3.5"
    title: "Idle timeout and warm-attach measurement"
    status: complete
    justifies: "FR-10, FR-11, NFR-09, AC-06, AC-07, AC-25, AC-26, AC-42. An always-resident daemon per repository is a resource leak users will notice; the measurement is what turns NFR-09's claimed benefit into a verified one rather than an assumption."
    verification: "Linux integration tests with a short timeout — exits with no clients, does not exit with a client attached, does not exit with an analyze in flight and no clients (AC-06); the timer restarts from zero rather than resuming after an analyze terminates; the cache reflects the last index after idle exit and the next start loads rather than re-indexes (AC-07); UDS/runtime modes plus loopback TCP credential enforcement satisfy the Linux local-user boundary (AC-25); warm-attach time-to-first-query measured on external/ripgrep and external/abseil-cpp shows no corpus-size scaling while cold start does (AC-26); Linux UDS/TCP/idle/stale-inode paths are exercised while macOS/Windows seams remain deferred (AC-42)."
    depends_on: ["3.4"]
  - id: "3.6"
    title: "Close final daemon review findings"
    status: complete
    justifies: "AC-05, AC-10, NFR-06. The final Phase 3 review found that a daemon could accept an analyze rooted in another project and race that project's daemon cache, a direct smoke test leaked the now-default daemon, stale metadata could be kept alive by an unrelated recycled endpoint, and fallback coverage did not call every advertised tool route."
    verification: "Bind daemon-mode `ServerInner` to its discovered project root and reject cross-project analyze requests before graph/cache mutation; a two-project process test proves the foreign cache is untouched. Make direct stdio smoke explicitly use `--no-daemon` and prove no runtime state/process remains. Once lock ownership is acquired, dead-owner metadata is not preserved merely because an unrelated endpoint accepts connections. In forced in-process fallback, invoke every name returned by `tools/list` with route-valid arguments and assert no method-not-found response. Run `cargo test -p code-graph-mcp -- --test-threads=1`, focused coordinator tests, clippy/rustfmt, and `make verify`."
    depends_on: ["3.5"]
  - id: "3.7"
    title: "Harden daemon root rejection ordering and normalization"
    status: complete
    justifies: "NFR-06, NFR-07. Task 3.6 prevents cross-root mutation, but focused review found that it reads a foreign root's config before rejection and compares roots produced by different canonicalizers, which can reject an equivalent Windows path form."
    verification: "Canonicalize the daemon root through the same `code_graph_core::paths` helper used by analyze requests. Reject an analyze path outside the bound daemon root before loading foreign config, while retaining the post-discovery equality check that rejects nested project configs. Linux tests prove an outside malformed config still returns the root-boundary error without parsing it and same-root/nested-scope requests continue to work; relevant native Windows normalization coverage remains assigned to Phase 11. Run focused daemon tests, rustfmt, clippy, and `make verify`."
    depends_on: ["3.6"]
  - id: "3.8"
    title: "Harden authoritative daemon lock recovery"
    status: complete
    justifies: "NFR-06, AC-08. The post-fix Phase 3 review found that opening a pre-existing lock follows a repository-planted symlink before truncation and that an unlocked stale identity naming a recycled live PID overrides the OS lock, enabling file overwrite and permanent recovery denial."
    verification: "On Linux, existing lock open refuses symlinks/non-regular files without touching their targets, validates the opened inode, and preserves owner-only mode; a malicious lock symlink to an external sentinel leaves the sentinel byte-identical and daemon startup fails safely. Once exclusive OS lock acquisition succeeds, stale on-disk lock/metadata identity never vetoes recovery solely because its pid/start-time appears live; tests pin unlocked-current-identity takeover while an actually held lock still returns no owner. Run daemon unit/process suites, rustfmt, clippy, and `make verify`."
    depends_on: ["3.7"]
  - id: "3.9"
    title: "Serialize daemon process integration tests"
    status: complete
    justifies: "NFR-04, AC-27. The final post-lock review reproduced metadata readiness timeouts and owner churn only when Cargo ran process-heavy daemon tests concurrently; isolated and `--test-threads=1` reruns passed, so the standard `make verify` gate is nondeterministic."
    verification: "Add dependency-free per-test-binary serialization guards to the Linux `daemon_proxy` and `daemon_serve` process suites so their multi-process timing scenarios do not compete with sibling scenarios under Cargo's default runner. `cargo test -p code-graph-mcp --test daemon_proxy` and `--test daemon_serve` each pass repeatedly without `--test-threads=1`; full package tests and two consecutive `make verify` runs pass."
    depends_on: ["3.8"]
  - id: "3.10"
    title: "Close final persistence and lock-handoff findings"
    status: complete
    justifies: "NFR-06, AC-07, AC-08. The definitive review found unlock-before-unlink can remove a successor's active lock, final cache save can follow a repository-planted temp symlink/hardlink, and the delayed-persist replacement test waits for completion rather than proving drain during persistence."
    verification: "On Linux, owned lock cleanup unlinks the pathname while the exclusive lock is still held and a deterministic handoff regression prevents a successor lock from being removed. `Graph::save` removes a pre-existing non-directory temp entry itself and then opens the temp path with create-new semantics, so symlink/hardlink sentinels remain byte-identical; non-regular directory entries fail safely. Debug persistence instrumentation exposes a distinct admitted/before-delay marker while preserving the existing completion marker, and the replacement test starts replacement after admission but before save completion. Run focused graph/daemon tests, full package/workspace gates, and `make verify` twice."
    depends_on: ["3.9"]
  - id: "3.11"
    title: "Harden metadata reads, fallback reporting, and lock waiters"
    status: complete
    justifies: "FR-38, NFR-06, AC-08, AC-47. The final review found static FIFO/symlink/oversized metadata can block startup, spawned-daemon TCP fallback is hidden because stderr is null, and a contender waiting on an inode unlinked during owner cleanup can acquire it and delete a successor's state."
    verification: "Metadata reads reject symlinks/non-regular files and oversized payloads without blocking; FIFO/symlink/large-file tests prove bounded safe fallback. A normal stdio proxy reports metadata-selected TCP fallback after successful attachment. On Unix, after acquiring an existing lock inode, the contender compares its fd dev/inode with the current pathname and retries without cleanup if they differ; a deterministic pre-opened waiter/successor regression proves successor metadata and lock survive. Run full daemon tests and two `make verify` gates."
    depends_on: ["3.10"]
  - id: "3.12"
    title: "Anchor daemon runtime operations to a verified directory"
    status: complete
    justifies: "NFR-06. The final frozen review found a pathname substitution window after `.code-graph` validation, allowing later permission and child-entry operations to escape the repository-local runtime boundary."
    verification: "On Linux, retain a no-follow verified handle to the runtime directory and perform owner-permission and fixed child-entry operations relative to that handle so renaming/replacing `.code-graph` cannot redirect mutation. Deterministic substitution tests preserve external sentinels and fail or retry safely. Run focused daemon tests, rustfmt, clippy, and `make verify`."
    depends_on: ["3.11"]
  - id: "3.13"
    title: "Bound every daemon runtime-record read"
    status: complete
    justifies: "NFR-06, AC-08. Metadata reads are bounded, but lock and shutdown request/ack records still use unbounded or check-then-reopen reads that can hang or exhaust memory."
    verification: "Use one bounded descriptor-based no-follow/nonblocking reader for lock, shutdown request, and shutdown acknowledgement records, including reads from an already-open lock descriptor. Oversized files and regular-to-FIFO/symlink substitutions fail within a bounded interval without mutation or memory growth. Run focused daemon tests, rustfmt, clippy, and `make verify`."
    depends_on: ["3.12"]
  - id: "3.14"
    title: "Acknowledge connection admission and harden process fixtures"
    status: complete
    justifies: "FR-16, NFR-04. The final frozen review found that UDS saturation is accepted at transport level then silently dropped before MCP admission, and interrupted process tests can later reuse predictable stale roots."
    verification: "For Linux UDS, complete an admission prelude only after both the service permit and lifecycle connection guard are secured; the proxy must not treat attachment as established before that acknowledgement. A saturation regression proves the 129th client retries or falls back rather than exiting successfully with no MCP response. Process-test roots are atomically fresh or collision-refusing and retain cleanup. Run daemon unit/process suites, rustfmt, clippy, and two `make verify` runs."
    depends_on: ["3.13"]
  - id: "3.15"
    title: "Atomically publish daemon owner-control records"
    status: complete
    justifies: "FR-12, FR-16. The post-hardening frozen review found that an interrupted direct write can leave a malformed final shutdown request or acknowledgement that neither the daemon nor later replacement attempts can recover."
    verification: "Publish shutdown request/ack records through owner-only create-new temporary children followed by descriptor-relative atomic rename. Malformed existing final records are removed only after active lock identity proves they cannot belong to another owner; exact-owner idempotence remains. Tests pin truncated request and acknowledgement recovery, concurrent publishers, external sentinel preservation, and successful binary replacement. Run daemon unit/process suites, rustfmt, clippy, and two `make verify` runs."
    depends_on: ["3.14"]
  - id: "3.16"
    title: "Scavenge abandoned unique cache temporaries"
    status: complete
    justifies: "NFR-06, AC-07. The final post-control review found that hard-killed cache writers leave uniquely named multi-megabyte temporaries that no later save removes, allowing project-disk growth across repeated crashes."
    verification: "Serialize `Graph::save` calls within a process and, before allocating a new unique temp, remove reserved unique temp sibling entries left by prior writers plus the legacy fixed temp. Never follow symlinks or mutate hardlink targets; refuse/retain directories and unrelated names. Tests pin interrupted-temp cleanup, symlink/hardlink sentinel contents, same-process concurrent saves, no candidate leaks, and final cache loadability. Run persistence tests, daemon replacement tests, workspace lint/format, and two `make verify` runs."
    depends_on: ["3.15"]
  - id: "3.17"
    title: "Anchor daemon ownership to the project root"
    status: planned
    justifies: "FR-13, AC-08, NFR-06. The cache-scavenging gate found that replacing `.code-graph` creates a second lock namespace while the original daemon continues serving through its retained descriptor; metadata publication temps also survive crashes."
    verification: "On Linux, hold a crash-released exclusive ownership lock on the verified project-root directory inode for the daemon lifetime in addition to the runtime-file identity, so replacing `.code-graph` cannot admit a second daemon. Contenders against a replaced runtime detect live root ownership and do not publish. After ownership acquisition, scavenge exact metadata-temp siblings descriptor-relatively while retaining symlink/hardlink/directory/unrelated sentinels. Tests pin runtime rename/recreate convergence, root-lock crash release, no split metadata/cache ownership, metadata-temp cleanup, and normal replacement. Run daemon suites, persistence tests, lint/format, and two `make verify` runs."
    depends_on: ["3.16"]
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
- [x] Implement the Linux UDS-first / loopback-TCP-fallback transport and retain the named-pipe branch behind the deferred platform seam
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

The amended Linux spec pins `CG-AUTH <64 lowercase hex>\n`, a 73-byte cap, a two-second timeout, constant-time comparison, and the daemon's successful `CG-OK\n` acknowledgement before MCP framing. It also pins safe `sysinfo` identity checks and crash-released `fs2` locking. Named-pipe/ACL code remains behind the Windows seam and is not part of Phase 3 acceptance.

The Linux host cannot validate native macOS or Windows behavior; those support gates are now phases 10/11 (AC-59/AC-60), not task 3.5. Cross-target compilation is not accepted as native runtime evidence.

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
- Focused review: `git show 5bae8b697f8e93c0b65d2f01afc79bbfafc6d92b`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
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
| `sdd_validate.py --format json --scope Plans/GraphPlatformExpansion --identity-mode current` plus governing-path diagnostic filter | GraphPlatformExpansion related graph; task 3.3 phase/plan, governing spec, and daemon design | PASS | The validator parsed 60 related artifacts; the scoped governing set reported 0 diagnostics. The repository-wide related graph still reports unrelated legacy-format diagnostics outside the current initiative boundary, consistent with the recorded scoped-validation policy. |

## 3.4: Flip the default on, with binary-identity replacement

### Subtasks
- [x] Flip the `[daemon].enabled` default from false to true — the single line that makes the daemon everyone's path
- [x] Compare the client's build identity against `daemon.json`: clean/dirty SHA plus executable-content fingerprint; a dirty SHA alone never establishes compatibility
- [x] On mismatch, publish an owner-bound repository-local control signal and wait for exit, then respawn
- [x] Route the control signal through the same graceful shutdown path as Ctrl-C and future idle exit
- [x] Finish an in-flight cache persist before exiting; hard-kill after a bounded grace period
- [x] Run the full snapshot suite against a daemon-backed server

### Notes
Revision boundary: the daemon is the default path, with a working replacement protocol.

The shutdown mechanism is an owner-bound file signal under `.code-graph/`, not an MCP call — a hidden control tool would contradict both the no-new-protocol decision and the unchanged-tool-surface guarantee. `shutdown.request`/`shutdown.ack` carry the exact lock identity, so the same mechanism works on POSIX and Windows without unsafe platform APIs. Because executable fingerprints make replacement fire on every byte-distinct rebuild during development, a bare kill would corrupt the cache routinely rather than rarely.

### Completion Evidence

- Verified: 2026-08-11
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `80f92a9d1e00e05aeb20b4a8934d3c68ec80fc0d`
- Identity recheck: `git rev-parse HEAD` at 2026-08-11T02:01:56Z, matching `80f92a9d1e00e05aeb20b4a8934d3c68ec80fc0d`
- Focused review: `git show 80f92a9d1e00e05aeb20b4a8934d3c68ec80fc0d`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `80f92a9d1e00e05aeb20b4a8934d3c68ec80fc0d`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-mcp --test daemon_proxy -- --test-threads=2` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 12 process tests passed: default-on plus both opt-outs, real byte-distinct executable replacement, clean and dirty identity handling, sequential/concurrent owner convergence, acknowledged delayed-persist drain, bounded hard-kill fallback, authenticated TCP attachment, established-daemon EOF, shared state, and contender cleanup. |
| `cargo test -p code-graph-mcp` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 20 daemon unit tests, 12 proxy process tests, 4 explicit-daemon process tests, and the unchanged 22-tool stdio smoke test passed. |
| `cargo test -p code-graph-tools persist_coordinator && cargo test -p code-graph-tools core::watch::tests && cargo test -p code-graph-tools --test watch_race` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Coordinator admission/drain/lost-wake tests, 8 typed watch lifecycle tests, and 3 watcher race tests passed; shutdown waits admitted analyses, persists, live watchers, and detached watch cleanup before its final active-project cache save. |
| `cargo test --release -p code-graph-mcp --test daemon_proxy --no-run` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | The release-profile replacement process target compiled; debug-only delay/ignore instrumentation remains paired with debug-only tests. |
| `cargo test -p code-graph-tools --test snapshot_tools_list && cargo test -p code-graph-tools --test snapshot_responses && make snapshot-clean` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | All 29 tool-description and 59 representative response snapshots passed unchanged; no pending snapshots remained and the tool surface stayed at 22. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && git diff --check` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting, workspace lint with warnings denied, and whitespace validation passed. |
| `tmp="/tmp/opencode/code-graph-testdata-cpp-cache-3.4.db"; mv "testdata/cpp/.code-graph-cache.db" "$tmp" && trap 'mv "$tmp" "testdata/cpp/.code-graph-cache.db"' EXIT && make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Full workspace tests, response/tool snapshots, snapshot cleanliness, and plugin mirror synchronization passed after temporarily relocating and restoring the pre-existing ignored fixture cache. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git show 80f92a9d1e00e05aeb20b4a8934d3c68ec80fc0d` | Complete task commit | PASS | One default-on replacement slice: executable identity metadata, active-lock authorization, request/ack control signal, bounded replacement and fallback, analyze/persist/watch drain coordination, final active-project cache save, truthful config docs, and unchanged MCP descriptors/responses. Idle connection accounting remains absent for task 3.5. |
| Four-lane iterative review plus final focused quality/blind-spot confirmation | Complete task diff | PASS | Findings around dirty replacement storms, cross-build attach races, unacknowledged long drains, lost wakeups, active-analysis and watcher mutation races, wrong-root final saves, stale request poisoning, control-file clobber, replacement oscillation, and default/idle documentation were fixed. The remaining safe-`sysinfo` PID-reuse interval is the accepted cross-platform/no-unsafe limitation already bounded by exact lock identity checks immediately before hard kill. |
| `sdd_validate.py --format json --scope Plans/GraphPlatformExpansion --identity-mode current` plus governing-path diagnostic filter | GraphPlatformExpansion related graph; task 3.4 phase/plan, governing spec, and daemon design | PASS | The validator parsed 60 related artifacts; the scoped governing set reported 0 diagnostics. Unrelated legacy-format findings remain outside the current initiative boundary under the recorded scoped-validation policy. |

### Trap
Treating a `-dirty` SHA as sufficient identity. It looks correct — the strings are equal — but two different dirty builds share a SHA. Require the executable fingerprint too; sessions launched from the exact same dirty executable may share, while a byte-distinct rebuild replaces the daemon. Omitting that second discriminator manifests as "my change didn't take effect".

## 3.5: Idle timeout and warm-attach measurement

### Subtasks
- [x] Implement the idle timer: runs only at zero connections and no analyze in flight
- [x] Cancel on new attachment; restart from zero after an analyze terminates
- [x] Persist cache, remove metadata and lock, close the listener before exit
- [x] Add the security tests for endpoint reachability and permissions
- [x] Run and record the warm-attach benchmark on two corpora
- [x] Exercise Linux UDS/TCP/lifecycle behavior including the POSIX stale-inode path; leave macOS and Windows behind their deferred platform seams

### Notes
Revision boundary: the daemon has a complete lifecycle — start, serve, idle out, restart warm.

Restart-from-zero rather than resume is the rule that is easy to get wrong: resuming a partial count means an analyze finishing at T-1s leaves one second of grace, and the next client attaches to a corpse.

AC-26 is a recorded metric, not an automated gate. The pass condition is the *absence of corpus-size scaling* in warm attach, not a millisecond target — a fixed threshold would be flaky across machines.

### Completion Evidence

- Verified: 2026-08-11
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `73c332f0f38ad4c6ce925fd4ad2aa07b0eba1406`
- Identity recheck: `git rev-parse 73c332f` at 2026-08-11T04:15:40Z, matching `73c332f0f38ad4c6ce925fd4ad2aa07b0eba1406`
- Focused review: `git show 73c332f0f38ad4c6ce925fd4ad2aa07b0eba1406`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `73c332f0f38ad4c6ce925fd4ad2aa07b0eba1406`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-mcp -- --test-threads=1` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 26 daemon unit tests, 12 proxy process tests, 8 explicit-daemon process tests, and the unchanged 22-tool smoke test passed. Idle coverage includes zero clients, attached clients, analyze-in-flight, restart-from-zero, cache reuse, zero sentinel, unauthenticated TCP, and cleanup. |
| `cargo test -p code-graph-tools server::tests` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Coordinator tests cover generation-based restart, attach-vs-expiry, connection/analyze admission, zero sentinel, and lost-wake-safe drain behavior. |
| `cargo build --release -p code-graph-mcp` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | The Linux release binary used for AC-26 built successfully from the task implementation revision. |
| `python3 scripts/bench-daemon-attach.py --binary target/release/code-graph-mcp --corpus external/ripgrep --repetitions 5 --json` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Warm median 0.278334 s over 100 indexed files; cold median 0.327747 s. All samples and graph counts are recorded in `notes/03-daemon-foundation.md`. |
| `python3 scripts/bench-daemon-attach.py --binary target/release/code-graph-mcp --corpus external/abseil-cpp --repetitions 5 --json` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Warm median 0.278876 s over 849 indexed files; cold median 0.424713 s. Warm latency stayed flat while cold work increased. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && make snapshot-clean` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting, lint, and snapshot cleanliness passed. |
| `tmp="/tmp/opencode/code-graph-testdata-cpp-cache-3.5.db"; mv "testdata/cpp/.code-graph-cache.db" "$tmp" && trap 'mv "$tmp" "testdata/cpp/.code-graph-cache.db"' EXIT && make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Full Linux workspace tests, snapshots, and plugin-sync gate passed after temporarily relocating and restoring the pre-existing ignored fixture cache. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git show 73c332f0f38ad4c6ce925fd4ad2aa07b0eba1406` | Complete task commit | PASS | One Linux-MVP lifecycle slice: connection/analyze generation accounting, atomic idle claim, transport guard integration, graceful final save/cleanup, security-mode coverage, process tests, docs, and reproducible benchmark harness. macOS/Windows remain outside this revision behind existing seams. |
| Four-lane iterative review plus final quality/blind-spot confirmation | Complete task diff | PASS | Timer races, unauthenticated TCP accounting, cache-load evidence, flaky timing margins, benchmark root isolation, query scaling, state restoration, request deadlines, and TCP acknowledgement ordering were reviewed and fixed. |
| Linux permission and transport inspection | UDS/runtime/control files and forced TCP fallback | PASS | Runtime is `0700`; UDS and owner files are `0600`; TCP metadata is loopback-only; missing, wrong, and stale credentials are rejected. This is the Linux enforcement criterion; native macOS/Windows behavior is carried by phases 10/11. |

## 3.6: Close final daemon review findings

### Subtasks
- [x] Bind daemon-mode server state to one immutable project root and reject foreign sync/async analyze jobs before graph/cache mutation
- [x] Keep the direct stdio smoke explicitly in-process and isolated from the caller's working directory
- [x] Make dead owner identity authoritative over an unrelated accepting endpoint after lock acquisition
- [x] Exercise all 22 advertised tool routes through forced in-process fallback

### Notes
Revision boundary: the four findings from the first frozen Phase 3 review are closed as one daemon-isolation and process-hygiene hardening slice. Follow-up task 3.7 handles two additional rejection-order/path-normalization findings discovered by focused review without rewriting this immutable task commit.

### Completion Evidence

- Verified: 2026-08-11
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `6599c8bc1bd5347fd4843855b6aba93e13cce9aa`
- Identity recheck: `git rev-parse HEAD` at 2026-08-11T04:52:44Z, matching `6599c8bc1bd5347fd4843855b6aba93e13cce9aa`
- Focused review: `git show 6599c8bc1bd5347fd4843855b6aba93e13cce9aa`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `6599c8bc1bd5347fd4843855b6aba93e13cce9aa`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-mcp -- --test-threads=1` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 28 daemon unit tests, 13 proxy process tests, 8 explicit-daemon process tests, and the direct smoke passed. Coverage includes two-root sync/async rejection, all-tool fallback routing, recycled endpoint cleanup, and no-daemon smoke isolation. |
| `cargo test -p code-graph-tools server::tests` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 50 server/coordinator tests passed. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting and workspace lint passed with warnings denied. |
| `tmp="/tmp/opencode/code-graph-testdata-cpp-cache-3.6.db"; mv "testdata/cpp/.code-graph-cache.db" "$tmp" && trap 'mv "$tmp" "testdata/cpp/.code-graph-cache.db"' EXIT && make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Full workspace tests, snapshot cleanliness, and plugin mirror synchronization passed after relocating and restoring the pre-existing ignored fixture cache. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git show 6599c8bc1bd5347fd4843855b6aba93e13cce9aa` | Complete task commit | PASS | The diff contains only immutable daemon-root binding, pre-mutation rejection, stale-runtime owner handling, process hygiene, and focused regressions for the original four findings. |
| Focused semantic review | Complete task diff plus callers | PASS | Original findings F-01 through F-04 are closed; two newly discovered rejection-order/path-normalization items are tracked in task 3.7 rather than hidden or folded into this immutable commit. |

## 3.7: Harden daemon root rejection ordering and normalization

### Subtasks
- [x] Normalize daemon startup root with `code_graph_core::paths::canonicalize`
- [x] Reject paths outside the bound root before loading their configuration
- [x] Preserve post-discovery rejection for nested project configurations
- [x] Add focused ordering and same-root regressions

### Notes
Revision boundary: root isolation is enforced before foreign reads and compares canonical paths produced by one shared helper. Native Windows execution remains Phase 11 evidence, but this task must not knowingly compare incompatible path representations.

### Completion Evidence

- Verified: 2026-08-11
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `ae2a3d3b202be25de6049000dd13085ddeb6b6db`
- Identity recheck: `git rev-parse HEAD` at 2026-08-11T05:08:30Z, matching `ae2a3d3b202be25de6049000dd13085ddeb6b6db`
- Focused review: `git show ae2a3d3b202be25de6049000dd13085ddeb6b6db`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `ae2a3d3b202be25de6049000dd13085ddeb6b6db`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools daemon_root_boundary_precedes_foreign_config_and_accepts_owned_scopes` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | The focused test proves outside malformed config is rejected before parsing or mutation, same-root and nested scopes sharing the root remain valid, and a nested project config remains a distinct rejected root. |
| `cargo test -p code-graph-mcp -- --test-threads=1` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 28 daemon unit, 13 proxy process, 8 serve process, and 1 smoke test passed sequentially after the task commit. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting and workspace lint passed with warnings denied. |
| `tmp="/tmp/opencode/code-graph-testdata-cpp-cache-3.7.db"; mv "testdata/cpp/.code-graph-cache.db" "$tmp" && trap 'mv "$tmp" "testdata/cpp/.code-graph-cache.db"' EXIT && make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Full workspace tests, snapshots, and plugin mirror checks passed after relocating and restoring the pre-existing ignored fixture cache. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git show ae2a3d3b202be25de6049000dd13085ddeb6b6db` | Complete task commit | PASS | One focused boundary slice: shared canonicalizer at daemon startup, pre-config outside-root rejection, retained nested-project equality gate, and regression coverage. |
| Focused final quality review | Exact task commit plus callers/tests | PASS | Linux MVP root ordering and canonicalization behavior aligned. Native Windows long-path representation remains explicitly deferred to Phase 11 rather than inferred from Linux. |

## 3.8: Harden authoritative daemon lock recovery

### Subtasks
- [x] Open existing Linux lockfiles without following symlinks and reject non-regular or multiply-linked inodes
- [x] Reapply owner-only mode after acquiring the lock and before rewriting recovered state
- [x] Make successfully acquired OS lock ownership authoritative over stale pid/start-time contents and metadata
- [x] Add malicious symlink/hardlink/nonregular, unlocked-live-identity, and held-lock regressions

### Notes
Revision boundary: repository-planted lock entries cannot redirect writes outside `.code-graph/`, and crash-released OS ownership — not stale process identity text — decides recovery. A direct `libc` crate use for `O_NOFOLLOW` is allowed only in the binary crate; it adds no native library and does not widen unsafe code.

### Completion Evidence

- Verified: 2026-08-11
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `36c83ec2893850f0e8ae559cf978c653070072ad`
- Identity recheck: `git rev-parse HEAD` at 2026-08-11T05:38:17Z, matching `36c83ec2893850f0e8ae559cf978c653070072ad`
- Focused review: `git show 36c83ec2893850f0e8ae559cf978c653070072ad`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `36c83ec2893850f0e8ae559cf978c653070072ad`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-mcp daemon:: -- --test-threads=1` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 32 daemon unit tests passed, including symlink/hardlink/nonregular refusal with untouched sentinels, mode repair, current-looking stale identity takeover, held-lock exclusion, and stale/live UDS handling. |
| `cargo test -p code-graph-mcp -- --test-threads=1` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 32 daemon unit, 13 proxy process, 8 serve process, and 1 smoke test passed. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting and workspace lint passed with warnings denied. |
| `tmp="/tmp/opencode/code-graph-testdata-cpp-cache-3.8.db"; mv "testdata/cpp/.code-graph-cache.db" "$tmp" && trap 'mv "$tmp" "testdata/cpp/.code-graph-cache.db"' EXIT && make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Full workspace tests, snapshots, and plugin mirror checks passed after relocating and restoring the pre-existing ignored fixture cache. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git show 36c83ec2893850f0e8ae559cf978c653070072ad` | Complete task commit | PASS | One Linux lock-recovery slice: no-follow/regular/single-link validation, post-lock owner-mode repair, authoritative stale takeover, direct libc constant dependency, and regressions. |
| Focused final quality review | Exact task commit plus lock callers/tests | PASS | Static repository-planted redirects and stale PID-reuse denial are closed under the owner-only runtime threat boundary; no unsafe code or native library was added. |

## 3.9: Serialize daemon process integration tests

### Subtasks
- [x] Add one dependency-free serialization guard per daemon process-test binary
- [x] Acquire the guard at every process-heavy test entry
- [x] Run each integration binary repeatedly under Cargo's default runner
- [x] Run full package tests and `make verify` twice

### Notes
Revision boundary: default `cargo test` and `make verify` no longer depend on host scheduling luck. Serialization is scoped to process integration binaries; unit tests remain parallel.

### Completion Evidence

- Verified: 2026-08-11
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `18dfd7cd4dc98d278eeef68edb53ce011fbcf1da`
- Identity recheck: `git rev-parse HEAD` at 2026-08-11T06:07:04Z, matching `18dfd7cd4dc98d278eeef68edb53ce011fbcf1da`
- Focused review: `git show 18dfd7cd4dc98d278eeef68edb53ce011fbcf1da`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `18dfd7cd4dc98d278eeef68edb53ce011fbcf1da`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-mcp --test daemon_proxy` (three consecutive runs) | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0` each) | All 13 proxy scenarios passed under Cargo's default runner in 78.09s, 78.74s, and 77.99s. |
| `cargo test -p code-graph-mcp --test daemon_serve` (three consecutive runs) | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0` each) | All 8 serve scenarios passed under Cargo's default runner in 66.17s, 66.12s, and 66.25s. |
| `cargo test -p code-graph-mcp` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 32 daemon unit, 13 proxy process, 8 serve process, and 1 smoke test passed with the default runner. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting and workspace lint passed with warnings denied. |
| `tmp="/tmp/opencode/code-graph-testdata-cpp-cache-3.9.db"; mv "testdata/cpp/.code-graph-cache.db" "$tmp" && trap 'mv "$tmp" "testdata/cpp/.code-graph-cache.db"' EXIT && make verify && make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Two consecutive full workspace, snapshot, and plugin-mirror gates passed after relocating and restoring the pre-existing ignored fixture cache. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git show 18dfd7cd4dc98d278eeef68edb53ce011fbcf1da` | Complete task commit | PASS | Only the two Linux process integration binaries changed; all 21 process tests acquire a full-scope per-binary mutex guard, with poison recovery and no production timeout/behavior edits. |
| Focused final quality review | Exact task commit | PASS | Every process scenario is serialized as its first statement; unit tests remain parallel and no dependency was added. |

## 3.10: Close final persistence and lock-handoff findings

### Subtasks
- [x] Remove owned Linux lock pathname before releasing its exclusive lock
- [x] Harden cache temp creation against planted symlink/hardlink/nonregular entries and concurrent ordinary saves
- [x] Split persistence admission and completion test markers
- [x] Make replacement start during the admitted delayed save and add focused regressions

### Notes
Revision boundary: daemon shutdown handoff keeps one authoritative lock, final cache persistence cannot redirect writes outside the project cache path, and the replacement drain test observes the intended in-flight interval.

### Completion Evidence

- Verified: 2026-08-11
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `6d1bbc71230fcc32d77cb9050af7da03eedf906f`
- Identity recheck: `git rev-parse HEAD` at 2026-08-11T07:03:22Z, matching `6d1bbc71230fcc32d77cb9050af7da03eedf906f`
- Focused review: `git show 6d1bbc71230fcc32d77cb9050af7da03eedf906f`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `6d1bbc71230fcc32d77cb9050af7da03eedf906f`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-graph persist::` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 24 persistence tests passed: legacy temp recovery, symlink/hardlink sentinel preservation, directory refusal, candidate collision retry, concurrent unique-temp saves, atomic loadability, and no temp leaks. |
| `cargo test -p code-graph-mcp --test daemon_proxy replacement_waits_for_delayed_persist_before_runtime_cleanup -- --exact` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Replacement starts after persistence admission while completion is absent, waits through the delay/save, then observes a current loadable cache and completion marker. |
| `cargo test -p code-graph-mcp` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 34 daemon unit, 13 proxy process, 8 serve process, and 1 smoke test passed under the default runner. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting and workspace lint passed with warnings denied. |
| `tmp="/tmp/opencode/code-graph-testdata-cpp-cache-3.10.db"; mv "testdata/cpp/.code-graph-cache.db" "$tmp" && trap 'mv "$tmp" "testdata/cpp/.code-graph-cache.db"' EXIT && make verify && make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Two consecutive full workspace, snapshot, and plugin-mirror gates passed after relocating and restoring the pre-existing ignored fixture cache. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git show 6d1bbc71230fcc32d77cb9050af7da03eedf906f` | Complete task commit | PASS | One persistence/handoff slice: Unix unlink-before-unlock, unique create-new cache temps with cleanup/collision handling, updated mmap safety rationale, and true in-flight replacement instrumentation. |
| Focused final quality review | Exact task commit plus callers/tests | PASS | Lock handoff, concurrent cache saves, planted legacy temp entries, mmap stability, and marker sequencing align under the accepted threat boundary. |

## 3.11: Harden metadata reads, fallback reporting, and lock waiters

### Subtasks
- [x] Bound and type-check daemon metadata reads before parsing
- [x] Report metadata-selected TCP fallback to normal proxy clients
- [x] Revalidate acquired existing-lock inode against the current pathname before cleanup
- [x] Add malicious metadata and pre-opened-waiter handoff regressions

### Notes
Revision boundary: static repository entries cannot block metadata discovery, fallback reporting reaches the actual client, and stale waiters cannot mutate a successor daemon's runtime state.

### Completion Evidence

- Verified: 2026-08-11
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `061f414916835ecbfa664f59f8dc087181088586`
- Identity recheck: `git rev-parse HEAD` at 2026-08-11T08:46:26Z, matching `061f414916835ecbfa664f59f8dc087181088586`
- Focused review: `git show 061f414916835ecbfa664f59f8dc087181088586`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `061f414916835ecbfa664f59f8dc087181088586`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-mcp daemon:: -- --test-threads=1` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 38 daemon tests passed, including bounded directory/symlink/oversized metadata rejection and detached-lock successor preservation. |
| `cargo test -p code-graph-mcp --test daemon_proxy tcp_metadata_attachment_and_start_failure_fallback_are_safe -- --exact` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Forced TCP attachment reports the metadata-selected fallback exactly once; in-process fallback reporting remains distinct. |
| `cargo test -p code-graph-mcp` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 38 daemon unit, 13 proxy process, 8 serve process, and 1 smoke test passed. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting and workspace lint passed with warnings denied. |
| `tmp="/tmp/opencode/code-graph-testdata-cpp-cache-3.11.db"; mv "testdata/cpp/.code-graph-cache.db" "$tmp" && trap 'mv "$tmp" "testdata/cpp/.code-graph-cache.db"' EXIT && make verify && make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Two consecutive full workspace, snapshot, and plugin-mirror gates passed after relocating and restoring the pre-existing ignored fixture cache. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git show 061f414916835ecbfa664f59f8dc087181088586` | Complete task commit | PASS | One metadata/handoff slice: bounded no-follow reads at discovery, preparation, and cleanup; TCP fallback reporting; fd/path inode revalidation and deterministic successor-state regression. |
| Focused final quality review | Exact task commit plus all metadata callers | PASS | Unsafe static metadata entries cannot block, fallback reporting reaches the client, and detached waiters retry without successor cleanup. |

## 3.12: Anchor daemon runtime operations to a verified directory

### Subtasks
- [x] Introduce a Linux no-follow runtime-directory handle
- [x] Route permission and fixed child-entry operations through the verified handle
- [x] Add deterministic directory-substitution and external-sentinel regressions

### Notes

Revision boundary: Linux daemon runtime mutation remains anchored to the originally validated repository-local directory even when its pathname is concurrently replaced.

### Completion Evidence

- Verified: 2026-08-11
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `201e2f7454a310393c6ef8c88d33b3759a8d0965`
- Identity recheck: `git rev-parse HEAD` at 2026-08-11T17:00:45Z, matching `201e2f7454a310393c6ef8c88d33b3759a8d0965`
- Focused review: `git show 201e2f7454a310393c6ef8c88d33b3759a8d0965`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `201e2f7454a310393c6ef8c88d33b3759a8d0965`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-mcp daemon:: -- --test-threads=1` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 44 daemon tests passed, including absent-runtime lazy anchoring, project/runtime substitution, retained lock cleanup, metadata publication, UDS self-descriptor attachment, and socket sentinel preservation. |
| `cargo test -p code-graph-mcp --test daemon_serve && cargo test -p code-graph-mcp --test daemon_proxy` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | All 8 explicit-daemon and 13 proxy process scenarios passed. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting and workspace lint passed with warnings denied. |
| `tmp="/tmp/opencode/code-graph-testdata-cpp-cache-3.12.db"; mv "testdata/cpp/.code-graph-cache.db" "$tmp" && trap 'mv "$tmp" "testdata/cpp/.code-graph-cache.db"' EXIT && make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Full workspace, snapshot, and plugin-mirror verification passed after relocating and restoring the pre-existing ignored fixture cache. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git show 201e2f7454a310393c6ef8c88d33b3759a8d0965` | Complete task commit | PASS | Linux ordinary runtime files use a retained root-relative directory capability and descriptor-relative operations; procfd is confined to Tokio's UDS pathname boundary; prior Windows validation is preserved. |
| Independent focused quality and blind-spots reviews | Complete task diff plus callers/tests | PASS | Root/runtime replacement, first-start lazy anchoring, lock cleanup, UDS self-aliasing, Windows cfg lint, and procfs-independent ordinary recovery findings were fixed. The remaining same-UID socket chmod race grants no capability beyond that UID's existing ownership; 0700 prevents the cross-UID scenario and inode revalidation prevents publication after replacement. |

## 3.13: Bound every daemon runtime-record read

### Subtasks
- [x] Generalize bounded no-follow/nonblocking runtime-record reads
- [x] Convert lock and shutdown request/ack readers, including open lock descriptors
- [x] Add oversized-record and substitution-race regressions

### Notes

Revision boundary: every daemon-owned JSON record has the same bounded descriptor-read safety contract established for metadata in task 3.11.

### Completion Evidence

- Verified: 2026-08-11
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `2594f124e3954cd15393ef7b0650eab8b8d497d0`
- Identity recheck: `git rev-parse HEAD` at 2026-08-11T17:27:17Z, matching `2594f124e3954cd15393ef7b0650eab8b8d497d0`
- Focused review: `git show 2594f124e3954cd15393ef7b0650eab8b8d497d0`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `2594f124e3954cd15393ef7b0650eab8b8d497d0`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-mcp daemon:: -- --test-threads=1` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 48 daemon tests passed, including oversized lock, shutdown, and credential records plus already-open descriptor substitution. |
| `cargo test -p code-graph-mcp --test daemon_serve && cargo test -p code-graph-mcp --test daemon_proxy` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | All 8 explicit-daemon and 13 proxy process scenarios passed. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting and workspace lint passed with warnings denied. |
| `tmp="/tmp/opencode/code-graph-testdata-cpp-cache-3.13.db"; mv "testdata/cpp/.code-graph-cache.db" "$tmp" && trap 'mv "$tmp" "testdata/cpp/.code-graph-cache.db"' EXIT && make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Full workspace, snapshot, and plugin-mirror verification passed after relocating and restoring the pre-existing ignored fixture cache. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git show 2594f124e3954cd15393ef7b0650eab8b8d497d0` | Complete task commit | PASS | One bounded reader validates opened descriptors and caps metadata, lock, shutdown, and credential records before parsing; open-lock recovery is bounded from its existing descriptor. |
| Independent focused quality review | Complete task diff plus all runtime-record callers | PASS | The review-found unbounded credential and non-Unix lock-ownership reads were converted; remaining `read_to_end` is the shared `limit + 1` implementation and filesystem reads are test assertions or executable fingerprinting. |

## 3.14: Acknowledge connection admission and harden process fixtures

### Subtasks
- [x] Add and validate a Linux UDS post-admission acknowledgement
- [x] Exercise connection-permit saturation through the normal proxy path
- [x] Make daemon process-test roots fresh and collision-safe

### Notes

Revision boundary: transport connection is not considered attached until daemon admission succeeds, and the saturation regression runs in a guaranteed-fresh process fixture.

### Completion Evidence

- Verified: 2026-08-11
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `5dfb170c82c89d1fd5988235e9b41cd6e475c880`
- Identity recheck: `git rev-parse HEAD` at 2026-08-11T17:55:49Z, matching `5dfb170c82c89d1fd5988235e9b41cd6e475c880`
- Focused review: `git show 5dfb170c82c89d1fd5988235e9b41cd6e475c880`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `5dfb170c82c89d1fd5988235e9b41cd6e475c880`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-mcp` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 48 daemon unit, 14 proxy process, 8 serve process, and 1 smoke test passed; the new 129th UDS client receives working in-process MCP after explicit failed admission. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting and workspace lint passed with warnings denied. |
| `tmp="/tmp/opencode/code-graph-testdata-cpp-cache-3.14.db"; mv "testdata/cpp/.code-graph-cache.db" "$tmp" && trap 'mv "$tmp" "testdata/cpp/.code-graph-cache.db"' EXIT && make verify && make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Two consecutive full workspace, snapshot, and plugin-mirror gates passed after relocating and restoring the pre-existing ignored fixture cache. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git show 5dfb170c82c89d1fd5988235e9b41cd6e475c880` | Complete task commit | PASS | UDS emits `CG-OK` only after permit and lifecycle admission; clients consume it before MCP framing; both process suites allocate roots with atomic `create_dir` collision retry. |
| Independent focused quality and blind-spots reviews | Complete task diff plus transport/lifecycle callers | PASS | No material findings: guard lifetime, idle race, acknowledgement timeout/error propagation, saturation fallback, framing, and fixture cleanup align. |

## 3.15: Atomically publish daemon owner-control records

### Subtasks
- [x] Publish request and acknowledgement through create-new temporary children and atomic rename
- [x] Recover malformed stale final records without clobbering another active owner
- [x] Add truncated-record, concurrent-publisher, sentinel, and replacement regressions

### Notes

Revision boundary: interrupted owner-control publication cannot permanently obstruct replacement, while exact-owner idempotence and active-owner isolation remain intact.

### Completion Evidence

- Verified: 2026-08-11
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `62e14811a184730f928316d19e349992e1a8adc1`
- Identity recheck: `git rev-parse HEAD` at 2026-08-11T19:59:30Z, matching `62e14811a184730f928316d19e349992e1a8adc1`
- Focused review: `git show 62e14811a184730f928316d19e349992e1a8adc1`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `62e14811a184730f928316d19e349992e1a8adc1`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-mcp` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 56 daemon unit, 14 proxy process, 8 serve process, and 1 smoke test passed, including truncated-request binary replacement. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting and workspace lint passed with warnings denied. |
| `tmp="/tmp/opencode/code-graph-testdata-cpp-cache-3.15.db"; mv "testdata/cpp/.code-graph-cache.db" "$tmp" && trap 'mv "$tmp" "testdata/cpp/.code-graph-cache.db"' EXIT && make verify && make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Two consecutive full workspace, snapshot, and plugin-mirror gates passed after relocating and restoring the pre-existing ignored fixture cache. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git show 62e14811a184730f928316d19e349992e1a8adc1` | Complete task commit | PASS | Linux request/ack mutations serialize on a persistent descriptor-validated control lock, publish complete temp records by atomic rename, and scavenge crash temps after every main-lock acquisition. |
| Independent iterative quality and blind-spots reviews | Complete task diff plus lifecycle callers/tests | PASS | Findings around check-unlink races, owner transitions, unsupported rename features, temp poisoning, hardlink sentinels, lock ordering, and non-Linux cfg regressions were resolved; final Linux mutation paths are serialized and fully gated. |

## 3.16: Scavenge abandoned unique cache temporaries

### Subtasks
- [x] Serialize same-process cache saves
- [x] Remove reserved abandoned unique and legacy temp siblings before each save
- [x] Add crash-temp, sentinel, concurrency, leak, and loadability regressions

### Notes

Revision boundary: each new save bounds crash residue by scavenging the reserved temp namespace before writing, without adding cross-process cache locking outside the approved daemon ownership model.

### Completion Evidence

- Verified: 2026-08-11
- Repository: `/home/daniel/Development/Code/code-graph-mcp`
- VCS: `git`
- Revision / checkpoint: `fc82fd00f3fd38663f9d09614d6ac3285c5999ff`
- Identity recheck: `git rev-parse HEAD` at 2026-08-11T20:23:15Z, matching `fc82fd00f3fd38663f9d09614d6ac3285c5999ff`
- Focused review: `git show fc82fd00f3fd38663f9d09614d6ac3285c5999ff`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `fc82fd00f3fd38663f9d09614d6ac3285c5999ff`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-graph persist::` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | 31 persistence tests passed: exact-shape crash-temp scavenging, symlink/hardlink contents, directory refusal, unrelated siblings, serialized concurrency, poison recovery, no leaks, and loadability. |
| `cargo test -p code-graph-mcp --test daemon_proxy replacement_waits_for_delayed_persist_before_runtime_cleanup -- --exact` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Replacement still waits for admitted persistence and observes a loadable current cache. |
| `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Formatting and workspace lint passed with warnings denied. |
| `tmp="/tmp/opencode/code-graph-testdata-cpp-cache-3.16.db"; mv "testdata/cpp/.code-graph-cache.db" "$tmp" && trap 'mv "$tmp" "testdata/cpp/.code-graph-cache.db"' EXIT && make verify && make verify` | `/home/daniel/Development/Code/code-graph-mcp` | PASS (`exit 0`) | Two consecutive full workspace, snapshot, and plugin-mirror gates passed after relocating and restoring the pre-existing ignored fixture cache. |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `git show fc82fd00f3fd38663f9d09614d6ac3285c5999ff` | Complete task commit | PASS | `Graph::save` recovers a poisoned process-wide mutex, scavenges only exact `<pid>.<sequence>` reserved siblings, retains unsafe directories, and then performs the existing atomic save. |
| Independent focused quality and blind-spots reviews | Complete task diff plus persistence callers/tests | PASS | Prefix-only deletion was narrowed to the generated numeric shape. Cross-process direct/daemon save coordination remains the approved explicit non-goal; same-process active temps are serialized and cannot be scavenged. |

## 3.17: Anchor daemon ownership to the project root

### Subtasks
- [ ] Hold Linux daemon ownership on the verified project-root inode
- [ ] Prevent runtime-directory replacement from creating a second daemon namespace
- [ ] Scavenge exact abandoned metadata publication temps under ownership
- [ ] Add root-lock, runtime-replacement, metadata-temp, sentinel, and replacement regressions

### Notes

Revision boundary: daemon single-instance authority survives replacement of its runtime child directory, and crash-abandoned metadata temps are bounded without widening direct-mode cache locking.

### Completion Evidence

Pending — not complete.

## Acceptance Criteria

- [x] **AC-04**: Two clients on one daemon share an index; one indexes, the other queries without re-indexing (FR-09).
- [x] **AC-05**: Spawning creates `.code-graph/` and nothing outside the repository (FR-06, FR-07, FR-08).
- [x] **AC-06**: Idle exit fires with no clients; not with a client attached; not with an analyze in flight (FR-10, FR-11).
- [x] **AC-07**: The cache reflects the last index after idle exit; the next start loads it (FR-11).
- [x] **AC-08**: Simultaneous starts converge on one daemon with no orphans (FR-13).
- [x] **AC-09**: A differently-built client does not attach; the daemon is replaced (FR-12).
- [x] **AC-10**: With the daemon unavailable, every tool answers in-process and the fallback is reported (FR-16).
- [x] **AC-25**: Linux UDS/runtime modes and loopback TCP credential enforcement exclude remote and other-UID use (NFR-06).
- [x] **AC-26**: Warm attach does not scale with corpus size; measured on two corpora and recorded (NFR-09).
- [x] **AC-30**: One watcher serves all attached clients (FR-14).
- [x] **AC-31**: The analyze job started by one session is observable by another (FR-15).
- [x] **AC-42**: Linux UDS/TCP/lifecycle and stale-inode paths are exercised; other platform seams are deferred (NFR-07).
- [x] **AC-47**: Linux UDS default with reported loopback-TCP fallback (FR-38).
- [x] **AC-48**: TCP fallback requires a per-instance secret; file is owner-only (FR-39).
- [x] **AC-49**: Clients read the transport from metadata and connect first try (FR-40).
- [x] **AC-27**: `make verify` passes (NFR-04).
- [x] FR-06 through FR-16 and FR-38 through FR-40 realized for the Linux MVP; NFR-06, NFR-07, NFR-09 satisfied; NFR-01 preserved.

## Phase Completion Evidence

Pending — not complete.
