---
title: "Command-Line Interface"
type: phase
plan: GraphPlatformExpansion
phase: 7
status: complete
created: 2026-08-08
updated: 2026-08-19
deliverable: "A code-graph CLI over the typed core, producing payloads identical to the MCP surface and working with or without a daemon."
tasks:
  - id: "7.1"
    title: "CLI interface design: command surface, output modes, exit statuses"
    status: complete
    justifies: "FR-19, FR-20. Designs/RepoLocalDaemon Decision 8 deliberately deferred this because designing a command surface against a typed core that did not exist was guesswork; with phase 2 landed the signatures are known and the design is short."
    verification: "A design document exists at Designs/CommandLineInterface with status review or approved, covering: the subcommand surface mapped to typed core functions, the machine-readable output convention, the human-readable default, and the exit-status mapping for success, tool error, and operational failure. Reviewed by plan-reviewer or spec-reviewer with no unresolved Critical or Major findings."
  - id: "7.2"
    title: "code-graph binary over the typed core"
    status: complete
    justifies: "FR-17, FR-18, AC-33 (CLI half), AC-40. FR-17's 'no duplicated query logic' is only achievable through the typed core; a CLI parsing JSON back out of CallToolResult would be a second place for response shapes to drift."
    verification: "cargo test -p code-graph-cli — every subcommand calls a core:: function with no rmcp type constructed; the three phase 1 queries are invocable from the CLI, completing AC-33; identical invocations produce identical machine-readable output with and without a running daemon (AC-40, FR-18); a query against an unindexed repository reports the same domain error the MCP surface does."
    depends_on: ["7.1"]
  - id: "7.3"
    title: "Output parity and exit-status behaviour"
    status: complete
    justifies: "FR-19, FR-20, AC-11, AC-12. Payload parity is the property that makes the CLI trustworthy for scripting; without per-shape coverage a divergence in one envelope type would go unnoticed until someone depended on it."
    verification: "cargo test -p code-graph-cli --test parity — machine-readable output equals the MCP payload for one query of each distinct shape: a Page envelope (get_callers), a non-Page tree (get_class_hierarchy), a flattened envelope with a conditional field (search_symbols, both with and without suggestions), a dual-page response (get_coupling direction=both), and a non-JSON body (generate_diagram format=mermaid) (AC-11); exit status distinguishes success, an unknown-symbol tool error, and an operational failure such as an unreadable cache (AC-12)."
    depends_on: ["7.2"]
---

# Phase 7: Command-Line Interface

## Overview

A second front-end over the typed core, so graph queries are usable from a terminal and in scripts rather than only from an agent session. Depends on phase 1 for the three new queries, phase 2 for the typed core that exposes them, and phase 3 for daemon attachment.

This phase opens with a design task rather than code — the CLI's interface was deliberately left unspecified until the core existed.

## 7.1: CLI interface design: command surface, output modes, exit statuses

### Subtasks
- [x] Read the landed `core::` signatures and enumerate what each subcommand needs
- [x] Design the subcommand surface, mapped one-to-one onto core functions
- [x] Decide the machine-readable output convention and the human-readable default rendering
- [x] Define the exit-status mapping for the three outcome classes
- [x] Decide whether the CLI auto-spawns a daemon or only attaches, resolving the plan's open question
- [x] Write `Designs/CommandLineInterface/README.md` following the design template
- [x] Dispatch a reviewer and address Critical and Major findings

### Notes
Revision boundary: an approved interface design exists; no CLI code is written. The artifact is the deliverable.

The starting sketch from Designs/RepoLocalDaemon Decision 8: a `--json` flag selecting machine output, subcommands mirroring tool names, and exit `0` / `1` / `2` for success / tool error / operational failure. Treat it as a starting point, not a conclusion — the point of deferring was to design against real signatures.

`ToolOk`'s three outcomes map naturally onto the exit-status classes, and `ToolOk::Text` is the case that needs thought: a mermaid diagram and a non-callable advisory are both text successes but want different human rendering.

**What review changed.** The first review round returned Needs-changes with one Critical and two Majors, all substantive: (1) the "attach-only" claim was false against `daemon.rs` primary sources — the unmodified proxy spawns a detached `--serve` contender whenever no compatible daemon is attachable and runs the replacement protocol (up to hard-kill) against an incompatible one; resolved by specifying a new `--attach-only` proxy mode, scoped into task 7.2. (2) Two landed query tools (`find_overrides`, `find_class_candidates`) were silently dropped and the subcommand arithmetic was wrong; both added, count corrected to 21. (3) The daemon/standalone `indexed` asymmetry (a daemon never loads the cache at startup) created a reachable FR-18 violation; resolved by Decision 7's unindexed-daemon fallback, with the daemon-loads-cache-at-startup alternative recorded as a phase-3-owned follow-up candidate. Second round: Approve-with-minors; the minor (error-table exit codes for the fallback rows) and nits applied; status `approved`.

### Completion Evidence

- Verified: 2026-08-19
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `e3a4cfdbf1697aeab9652cf03d83439cba7f797b`
- Identity recheck: `git rev-parse HEAD` at 2026-08-19 13:58 matched `e3a4cfdbf1697aeab9652cf03d83439cba7f797b`
- Focused review: independent design review, two rounds (Needs-changes → all Critical/Major findings addressed → Approve-with-minors → minors applied)
- Reviewed candidate / final: `e3a4cfdbf1697aeab9652cf03d83439cba7f797b`
- Review result: PASS/Aligned (design status `approved`)

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `git show e3a4cfd --stat` | `.` | PASS | `Designs/CommandLineInterface/README.md exists (434 lines) with status: approved, covering the full task verification surface: 21-subcommand table mapped one-to-one onto core:: functions with landed-signature fidelity (find_overrides Page<CallChain> paging; find_class_candidates bare Vec<SymbolResult>); machine-readable convention (payload-defined, compact serde_json, ToolOk::Text verbatim); human default (one renderer fed from payload JSON); exit-status mapping 0/1/2 with the corrupt-vs-unreadable line drawn on the Graph::load contract.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `design review (independent)` | `two rounds against spec FR-17..20 / AC-11/12/33/40, phase traps, and primary sources (core/mod.rs, handlers/mod.rs tool_success_json, daemon.rs, mmap.rs)` | PASS | `Round 1: Needs-changes — 1 Critical (proxy auto-spawn contradiction), 2 Major (omitted tools; indexed asymmetry), 3 Minor, 1 Nit. All Critical/Major resolved substantively (verified against daemon.rs:723-735/1211-1223, core/query.rs:214, core/structure.rs:390, core/analyze.rs:324/584). Round 2: Approve-with-minors — remaining minor + nits applied (fallback-row exit codes, daemon.rs line count, fixture mechanism naming, watch-divergence observation recorded). No unresolved Critical or Major findings.` |

### Trap
The design's own first draft walked into it: claiming "attach-only" while delegating attachment to a component whose contract is attach-or-spawn-or-replace. Review against primary sources, not against the summary in another design's decision.

## 7.2: code-graph binary over the typed core

### Subtasks
- [x] Create the `code-graph` binary crate with `clap`, added to the workspace members
- [x] Implement subcommands calling `core::` functions directly
- [x] Route through `core::` with an HONEST `indexed` flag (gate artifact 17 follow-up: the `pub handlers::*` layer hardcodes `indexed=true` and is an unguarded entry surface — the CLI must not inherit that shortcut; revisit the guard shape here)
- [x] Add `--attach-only` proxy mode to `code-graph-mcp` (design Decision 3: attach to a published, compatible, live daemon or serve in-process — never spawn a contender, never initiate the replacement protocol)
- [x] Implement daemon attachment by spawning `code-graph-mcp --attach-only` as a child (design Decision 3), with standalone as the fallback and the Decision 7 unindexed-daemon retry
- [x] Wire the three phase 1 queries through their `core::` functions — migrated there by phase 2 tasks 2.3 through 2.5 — so AC-33's CLI half is satisfied
- [x] Tests for daemon and standalone parity and the unindexed error path

### Notes
Revision boundary: the CLI answers queries end to end in both modes.

`clap` is a new workspace dependency scoped to this binary. It does not enter any of the four protected crates, so NFR-02 is unaffected.

The CLI must not construct an rmcp type anywhere. If a subcommand finds itself deserializing a `CallToolResult`, the typed core is being bypassed and FR-17 is violated in spirit even if the output happens to match.

### Completion Evidence

- Verified: 2026-08-19
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `43f1e0092ffdfb0f343278ba90bd7f59a3321eb6`
- Identity recheck: `git rev-parse HEAD` at 2026-08-19 14:24 matched `43f1e0092ffdfb0f343278ba90bd7f59a3321eb6`
- Focused review: `git show 43f1e0092ffdfb0f343278ba90bd7f59a3321eb6`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `43f1e0092ffdfb0f343278ba90bd7f59a3321eb6`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-cli` | `.` | PASS (`exit 0`) | `5 integration tests passed: the three phase-1 queries invocable via the CLI with parsing machine payloads (AC-33 CLI half); exit statuses separate the three outcome classes including the byte-exact unindexed domain error computed from the core guard and the directory-shaped-cache operational fixture; the no-rmcp/no-handlers structural guard; AC-40 byte-identity with and without a daemon including the Decision 7 unindexed-daemon window; attach-only on stale metadata answers byte-identically and leaves no daemon.lock behind.` |
| `make verify` | `.` | PASS (`exit 0`) | `clippy -D warnings clean across the workspace including the new crate; fmt clean; full workspace tests green; no pending snapshots; plugin mirrors in sync.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `focused diff review` | `git show 43f1e00` | PASS | `13 files: the new crate (args/exec/daemon_client/main — no rmcp dependency, no handlers import, #![forbid(unsafe_code)]); proxy_attach_only in daemon.rs reuses the full proxy's helpers with the same post-connect owner revalidation, fails fast on the outcomes retrying cannot change, and contains no spawn_contender/request_replacement call; main.rs gives --no-daemon and --serve precedence over --attach-only; the three core re-exports carry doc comments naming the design decision; adapter-owned bool defaults in exec.rs mirror server.rs's unwrap_or values verbatim (top_level_only false, brief true, count_only false, near false, styled false, force false).` |

### Trap
Reaching for the MCP handlers because they are already wired and their signatures are familiar. That produces a CLI that parses JSON out of a wire envelope to re-render it — the exact duplication Track A existed to prevent, and it will pass every parity test while making the next response-shape change a two-place edit.

## 7.3: Output parity and exit-status behaviour

### Subtasks
- [x] Implement the machine-readable output mode per the 7.1 design (landed with 7.2 — payload-defined, required there for the AC-40 tests)
- [x] Implement the human-readable default rendering
- [x] Implement the exit-status mapping (landed with 7.2 — required there for the unindexed-error test)
- [x] Add parity tests across the five distinct response shapes
- [x] Add exit-status tests for the three outcome classes (landed with 7.2's `exit_statuses_separate_the_three_outcome_classes`)
- [x] Update CLAUDE.md and the plugin README with CLI usage

### Notes
Revision boundary: the CLI is complete and its output contract is verified.

The five shapes are chosen to cover structurally different envelopes, not five arbitrary tools: a plain `Page`, a non-`Page` tree, a flattened envelope with a `skip_serializing_if` field, a dual-page response with no top-level `results`, and a non-JSON body. A parity bug will live in one of those shapes, not in a particular tool.

`search_symbols` must be exercised both with and without `suggestions`, since the field is absent rather than null when empty — a renderer that assumes presence fails only on the populated path, or only on the empty one.

### Completion Evidence

- Verified: 2026-08-19
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `cca67059873ecbebfb250d3e2473624356f7eec3`
- Identity recheck: `git rev-parse HEAD` at 2026-08-19 14:37 matched `cca67059873ecbebfb250d3e2473624356f7eec3`
- Focused review: `git show cca67059873ecbebfb250d3e2473624356f7eec3`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `cca67059873ecbebfb250d3e2473624356f7eec3`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-cli` | `.` | PASS (`exit 0`) | `11 tests passed: the six-test parity suite pins AC-11 across the five distinct response shapes — Page (get_callers), tree (get_class_hierarchy), flattened envelope BOTH with and without suggestions (search_symbols, absent-vs-present), dual-page (get_coupling both), non-JSON mermaid body — each comparing CLI --json bytes against the REAL adapter path (core:: through to_call_tool_result, payload extracted from the serialized envelope without naming an rmcp type); the human-mode pin shows the did-you-mean footer tracks field presence; the five task-7.2 tests (AC-33, exit statuses incl. the byte-exact unindexed error, no-rmcp guard, AC-40 daemon/standalone byte-identity incl. the Decision 7 window, attach-only stale-metadata) stay green.` |
| `make verify` | `.` | PASS (`exit 0`) | `clippy -D warnings clean; fmt clean; full workspace tests green; no pending snapshots; plugin mirrors in sync (plugin/README.md is hand-maintained canonical content, not a fanned-out mirror).` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `focused diff review` | `git show cca6705` | PASS | `5 files: render.rs keys on structural family (results+total → Page; incoming+outgoing → dual-page; hierarchy → tree; array → table; else labelled lines) so no per-subcommand renderer can drift; the suggestions footer reads field PRESENCE, matching the skip_serializing_if contract; main.rs human mode now renders from the parsed payload and ToolOk::Text stays verbatim in both modes; CLAUDE.md gains the CLI section + workspace-map row and the tool table now enumerates all 25 tools (the design review's 23-vs-25 reconciliation); plugin/README.md CLI usage names the payload-identity property the skills rely on.` |

## Acceptance Criteria

- [x] **AC-11**: CLI machine-readable output equals the MCP payload for each of the five distinct response shapes (FR-17, FR-19). — six-test parity suite (`tests/parity.rs`): Page, tree, flattened envelope BOTH with and without `suggestions`, dual-page, non-JSON mermaid — each byte-compared against the real `to_call_tool_result` adapter path
- [x] **AC-12**: Exit status distinguishes success, tool error, and operational failure (FR-20). — `exit_statuses_separate_the_three_outcome_classes`: 0 incl. `found:false`, 1 incl. the byte-exact unindexed domain error, 2 via the directory-shaped-cache genuine I/O fixture; structurally invalid/version-mismatched cache = honest-unindexed = 1, while a bytecheck-valid but semantically inconsistent main archive is operational corruption per the `Graph::load` contract
- [x] **AC-33**: Position lookup, shortest path, and community detection are invocable from both the MCP surface and the CLI, completing the criterion opened in phase 1 (FR-26). — `phase_one_queries_are_invocable_from_the_cli`; `limit`/`offset`/`max_bytes` flow through the same core functions
- [x] **AC-40**: Identical output with and without a running daemon (FR-18). — `daemon_and_standalone_output_are_byte_identical` covers the steady state AND the Decision 7 unindexed-daemon window; `get-status` carved out (reports daemon-side state) per the design
- [x] No query logic duplicated between the CLI and the MCP adapter; every subcommand calls the typed core (FR-17). — all 21 arms in `exec.rs` call `core::`; no rmcp dependency, no handlers import (structurally pinned by `cli_depends_on_the_typed_core_only`); adapter-owned bool unwraps mirror `server.rs` verbatim
- [x] **AC-27**: `make verify` passes (NFR-04). — green at the phase endpoint (tasks 7.2/7.3 evidence rows)
- [x] FR-17, FR-18, FR-19, FR-20 realized. — spec lane cycle-1 disposition table: all SATISFIED with file:line evidence

## Phase Completion Evidence

- Verified: 2026-08-19
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `35aa82f31c34aaad5c15c8e643f5141f4eebd6d4`
- Identity recheck: `git rev-parse 35aa82f` at 2026-08-19 14:50 matched `35aa82f31c34aaad5c15c8e643f5141f4eebd6d4`
- Focused review: four-lane frozen gate over `10a625377c5ee7fa0f7e972fc38c3808435ff0b4..35aa82f31c34aaad5c15c8e643f5141f4eebd6d4` (one cycle, all four lanes PASS/Aligned; two sub-material record inaccuracies repaired at the planning revision `415cc6d`); design task 7.1 separately reviewed in two rounds (Needs-changes → Approve-with-minors → approved)
- Reviewed candidate / final: `35aa82f31c34aaad5c15c8e643f5141f4eebd6d4`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `make verify` | `.` | PASS (`exit 0`) | `clippy -D warnings (including the new crate), rustfmt check, full workspace tests (natively on Windows), pending-snapshot check, and plugin-mirror sync all green at the phase endpoint (AC-27); only docs-only commits follow the last verified code commit cca6705.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `four-lane phase gate` | `frozen 10a6253..35aa82f, planning 415cc6d` | PASS | `All four independent lanes returned PASS/Aligned on the first cycle; residual observations recorded as accepted follow-ups in the review artifact.` |

### Completed task identities
- `7.1`: `e3a4cfdbf1697aeab9652cf03d83439cba7f797b`
- `7.2`: `43f1e0092ffdfb0f343278ba90bd7f59a3321eb6`
- `7.3`: `cca67059873ecbebfb250d3e2473624356f7eec3`

- Final aligned review: `reviews/19-command-line-interface-final-review-10a6253-35aa82f.md`; frozen: `10a625377c5ee7fa0f7e972fc38c3808435ff0b4..35aa82f31c34aaad5c15c8e643f5141f4eebd6d4`
