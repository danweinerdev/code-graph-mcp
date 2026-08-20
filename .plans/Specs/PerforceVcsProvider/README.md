---
title: "Perforce VCS Provider (code-graph-vcs-p4)"
type: spec
status: approved
created: 2026-08-20
updated: 2026-08-20
tags: [vcs, perforce, p4, subprocess, provider]
related: [Research/perforce-vcs-provider, Specs/GraphPlatformExpansion]
supersedes: ""
superseded_by: ""
implemented_in: ""
waivers: []
---

# Perforce VCS Provider (code-graph-vcs-p4)

## Overview
A second `VcsProvider` backend so `blame_symbol` and `symbol_history` work in
Perforce Helix Core working trees exactly as they do in git checkouts. Per
D-0015, the provider is a subprocess CLI integration: a new
`code-graph-vcs-p4` crate spawns the user's own `p4` executable with
structured output and parses it inside `spawn_blocking` — no Perforce library
is linked and no Perforce C/C++ source is compiled (D-0004 untouched). The
existing trait surface (`blame`, `revisions_touching`, `read_at`,
`resolve_rev`, plus sync `id`/`detect`), opaque `RevId`, registry detection,
and the tools' success-shaped-unavailability contract (GraphPlatformExpansion:FR-36) all carry over unchanged; changelist numbers ride in `RevId`.
Full option analysis: `Research/perforce-vcs-provider.md`.

## Goals
- Perforce attribution and history for the two VCS-backed tools with zero
  MCP/CLI wire-contract changes and zero handler edits.
- Behavioral parity with the git provider wherever the trait contract speaks:
  hunk shape, revision-window semantics, unavailability-as-success, and
  path-rename non-following (the git provider's documented history-stops-at
  path-creation behavior; D-0005 itself governs symbol matching, which the
  tools apply identically over any provider).
- A hermetic `p4d`-fixture test harness mirroring `code-graph-vcs-git`'s
  git-CLI harness, auto-skipping (with a setup hint) where Perforce binaries
  are absent.

## Non-Goals
- Any `-sys`/FFI binding of the Perforce C++ API (rejected by D-0015; revival
  requires a superseding ledger decision).
- Write operations of any kind: no submit, sync, edit, shelve, or spec
  mutation. The provider is strictly read-only.
- Credential/session management: the provider never runs `p4 login`, never
  prompts, and never stores tickets — an expired session surfaces as an
  actionable error naming `p4 login`.
- Following renames/moves across `p4 move` history (mirrors the git
  provider's documented path-history behavior: history stops at the
  path-creation point; not a D-0005 matter — that entry governs symbol
  matching).
- Pending/shelved changelist visibility: attribution reflects submitted state
  only, mirroring the git provider's committed-state rule.
- Stream switching, client-spec editing, proxy/broker/replica-specific
  behavior beyond whatever the user's own `p4` configuration already routes.

## Requirements

### Functional Requirements
- **FR-01**: A new `code-graph-vcs-p4` crate implements `VcsProvider`
  (the four async ops plus `id()`/`detect()`), depends on the
  `code-graph-vcs` trait crate only (no `rmcp`, no `handlers`, no Perforce
  crates), reports `id() == "perforce"` (the trait doc's canonical spelling;
  the fingerprint sidecar cache keys on it, so it is frozen at first
  release), and is injected by the `code-graph-mcp` binary and the
  `code-graph` CLI alongside the git provider (GraphPlatformExpansion:AC-23 parity).
- **FR-02**: `detect(working_tree)` is offline and cheap: it reports true
  when a `P4CONFIG`-named file (default `.p4config`, honoring the `P4CONFIG`
  environment variable) exists on the upward walk from the working tree, or
  when both `P4PORT` and `P4CLIENT` are discoverable from the environment —
  where "environment" includes process environment variables, the
  `P4ENVIRO` file, and (on Windows) the Perforce registry keys
  (`HKCU\Software\Perforce\Environment`), because `p4 set` on Windows
  writes the registry and produces neither a config file nor env vars
  *[amended 2026-08-20, blind-review finding: the dominant Windows
  configuration was undetectable]*. It never spawns `p4` and never
  contacts a server. The `.p4config` default is a deliberate heuristic
  divergence from `p4` itself (which performs no config lookup when
  `P4CONFIG` is unset): a detection-true tree whose `p4` cannot actually
  connect surfaces the FR-10 actionable errors at op time rather than
  being silently skipped.
- **FR-03**: Every Perforce interaction is a captured-output subprocess
  invocation of a user-provided `p4` executable (resolved from `PATH`);
  stdout and stderr are never inherited (MCP stdio purity — the daemon's
  captured `icacls`/`whoami` precedent).
- **FR-04**: Structured output is parsed from `p4 -G` (Python-marshal
  dictionaries) by a pure-Rust parser covering exactly the marshal subset p4
  emits (string-keyed dicts of strings/integers); no Python interpreter is
  executed. A design-level fallback to `-ztag` text parsing is permitted if
  the marshal subset proves unstable, but one format must be chosen per
  command, not probed at runtime. Metadata strings (author, summary) are
  decoded as UTF-8 with lossy replacement — unicode-mode servers
  (`P4CHARSET`) must never fail an op on undecodable metadata bytes;
  `read_at` file content stays raw bytes.
- **FR-05**: `resolve_rev(spec)` resolves a user revision spec to a submitted
  changelist number carried in `RevId`. A NUMERIC spec must resolve to
  exactly that submitted changelist — a nonexistent, pending, or deleted
  changelist number is a `VcsError` naming the bad spec, never a silent
  nearest-earlier resolution (`@CL`'s native at-or-before semantics apply
  to `@date` forms only) *[amended 2026-08-20, blind-review finding]*.
  The provider-default revision is the workspace HAVE state
  (`//<client>/...#have` — the true analog of git `HEAD`: attribution
  reflects what this workspace is synced to, not server tip; tip-of-view
  was rejected because actively-developed depots would report `stale` on
  nearly every file). `@date` specs are interpreted in the SERVER's
  timezone (Perforce semantics) — the provider documents this rather than
  compensating.
- **FR-06**: `revisions_touching(path, limit)` returns the submitted
  changelists that touched the depot path mapped from the file, **newest
  first** (matching the `RevisionWindow` trait contract — `symbol_history`
  reorders at the tool layer), via `p4 filelog`/`p4 changes`, with
  `RevisionWindow.truncated` set only when the provider's own examination
  bound (not the caller's `limit`) cut the walk short.
- **FR-07**: `read_at(rev, path)` returns the file's bytes at the given
  changelist via `p4 print -q <depotPath>@<CL>`, distinguishing
  file-absent-at-revision from I/O failure per the trait's error contract.
- **FR-08**: `blame(path, lines: Option<(u32, u32)>, at: Option<&RevId>)`
  produces line-attributed hunks: `p4 annotate -c -q` supplies the
  introducing changelist per line, joined with changelist metadata for
  author and UTC timestamp. `lines: None` blames the whole file; a `lines`
  range extending past EOF at the blamed revision clips to EOF (git
  parity, never an error); `at: None` blames at the FR-05
  provider-default revision. The metadata join is a SINGLE batched
  `p4 changes` invocation per blame op (per-changelist `describe` calls
  rejected — resolved 2026-08-20), and its `CL -> (author, time)` result
  is cached per provider instance so repeat blames over one file's
  history (the `symbol_history` walk pattern) do not refetch it. Spawn
  budget, enumerated exhaustively *[re-amended 2026-08-20 — the prior
  amendment still undercounted; blind-review finding]*: one-time
  `p4 info` probe (first op on the provider), per-path `p4 where`
  (cached), `at: None` default-revision resolution (one `p4 changes -m1`
  per blame call taking the default), the annotate call, and the batched
  join (cached). Steady-state explicit-revision blame is therefore two
  spawns; steady-state default-revision blame is three; first-ever call
  on a fresh provider peaks at five. Hunks are clipped to the requested
  range, in file order, non-overlapping (`BlameHunk` parity with git).
- **FR-09**: The provider verifies a queried file belongs to its bound
  client workspace via `p4 where` semantics (exclusionary view lines
  respected); a path outside the client view reports the trait's
  unavailability shape (the git provider's belongs-to-a-different-repository
  precedent), which the tools already render as `available: false` +
  `reason`.
- **FR-10**: Operational failures map to actionable `VcsError`s, classified
  PRIMARILY on `p4 -G`'s structured error records (`code: "error"` dicts
  carrying numeric `severity`/`generic` fields — stable across server
  versions and locales) with English stderr text as the fallback signal
  only *[amended 2026-08-20, blind-review finding: text-only matching
  breaks on older servers and localized output]*. Missing `p4` executable
  at op time, connection failure, authentication failure, and
  SSL-trust-not-established are distinct messages; the authentication
  message names `p4 login` and the trust message names `p4 trust`. A path
  with no submitted history at the blamed revision is unavailability, not
  an error (GraphPlatformExpansion:FR-36 parity).
- **FR-11**: Every subprocess invocation is bounded by a timeout: default
  30 seconds, overridable via the `CODE_GRAPH_P4_TIMEOUT_SECS` environment
  variable (large generated files and WAN proxies legitimately exceed the
  default; a hard-coded bound would kill healthy ops with no escape hatch
  *[amended 2026-08-20, blind-review finding]*), and injectable in tests
  without wall-clock waits. A hung or unreachable server fails the op
  instead of wedging the tool handler. All FR-10/FR-11 failures map onto
  EXISTING `VcsError` variants — no trait-crate change, keeping the
  Dependencies claim consistent.
- **FR-12**: A hermetic test harness provisions a throwaway local `p4d`
  server + client workspace per test (mirroring the git provider's
  git-CLI fixture harness), and auto-skips with an `eprintln!` setup hint
  when `p4`/`p4d` binaries are unavailable (dogfood-submodule precedent —
  no panic, no `--ignored` opt-in).
- **FR-13**: Registry precedence is pinned: the git provider is registered
  BEFORE the p4 provider, so a working tree that satisfies both detectors
  (a `.git` checkout inside a Perforce client view, or machine-wide
  `P4PORT`/`P4CLIENT`) resolves to git — `.git` presence is a stronger
  locality signal than environment variables, and FR-02's env-var branch
  would otherwise claim every directory on a globally configured Perforce
  machine.
- **FR-14**: When the server reports case-insensitive path handling
  (`p4 info` `caseHandling: insensitive`), the provider matches depot and
  client paths case-insensitively when correlating command output rows to
  the queried file; on case-sensitive servers matching stays exact. A
  casing mismatch on an insensitive server must never produce a false
  not-in-view unavailability.
- **FR-15**: Every local or depot path placed in a `p4` argv is
  filespec-encoded first: `@` → `%40`, `#` → `%23`, `%` → `%25`, `*` →
  `%2A` (Perforce's reserved revision/wildcard characters). A file named
  `foo@2x.png` or `bar#1.cpp` must round-trip through `where`, `print`,
  `annotate`, and `changes` correctly — unencoded, `p4` would parse the
  suffix as a revision spec and silently resolve the wrong file
  *[added 2026-08-20, blind-review finding]*.

### Non-Functional Requirements
- **NFR-01**: D-0004/D-0015 conformance: the dependency graph gains no
  Perforce library crate and compiles no Perforce C/C++ source; every
  Perforce interaction is a spawned subprocess. `#![forbid(unsafe_code)]`
  in the new crate.
- **NFR-02**: The provider builds and its unit tests pass on Windows,
  Linux, and macOS; the `p4d` harness runs on at least Windows and Linux
  natively. AC-10's case-SENSITIVE arm is Linux-only by construction
  (Windows `p4d` is case-insensitive unconditionally) and is gated
  accordingly rather than claimed cross-platform *[noted 2026-08-20,
  blind-review finding]*.
- **NFR-03**: Workspace conventions hold: no `tracing` dependency
  (`eprintln!` for out-of-handler warnings), blocking subprocess work
  inside `spawn_blocking`, crate listed in the workspace map with its
  responsibility line.
- **NFR-04**: Per-call subprocess overhead is accepted as the cost of
  D-0015 (no persistent connection); the provider must not spawn more
  than one `p4` process per trait-op call except the enumerated,
  individually-cached auxiliaries *[re-amended 2026-08-20 to close the
  blind-review undercount]*: (a) blame's single batched metadata join,
  cached per provider instance; (b) the per-path depot-mapping call,
  cached per path; (c) the one-time `p4 info` probe, cached per provider
  lifetime; (d) blame-at-default's revision resolution (one
  `p4 changes -m1` when `at: None`). `symbol_history`'s window walk must
  reuse the existing fingerprint sidecar cache rather than re-blaming.

## User Stories
- As an engineer in a Perforce-hosted codebase (UE-style depot), I ask
  `blame_symbol` who last changed a function and get committed-state
  attribution with changelist numbers, authors, and timestamps — without
  installing anything beyond the `p4` CLI I already use.
- As the same engineer, `symbol_history` shows me the changelists where a
  symbol's content actually changed, skipping reformat-only submits, with
  the same transition semantics I get in git repos.
- As a user without Perforce configuration (no `.p4config`, no
  `P4PORT`/`P4CLIENT`), the VCS tools report git results or clean
  unavailability — the p4 provider never activates, never spawns, and
  never slows detection down.
- As a user whose ticket expired overnight, I get an error that says to run
  `p4 login` instead of a cryptic subprocess failure.

## Acceptance Criteria
- **AC-01**: Against the `p4d` fixture, `blame_symbol` returns
  `available: true` with non-overlapping hunks in file order whose
  `rev`/`author`/`timestamp_utc` match the fixture's submitted changelists;
  the response shape is byte-shape-compatible with the git provider's
  (single deserializer covers both).
- **AC-02**: Against the fixture, `symbol_history` reports
  `introduced`/`modified`/`removed` transitions across submitted
  changelists with both fingerprint modes, and a reformat-only submit is
  invisible under `normalized`.
- **AC-03**: `detect()` returns false in a directory with no Perforce
  configuration and true beside a `.p4config`, without spawning any
  process — verified by a test that puts a poison `p4` shim on `PATH`
  which fails the test if executed.
- **AC-04**: With `p4` absent from `PATH`, trait ops return the
  missing-executable `VcsError` and the tools render their existing
  error/unavailability contracts; nothing panics and no stdio pollution
  occurs.
- **AC-05**: An out-of-view path (exclusionary view line or a file outside
  the client root) yields success-shaped unavailability with a reason, not
  a tool error.
- **AC-06**: Dependency-graph inspection shows no Perforce crate and no
  compiled Perforce source (`cargo tree` + build inspection recorded as
  evidence); the new crate carries `#![forbid(unsafe_code)]`.
- **AC-07**: An authentication-failure fixture run maps to the
  `p4 login`-naming error; a connection-refused fixture run maps to the
  connection error; a deliberately hung listener trips the FR-11 timeout
  (via the test-injectable timeout, not a wall-clock wait).
- **AC-08**: `make verify` passes on a machine with no Perforce binaries
  installed: the harness auto-skips with the setup hint and no test fails.
- **AC-09**: In a working tree that satisfies both detectors (a git
  checkout with a `.p4config` beside it, or with `P4PORT`/`P4CLIENT` in
  the environment), registry detection selects the git provider (FR-13);
  a Perforce-only tree still selects the p4 provider.
- **AC-10**: Against a case-insensitive fixture server (`p4d -C1`), a
  query path whose casing differs from the depot's stored casing still
  blames successfully (FR-14); the same mixed-casing query against a
  case-sensitive fixture (Linux-only arm — Windows `p4d` cannot run
  case-sensitive) reports not-in-view unavailability, exercising both
  matching modes.
- **AC-11**: `resolve_rev` with a numeric changelist that does not exist
  as a submitted changelist on the fixture server (too high, pending, or
  deleted) returns the FR-05 bad-spec error — never a silent
  nearest-earlier resolution; blame at that spec is a tool error naming
  it *[added 2026-08-20, blind-review finding]*.
- **AC-12**: A fixture file whose name contains Perforce reserved
  characters (`@`, `#`, `%`) submits, resolves through `where`, blames,
  and reads back byte-identically via the FR-15 encoding; the same
  operations without encoding would misparse — the test pins the encoded
  argv *[added 2026-08-20, blind-review finding]*.

## Constraints
- The `p4` executable is user-provided; the provider never downloads,
  bundles, or updates Perforce binaries. The test harness locates
  preinstalled `p4`/`p4d` binaries only — it never downloads them either;
  absence means auto-skip (FR-12).
- Attribution reflects submitted state only; pending edits in the client
  workspace surface through the existing staleness channel
  (`stale`/`stale_reason`), not through pending-changelist inspection.
- RCS keyword expansion (`+k`/`ktext` file types): `read_at` must fetch
  with keyword expansion SUPPRESSED (`p4 print -k`, pinned by fixture
  capture) so that `$Id$`/`$Change$` churn does not turn every submit
  into a spurious `symbol_history` `modified` transition for symbols
  containing keyword text *[added 2026-08-20, blind-review finding]*.
- External contract pin: `p4 -G`/`-ztag` output shapes and the
  `annotate`/`filelog`/`print`/`changes`/`where` semantics are pinned to
  the Perforce Helix Core 2025.1 command reference
  (https://help.perforce.com/helix-core/server-apps/cmdref/current/) and
  the primary-source findings in `Research/perforce-vcs-provider.md`
  (as of 2026-08-20). Implementation derives output-format behavior from
  captured fixture output against a real `p4d` of that generation, never
  from model memory.
- The subprocess boundary is the D-0015 line: any future need for a
  persistent connection or native bindings requires a superseding ledger
  decision before implementation.

## Dependencies
- `code-graph-vcs` trait crate (no changes expected; any trait gap found
  during implementation is a spec-change conversation, not a silent trait
  edit).
- `Research/perforce-vcs-provider.md` (primary-source analysis), D-0015
  (strategy), D-0004 (dependency boundary), D-0005 (rename semantics
  precedent), GraphPlatformExpansion:FR-36 (unavailability shape).
- Test-time only: `p4`/`p4d` binaries discoverable on the harness machine;
  their absence must degrade to auto-skip (FR-12), never to failure.

## Open Questions
None.
