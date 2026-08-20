# Windows certification matrix (task 11.3, AC-60)

Native evidence that the complete GraphPlatformExpansion surface (phases
1–9) works on Windows. Every row below was executed natively on the
runner identified here — no cross-compilation, no Linux substitution.

- **Runner:** Microsoft Windows 10.0.26100.9106 (native MSVC toolchain)
- **Toolchain:** rustc 1.94.1 (e408947bf 2026-03-25)
- **Workspace identity:** `ca2e7a847fe64986e56473bb3fee402d629713dd`
  (branch `feature/shared-process`)
- **Umbrella gate:** `make verify` — PASS (`exit 0`): clippy
  `-D warnings`, `cargo fmt --all --check`, full workspace tests
  (**1,935 passed, 0 failed** across all test binaries), pending-snapshot
  check, plugin-mirror sync. Run 2026-08-20. The tally includes the
  dogfood baseline tests, which auto-skip (early-return as PASSED, with
  an eprintln hint) when `external/` submodules are uninitialized on the
  runner — disclosed here next to the headline number, with the
  compensating parser coverage noted in the dogfood row below.
- **Shell caveat recorded for reproducibility:** suites that spawn the
  daemon or CLI binaries require no live `code-graph-mcp.exe` /
  `code-graph.exe` processes (the daemon holds an exclusive lock on its
  executable); the runner kills strays before each run.

Every phases 1–9 task is covered either by a dedicated row below or by
the umbrella gate (all cited suites are members of the same workspace
test set the umbrella runs; dedicated rows record their individually
observed native counts). "N/A" rows carry their rationale inline.

## Windows-path contracts (11.3 subtasks 1–2)

| Contract | Native command | Result | Evidence |
|---|---|---|---|
| Verbatim-disk (`\\?\C:\…`) prefix strip | `cargo test -p code-graph-core simplify_` | PASS (3, incl. the 2 `#[cfg(windows)]` pins) | `simplify_strips_extended_disk_prefix`; these are compile-time-removed on Linux, so this native run is their only automated execution |
| Verbatim-UNC passthrough boundary | same run | PASS | `simplify_leaves_verbatim_unc_unchanged` — `\\?\UNC\server\share\…` rides unchanged by design (documented limitation, not a regression) |
| PathTrie key semantics for drive-letter + verbatim forms | `cargo test -p code-graph-path-trie windows_` | PASS (2) | `windows_drive_letter_path_round_trips_via_keys`, `windows_verbatim_disk_prefix_is_a_distinct_trie_key` — the distinct-key property is exactly why the watch dispatch boundary must strip |
| Watch-event path normalization at the dispatch boundary | `cargo test -p code-graph-tools canonicalize_event_path` | PASS (4, incl. the `#[cfg(windows)]` verbatim-strip pin) | `canonicalize_event_path_strips_verbatim_disk_prefix_on_windows` — `ReadDirectoryChangesW` delivers `\\?\D:\…` event paths; without the strip every watched edit would insert a duplicate `PathTrie` entry |
| Watch normalization through REAL Windows notifications | `cargo test -p code-graph-tools --test watch_cpp_macro_strip --test watch_race` | PASS | The two suites that traverse the real backend: both start the production watcher via `watch_start` (live `notify-debouncer-full` on native `ReadDirectoryChangesW`); the macro-strip test's sentinel-then-discriminator pattern proves reindex correctness through real events. (`watch_dangling_edges` also passes natively but deliberately calls `try_reindex_file` directly for determinism — it pins the reindex logic, NOT the OS-watcher path, and is cited under the umbrella, not here.) |
| Incoming user-path normalization (`.`/`..`, mixed separators) | `cargo test -p code-graph-tools --test path_normalization` | PASS (2) | `four_file_taking_tools_resolve_dot_segment_paths` — the strongest cross-platform pin on `normalize_user_path` wraps |
| 8.3 short-form TEMP vs canonical long form | umbrella + `cargo test -p code-graph-cli` | PASS | The CLI/daemon fixtures run under `DANIEL~1.WEI`-style short TEMP paths; canonicalize-at-boundary discipline (11.1 port work) is exercised by every tempdir fixture in the workspace |

## Daemon transport, security, lifecycle (phases 3–4 on Windows)

| Area | Native command | Result | Evidence |
|---|---|---|---|
| Daemon serve: named-pipe publication, admission (`CG-OK`), idle exit, cache save, warm restart, graceful stop via `shutdown.request` | `cargo test -p code-graph-mcp --test daemon_serve` | PASS (9) | Includes `runtime_directory_dacl_is_restricted_to_the_invoking_user` — deterministic security-descriptor inspection: no inherited ACEs, no broad built-in principals, exactly one grant naming the invoking user (D-0014 scope: one local user, one local project) |
| Proxy: attachment, byte-pump, queue-through-proxy, replacement (grace→drain→hard-kill), contender convergence, stale-lock recovery, mid-session death exit-0 | `cargo test -p code-graph-mcp --test daemon_proxy` | PASS (15) | Windows mandatory-lock semantics exercised throughout (`is_lock_violation` as liveness proof; owner identity via `daemon.json`) |
| TCP fallback + auth + credential rotation | same suites | PASS | Runs at process level via the debug-only `CODE_GRAPH_TEST_FORCE_TCP_ROOT` seam (per-PID pipe names cannot be occupied externally on Windows); constant-time `CG-AUTH` compare |
| Daemon unit surface (locks, metadata, identity, handle sealing) | `cargo test -p code-graph-mcp` | PASS (23 unit + the three integration binaries above — the command runs all four; the unit count is the `src/main.rs` binary's tally) | Includes the `seal_standard_handles_from_inheritance` seam (the workspace's one function-scoped unsafe outside `code-graph-graph`) |
| Analyze queue/job model (phase 4) | `cargo test -p code-graph-tools --test analyze_async_lifecycle` + umbrella (queue unit tests in `code-graph-tools`) | PASS (1 + umbrella) | Async kickoff → poll → completed `AnalyzeJobView`; slot rotation and pending-FIFO semantics are unit-tested in the workspace set |
| Second-local-account denial | N/A | N/A | Removed from scope by D-0014: the daemon serves one local user's sessions in one local project; cross-account isolation is not a claimed guarantee |

## Graph tools, cache, queries (phases 1–2, 9)

| Area | Native command | Result | Evidence |
|---|---|---|---|
| Phase 1 queries (`get_symbol_at`, `find_path`, `detect_communities`) | umbrella + `cargo test -p code-graph-cli --test cli` (AC-33 test) | PASS | `phase_one_queries_are_invocable_from_the_cli` exercises all three natively end to end through analyze + query |
| Typed core layering (phase 2) | umbrella (`cargo test -p code-graph-tools`: 483 lib + suites) | PASS | The core/adapters split is structural; the CLI's no-rmcp/no-handlers guard test (`cli_depends_on_the_typed_core_only`) pins the reachability property natively |
| Response shapes + byte budget | umbrella (`snapshot_responses` 59, `byte_budget_acceptance`, `response_shape_acceptance`) | PASS | Snapshot separator normalization (11.1 port work) keeps snapshots platform-stable |
| rkyv cache v11 round-trip + version mismatch re-index | `cargo test -p code-graph-graph persist` | PASS (29 on this host; 3 unix-gated tests compile out) | Includes `round_trip_preserves_candidate_count` (non-default value, both adjacency directions) and `load_version_mismatch_returns_false` |
| Candidate count on all four edge-reporting tools (phase 9, AC-57) | `cargo test -p code-graph-tools --test candidate_count` | PASS (4) | Real 2-candidate contest distinguished from 1 in single responses natively |
| Dogfood baselines | umbrella | PASS/auto-skip | Submodule-gated baseline tests auto-skip with the documented hint when `external/` is uninitialized on the runner; parser-correctness coverage rides the per-crate suites (e.g. `testdata_cpp_baseline`), which pass natively |

## History (phases 5–6, 8)

| Area | Native command | Result | Evidence |
|---|---|---|---|
| Git provider (gix, hermetic git-CLI harness) | `cargo test -p code-graph-vcs-git` | PASS (14) | Includes `gix_tree_path` separator handling (11.1 repair), shallow-clone boundary, merge parents, mode-only changes, gitlink `Operation`-not-`NotFound`, foreign-repo refusal |
| `blame_symbol` (oracle-tested, staleness tri-state) | `cargo test -p code-graph-tools --test blame_symbol` | PASS (9) | Includes the `git blame --porcelain` oracle and CRLF-clean staleness — the autocrlf case is a WINDOWS-native concern by construction |
| `symbol_history` (transitions, deletion arc, boundary flags, config pipeline, live `literal_insensitive`) | `cargo test -p code-graph-tools --test symbol_history` | PASS (13) | Hermetic env-cleared git fixtures run natively; the macro-config test proves the preprocess pipeline over Windows paths |
| Per-language AST fingerprints (phase 8, AC-38/AC-39) | `cargo test -p code-graph-lang-{cpp,rust,go,python,csharp,java} fingerprint` | PASS (7+6+4+4+4+4 = 29) | Both modes, all six languages, including the boundary pins (Rust outer attributes, C++ template clauses) |

## CLI (phase 7) and cross-front-end parity

| Area | Native command | Result | Evidence |
|---|---|---|---|
| CLI end to end: AC-33 queries, exit statuses 0/1/2, byte-exact unindexed domain error, no-rmcp guard, AC-40 daemon/standalone byte-identity (incl. the Decision 7 window), attach-only leaves no contender | `cargo test -p code-graph-cli --test cli` | PASS (5) | The daemon-parity test spawns a REAL `--serve` daemon over native named pipes and compares machine output byte-for-byte |
| CLI machine output ≡ MCP payloads (AC-11, five distinct response shapes, `suggestions` both arms) | `cargo test -p code-graph-cli --test parity` | PASS (6) | Each test runs the real `to_call_tool_result` adapter path and the built `code-graph.exe` with `--json` on the same fixture and asserts byte equality — this IS the "representative CLI machine output vs MCP payloads on Windows" subtask, mechanized |
| Human renderer | same suite | PASS | `parity_human_mode_suggestions_footer_tracks_field_presence` |

## Phase-by-phase task coverage summary

| Phase | Tasks | Native disposition |
|---|---|---|
| 1 Graph Queries (3 tools) | 1.1–1.7 | Umbrella + CLI AC-33 row; response snapshots platform-stable |
| 2 Typed Core | 2.1–2.6 | Umbrella (byte-identical adapters); no-rmcp reachability pinned via the CLI guard test |
| 3 Daemon Foundation | 3.x | daemon_serve/daemon_proxy/unit rows above — the Windows transport is named pipes, natively exercised |
| 4 Analyze Queue | 4.1–4.4 | Queue unit tests in umbrella + `analyze_async_lifecycle`; queue-through-proxy in daemon_proxy |
| 5 VCS + Blame | 5.1–5.6 | vcs-git (14) + blame_symbol (9) rows |
| 6 Symbol History | 6.1–6.7 | symbol_history (13) + fingerprint-cache unit tests (umbrella: 7) |
| 7 CLI | 7.1–7.3 | cli (5) + parity (6) rows; design task 7.1 is an artifact, N/A for runtime certification |
| 8 Fingerprints | 8.1–8.7 | Six per-language suites (29) + live-mode history test |
| 9 Candidate Count | 9.1–9.3 | persist (29) + candidate_count (4) rows; 9.3 is docs/descriptions, certified via tools-list snapshots in umbrella |

## Acceptance-criteria coverage (phases 1–9)

All phase 1–9 acceptance criteria were individually evidenced at their
phases' closes (artifacts 13, 15–21) with `make verify` rows. Native
provenance, scoped honestly (gate artifact 22 corrected the first
draft's overclaim): phases 6–9 were implemented entirely on this runner
after the pull-forward, so every one of their per-task commands and
`make verify` runs executed natively; phase 5's LATER tasks (5.4–5.6,
2026-08-18) are native, but 5.1–5.3 (2026-08-16) predate the
pull-forward — the `ccd9e11` commit message itself records a native
blame failure caught by a 5.2-era test — so phases 1–4 AND 5.1–5.3
certify on the same basis: their suites are members of today's umbrella
set and pass natively at this identity. That is the claim AC-60 makes —
the surface works on Windows NOW, at `ca2e7a8`, witnessed by the
1,935-test green umbrella plus the dedicated rows above.

**Linux-native acceptance criteria — explicit N/A rows** (the 11.3
verification field requires an explicit rationale per criterion.
AC-25/AC-42/AC-47 are Linux-scoped by their own text; AC-48's text is
transport-generic, but its owner-only-file arm is a `cfg(unix)`
mechanism and its shared enforcement logic IS natively exercised on
Windows — each row states its own precise basis):

| AC | N/A rationale |
|---|---|
| AC-25 (UDS owner-only + TCP credential, Linux) | Scoped "On Linux" by its own text; the Windows analogues are the named-pipe admission + runtime-directory DACL rows above (D-0014 scope) plus the process-level TCP fallback/auth rows |
| AC-42 (Linux MVP exercised natively on Linux) | Scoped "natively on Linux" — Windows execution can neither satisfy nor regress it; its suites remain `#[cfg(unix)]`-gated in the workspace set |
| AC-47 (UDS default + forced TCP fallback, Linux) | UDS is a Linux transport; the Windows counterpart (named pipe default + `CODE_GRAPH_TEST_FORCE_TCP_ROOT`-forced TCP fallback) is natively exercised in the daemon rows above |
| AC-48 (TCP secret refusal/rotation) | The enforcement logic is shared and natively exercised on Windows via the forcing seam (daemon rows); the UDS-adjacent arms are Linux-scoped |

## AC-60 disposition

**PASS**, against AC-60 as amended 2026-08-20 per D-0014 (the amendment
note in the spec records what changed and why: the another-local-account
denial was removed from scope by D-0014, and the delivered deterministic
security-descriptor inspection covers the runtime DIRECTORY's DACL — the
state holding the TCP secret and daemon metadata — not the pipe object's
own SD, which carries the default descriptor and is uninspected). Native
Windows workspace (build/lint/test), daemon named-pipe/ACL/lifecycle,
path contracts, and CLI parity all carry native evidence above.

**Linux status, stated precisely:** no `#[cfg(unix)]`-gated code block
was DELETED by the Windows repairs; two test suites were deliberately
un-gated (their `#![cfg(unix)]` inner attributes removed so they run on
both platforms — widening Linux coverage, not shrinking it);
`daemon.rs` RESTRUCTURED some unix gates (statement-level →
function-level, the UDS bind arm extracted into a cfg'd helper), and two
shared-code changes touch Linux behavior (the `gix_tree_path`
forward-slash join fix — a correctness fix on both platforms — and the
~30 shared test-file ports). All of this is diff-visible in the
pull-forward series and disclosed in the phase doc. A post-repair Linux
run has NOT been executed from this Windows host; the phase AC's Linux
arm is therefore certified as "diff-reviewed, cfg-scoped, no Linux gate
deleted" with the full Linux re-run recorded as a follow-up for the next
Linux runner session (gate artifact 22).

**Known omission recorded:** NTFS case-insensitivity (two casings of one
path resolving to one file) has no dedicated pin; canonicalize-at-boundary
resolves to on-disk casing for existing files, which is the operative
protection. Filed as a follow-up candidate, not a certified property.
