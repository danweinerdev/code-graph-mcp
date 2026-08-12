---
title: "Phase 3 Debrief: Daemon Foundation"
type: debrief
status: draft
plan: GraphPlatformExpansion
phase: 3
phase_title: "Daemon Foundation"
created: 2026-08-11
updated: 2026-08-11
tags: [daemon, performance, ac-26, nfr-09]
related:
  - Plans/GraphPlatformExpansion
  - Specs/GraphPlatformExpansion
  - Designs/RepoLocalDaemon
---

# Phase 3 Debrief: Daemon Foundation

Draft phase note recording the task 3.5 warm-attach measurement and Linux MVP evidence. AC-26 is a recorded metric, not a wall-clock CI threshold. Native macOS and Windows completion is deliberately deferred to phases 10 and 11.

## Measurement protocol

- Host: `Linux enterprise.core.3p14.net 7.1.7-200.fc44.x86_64`, x86_64.
- Compiler: `rustc 1.96.1 (31fca3adb 2026-06-26)`, LLVM 22.1.2.
- Binary: release build from implementation revision `73c332f0f38ad4c6ce925fd4ad2aa07b0eba1406`.
- Repetitions: 5 per mode; median reported with every sample.
- Warm sample: spawn the default stdio proxy, initialize MCP, then run exact-file `get_file_symbols(count_only=true)` against an already-indexed resident daemon.
- Cold sample: spawn `--no-daemon`, initialize MCP, force-index the complete corpus, then run the same exact-file query.
- The harness temporarily establishes a missing corpus-local `.code-graph.toml` project boundary, preserves/restores any existing config and cache, validates equal indexed counts across cold samples, and stops only the daemon whose metadata and lock identities agree.

Reproduce:

```sh
cargo build --release -p code-graph-mcp
python3 scripts/bench-daemon-attach.py --binary target/release/code-graph-mcp --corpus external/ripgrep --repetitions 5 --json
python3 scripts/bench-daemon-attach.py --binary target/release/code-graph-mcp --corpus external/abseil-cpp --repetitions 5 --json
```

## Corpora

| Corpus | Pin | Physical files | Indexed files | Symbols | Edges | Query file |
|---|---|---:|---:|---:|---:|---|
| `external/ripgrep` | `15.1.0` / `af60c2d` | 220 | 100 | 3,137 | 15,501 | `build.rs` |
| `external/abseil-cpp` | `20260107.1` / `255c84d` | 1,570 | 849 | 9,928 | 92,077 | `CMake/install_test_project/simple.cc` |

## Results

| Corpus | Warm attach samples (s) | Warm median | Cold samples (s) | Cold median |
|---|---|---:|---|---:|
| ripgrep | 0.291069, 0.278334, 0.289369, 0.277912, 0.265090 | **0.278334 s** | 0.330723, 0.327747, 0.326626, 0.334446, 0.323904 | **0.327747 s** |
| abseil-cpp | 0.278876, 0.290204, 0.265823, 0.267659, 0.285046 | **0.278876 s** | 0.424713, 0.417894, 0.419936, 0.449022, 0.438676 | **0.424713 s** |

## Reading the numbers

Warm attach is effectively constant across these corpora: the abseil median is 1.002× the ripgrep median despite 8.49× as many indexed files and 6.34× as many edges. Cold force-index plus query rises to 1.296× the ripgrep median. This meets AC-26's intended discriminator—no corpus-size scaling in warm attach while cold work increases—without claiming a universal latency threshold.

The absolute warm figure (~279 ms) includes process spawn, executable fingerprinting, MCP initialization, and one exact path-trie file query. It is not socket-connect latency alone. Both corpora are modest compared with UE/LLVM-scale repositories, and five samples on one Linux host are directional evidence rather than a cross-machine benchmark.

## Native platform status

| Platform/security boundary | Status | Evidence / blocker |
|---|---|---|
| Linux UDS, TCP fallback, idle lifecycle, POSIX stale inode | Exercised | Rust unit/process suites and full `make verify` pass on this host. Runtime/UDS/control modes and loopback TCP authentication are covered. |
| Different local UID | Linux enforcement complete | `0700` runtime and `0600` UDS/control modes plus per-instance TCP authentication are pinned. The MVP criterion is the enforcement properties; no privileged second-account harness is required. |
| macOS UDS + stale inode | Deferred to Phase 10 / AC-59 | The seam remains explicit; no support claim is made by the Linux MVP. |
| Windows named pipe + ACL + TCP fallback | Deferred to Phase 11 / AC-60 | Named-pipe/path/ACL branches may remain ignored or best-effort until native completion. |

Phase 3 is complete for the Linux MVP. Tasks 3.6-3.17 closed the successive isolation, admission, persistence, runtime-anchoring, cache-temp, and project-root ownership findings. The final frozen four-lane review is Aligned; native macOS/Windows evidence remains assigned to phases 10/11.

## Decisions Made

None. Task 3.5 implements the already-approved idle lifecycle and records its measurement; no new product or architecture choice was made.

## Follow-Ups

- Execute Phase 10 when native macOS support becomes a priority.
- Execute Phase 11 when native Windows support becomes a priority.

## Requirements Assessment

FR-10/FR-11 and AC-06/AC-07 are implemented and Linux-verified. AC-25 is established by Linux owner-mode, loopback, and authentication enforcement. AC-26 is recorded above, and AC-42 is satisfied by native Linux UDS/TCP/lifecycle/stale-inode coverage. AC-59/AC-60 carry deferred native platform completion.

## Deviations

- AC-26 originally named only a measurement outcome; a repeatable stdlib-only MCP harness was added because `code-graph-bench` measures index/cache internals, not stdio-proxy attachment.
- The original cross-platform acceptance text was rescaled to the Linux MVP; macOS and Windows were not silently waived, but moved to explicit deferred phases with their own acceptance ids.

## Risks & Issues Encountered

- A benchmark rooted at `external/<corpus>` would otherwise inherit the repository's parent `.code-graph.toml` and disturb the wrong daemon/cache. The harness creates a temporary corpus-local project boundary when needed and restores state on every bounded failure path.
- An O(graph) wildcard query would hide attach scaling inside query scaling. The harness uses an exact-file path-trie lookup instead.

## Lessons Learned

- Lifecycle eligibility must share one synchronization domain for connection and analyze activity; independent counters leave an attach-vs-expiry gap.
- Recorded performance evidence needs a reproducible harness and full sample disclosure, not only four summary numbers.

## Impact on Subsequent Phases

Phase 4 can reuse the lifecycle generation/coordinator when queued analyses are added, but must preserve the distinction between admitted work and attached connections.

## Skill Opportunities

- A native-platform evidence collector could standardize OS, transport, ACL, and test-output capture for cross-platform phase gates.
