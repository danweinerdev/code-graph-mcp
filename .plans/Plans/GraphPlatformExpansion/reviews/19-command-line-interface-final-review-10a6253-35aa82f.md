---
title: "Phase review: Command-Line Interface"
type: review
status: resolved
created: 2026-08-19
updated: 2026-08-19
tags: [review, cli, clap, typed-core, daemon, parity, phase-7, final]
related: ["Plans/GraphPlatformExpansion/07-Command-Line-Interface.md"]
review_of: "Plans/GraphPlatformExpansion/07-Command-Line-Interface.md"
rev: "10a625377c5ee7fa0f7e972fc38c3808435ff0b4..35aa82f31c34aaad5c15c8e643f5141f4eebd6d4"
review_scope: phase
frozen: true
verdict: Aligned
reviewed_planning_revision: "415cc6dd219822aa3849fae2566a5eb6cce160e1"
review_mode: independent
lane_results:
  - lane: review_plan_drift
    result: PASS/Aligned
    reviewed_identity: "10a625377c5ee7fa0f7e972fc38c3808435ff0b4..35aa82f31c34aaad5c15c8e643f5141f4eebd6d4"
    evidence: "The range contains exactly the seven commits the record claims with nothing untraceable to phase 7; task 7.1's verification holds (approved 434-line design covering the full promised surface, two-round review narrative consistent across the phase doc, commit message, and design content); every checked 7.2/7.3 subtask maps to landed code including the three 'landed with 7.2' claims; evidence blocks conform to the template with claims that check out against the diffs (file counts, test counts 5+6=11, described behaviors); statuses coherent pre-gate. One minor (the 7.3 verification filter 'parity::' selects zero tests — the evidence itself used the unfiltered run) — repaired at the planning revision; one cosmetic nit."
  - lane: review_quality
    result: PASS/Aligned
    reviewed_identity: "10a625377c5ee7fa0f7e972fc38c3808435ff0b4..35aa82f31c34aaad5c15c8e643f5141f4eebd6d4"
    evidence: "The standalone bootstrap composes the identical six-plugin + git-provider shape as the MCP binary (the project-root vs cwd binding divergence is verified benign — gix discovers upward, and the CLI's choice matches the daemon's); all six adapter-owned unwrap_or defaults are byte-faithful to server.rs including get_orphans's Option pass-through; indexed is honest with the Ok(false)→1 / Err→2 line exactly per Decision 5; no lock held across an await; the JSON-RPC client sequences correctly, maps errors per the design table, and never consults the child's exit status; proxy_attach_only fail-fasts correctly and keeps the full proxy's post-connect revalidation; parity is enforced structurally (single serializer, single renderer). Findings confined to test hygiene and edge-case UX; the design-vs-code handshake-failure row was reconciled at the planning revision."
  - lane: review_spec_compliance
    result: PASS/Aligned
    reviewed_identity: "10a625377c5ee7fa0f7e972fc38c3808435ff0b4..35aa82f31c34aaad5c15c8e643f5141f4eebd6d4"
    evidence: "FR-17/18/19/20, AC-11/12/33/40, NFR-02 all SATISFIED at the endpoint with file:line evidence: all 21 subcommand arms call core::, no rmcp/handlers (structurally test-pinned), tool_call is transport framing not duplicated logic; the byte-identity tests cover the steady state AND the Decision 7 window with the get-status carve-out documented; the six-test parity suite covers exactly AC-11's five shapes with both suggestions arms through the REAL adapter path; the exit trichotomy including the corrupt-vs-unreadable line is implemented and test-pinned; the protected crates' manifests have an empty range diff (clap scoped to the CLI crate, D-0004/D-0014 untouched). Nothing found that would make ticking any AC checkbox dishonest at phase close."
  - lane: review_blind_spots
    result: PASS/Aligned
    reviewed_identity: "10a625377c5ee7fa0f7e972fc38c3808435ff0b4..35aa82f31c34aaad5c15c8e643f5141f4eebd6d4"
    evidence: "Nine attack surfaces probed. Sound: the Decision 7 byte-exact fallback same-build (guard text identical at both sites, tool_error adds no prefix, content[0].text read verbatim); every tool_call field type against the server Args structs (explicit nulls deserialize to None; force bare-bool ≡ unwrap_or(false)); cache concurrency (unique temp + fsync + atomic rename; mmap pins the old inode; TOCTOU standalone-read-vs-daemon-save is torn-read-free); stderr attribution unambiguous (code-graph-mcp: vs code-graph: prefixes). Confirmed minors filed as follow-ups: attach-only never removes dead-owner daemon.json (permanent spawn+handshake double hop after a daemon crash — answers stay correct); the Decision 7 wording match is unversioned against a split-install sibling binary; the stale-metadata test exercises the binary-incompatible arm rather than the owner-liveness arm its comment claims; optional-bool flags swallow a following positional (loud clap error); the no-rmcp source scan omits render.rs."
findings: []
followups:
  - "proxy_attach_only never removes dead-owner daemon.json: after a daemon crash, every CLI call pays child spawn + handshake + not-indexed + Decision 7 retry indefinitely (answers stay correct). The child's own no-live-owner determination is exactly the daemon's stale-deletion predicate — a cheap cleanup hook."
  - "Decision 7's byte-exact wording match is unversioned across split-install binaries: a sibling code-graph-mcp from a different build whose not-indexed wording changed makes the fallback silently stop firing (exit 1 with the error instead of a cache answer). The initialize response's serverInfo is currently discarded; a version check there would close it."
  - "The stale-metadata CLI test exercises proxy_attach_only's binary-incompatible arm, not the owner-liveness arm its comment claims (the fabricated all-zeros fingerprint fails metadata_compatible first). The asserted contract holds on both arms; fabricate with the real fingerprint or add a second fixture to cover the dead-owner arm."
  - "The design test plan's 'live but binary-incompatible daemon triggers no replacement' bullet is code-verified but not test-pinned; a refactor of proxy_attach_only could regress it silently."
  - "Optional-bool flags (num_args 0..=1) swallow a following positional: `--brief foo.rs` is a loud clap error; a positional literally spelled true/false is silently eaten. Document positional-first ordering / `=` syntax in the flag help, or accept as clap-idiom cost."
  - "The no-rmcp/no-handlers guard test scans a hardcoded four-file list that omits render.rs; iterate src/ instead."
  - "The daemon parity test spawns --serve without a kill-on-drop guard: an assert firing before stop_daemon leaks a live daemon for up to idle_timeout_secs and can fail TempDir cleanup on Windows. RAII guard per the daemon_serve precedent."
  - "Windows stop_daemon writes shutdown.request then waits unboundedly; a bounded wait with a diagnostic would match the suite's sentinel convention."
  - "render.rs cosmetics: no cell wrapping (one huge cell pads every row), chars().count() is not display width (CJK/emoji misalignment), and human-mode tables key columns on the first row's fields (fields absent from row 1 are dropped). Machine mode is unaffected — the payload is verbatim."
  - "Daemon-backend Json-vs-Text classification is by parseability: a hypothetical ToolOk::Text body that IS valid JSON would be human-rendered as structure. No current text body parses as JSON; machine mode unaffected."
---

# Phase review: Command-Line Interface

Reviewed `Plans/GraphPlatformExpansion/07-Command-Line-Interface.md` at
frozen identity `10a6253..35aa82f` (planning content at `415cc6d`).
**Review mode:** independent lanes — four parallel, non-inheriting contexts
with isolated inputs, dispatched once.

## Cycle history

- **Design review (task 7.1, pre-gate):** two rounds. Round 1 returned
  Needs-changes with one Critical — the "attach-only" claim was false
  against `daemon.rs` primary sources (the unmodified proxy spawns a
  `--serve` contender and can hard-kill an incompatible daemon via the
  replacement protocol) — plus two Majors (two landed query tools
  silently dropped from the subcommand table; the daemon/standalone
  `indexed` asymmetry created a reachable FR-18 violation). All resolved
  substantively: the `--attach-only` proxy mode, the 21-subcommand
  table, and Decision 7's unindexed-daemon fallback. Round 2:
  Approve-with-minors; minors applied; design `approved`.
- **Phase gate (this artifact):** all four lanes PASS/Aligned on the
  first cycle. Two sub-material record inaccuracies repaired at the
  planning revision `415cc6d` (the 7.3 verification filter string; the
  design's handshake-failure row, reconciled to the implementation's
  spawn-failure-only retry rationale). Residual observations recorded
  above as follow-ups; none is material to the phase deliverable.

## Verification

- `make verify` at the endpoint: PASS (`exit 0`) — clippy `-D warnings`
  including the new crate, rustfmt, full workspace tests (natively on
  Windows), pending-snapshot check, plugin-mirror sync.
- `cargo test -p code-graph-cli` (11): the six-test AC-11 parity suite
  (Page, tree, flattened envelope with BOTH suggestions arms, dual-page,
  non-JSON mermaid — each byte-compared against the real
  `to_call_tool_result` adapter path) plus the human-mode
  suggestions-presence pin; AC-33's three phase-1 queries; the exit
  trichotomy including the byte-exact unindexed domain error and the
  directory-shaped-cache operational fixture; the no-rmcp/no-handlers
  structural guard; AC-40 byte-identity with and without a daemon
  including the Decision 7 window; attach-only leaves no contender
  behind on stale metadata.

## Resolution Log

No open findings; the follow-ups above are accepted, recorded
non-blockers.
