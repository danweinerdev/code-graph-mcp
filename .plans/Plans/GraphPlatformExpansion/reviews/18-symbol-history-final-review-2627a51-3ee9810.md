---
title: "Phase review: Symbol History"
type: review
status: resolved
created: 2026-08-19
updated: 2026-08-19
tags: [review, vcs, history, fingerprint, phase-6, final]
related: ["Plans/GraphPlatformExpansion/06-Symbol-History.md"]
review_of: "Plans/GraphPlatformExpansion/06-Symbol-History.md"
rev: "2627a51ab1a0529e8fff0f7365cef0d45b349c2d..3ee981012cbd8d4f61320293938e7b538eb54a76"
review_scope: phase
frozen: true
verdict: Aligned
reviewed_planning_revision: "8fccc8362da50cec133860014f8e0952f5400904"
review_mode: independent
lane_results:
  - lane: review_plan_drift
    result: PASS/Aligned
    reviewed_identity: "2627a51ab1a0529e8fff0f7365cef0d45b349c2d..3ee981012cbd8d4f61320293938e7b538eb54a76"
    evidence: "Task 6.5's record is complete and accurate: four checked subtasks map one-to-one onto e747b14's 3-file diff, the evidence block conforms to the template and its claims (file counts, 14 vcs-git + 13 integration tests, verification commands) all check out; whole-range coherence holds after two fix rounds (6.1-6.4 subtasks map to their commits, no landed code lacks a record); all cycle-2 accepted follow-ups are honestly recorded and defensibly minor; statuses/dates coherent for a pre-close doc. One minor advisory (CLAUDE.md window_filled gloss still overclaimed) — repaired at the planning revision."
  - lane: review_quality
    result: PASS/Aligned
    reviewed_identity: "2627a51ab1a0529e8fff0f7365cef0d45b349c2d..3ee981012cbd8d4f61320293938e7b538eb54a76"
    evidence: "The 6.5 fix is correct and regression-free: find_blob maps to Operation with a load-bearing contract comment, NotFound remains only on the lookup_entry-None arm; the gitlink regression test is hermetic, deterministic, and fails closed on a revert; the truncation test makes at_window_boundary attributable only to history_truncated; the window_filled doc fix matches the implementation. Fresh-eyes sweep of the whole phase surface found nothing material; one Low residual (early resolution arms still NotFound -> tombstone, compound near-unreachable race with provably inert poisoning) recorded as a follow-up; parked lists cover every sub-material residual."
  - lane: review_spec_compliance
    result: PASS/Aligned
    reviewed_identity: "2627a51ab1a0529e8fff0f7365cef0d45b349c2d..3ee981012cbd8d4f61320293938e7b538eb54a76"
    evidence: "e747b14 regressed nothing: blame's FR-27..32 routing is untouched (its NotFound unavailability keys on provider.blame, and its read_at staleness probe maps every error to stale_reason, never a tombstone); the truncation gap is closed end to end; NFR-02 verified structurally (empty Cargo.toml/Cargo.lock diff over the range). All ten AC lines dispositioned SATISFIED except AC-27 (CLAIM-PRESENT — make verify evidence recorded at tasks 6.3/6.4/6.5; independently executed green by the gate runner). FR-34's final AC line matches endpoint behavior exactly: upfront data-independent literal_insensitive rejection, default-hook None contract, no silent fallback anywhere. Nothing found that would make ticking any AC checkbox dishonest."
  - lane: review_blind_spots
    result: PASS/Aligned
    reviewed_identity: "2627a51ab1a0529e8fff0f7365cef0d45b349c2d..3ee981012cbd8d4f61320293938e7b538eb54a76"
    evidence: "Every remaining NotFound producer in read_at enumerated and dispositioned: the lookup_entry-None arm is deterministic tree membership at an immutable rev (safe to tombstone); the early resolution arms require a gc-prune race whose false tombstone is provably inert (a pruned rev is unreachable from HEAD and never re-emitted by the HEAD-rooted walk); ownership/relative-path arms are pre-empted by revisions_touching running the identical checks first, routing to success-shaped unavailability before any tombstone. Both regression tests genuinely exercise their arms. Config identity is snapshotted once and feeds both key sites — a mid-walk watch reindex cannot split the key. Concurrent daemon walks are torn-read-safe (atomic rename, pid+sequence temp names, corrupt-shard self-healing). The 6.5 Trap paragraph's skip-vs-tombstone wording inaccuracy — repaired at the planning revision. Nothing material hides in the follow-up lists."
findings: []
followups:
  - "read_at early resolution arms (rev_parse_single/object/peel_to_commit) still map to NotFound -> tombstone; compound near-unreachable race with provably inert poisoning, but a two-line remap to Operation for provider-issued full OIDs would make NotFound provably mean 'tree has no entry' end to end."
  - "blame's find_blob arm still maps blob-read failure to NotFound — inconsistent with read_at's corrected semantics; benign (blame's NotFound routes to uncached success-shaped unavailability) but the 'no history for this path' wording is misleading for a partial-clone missing blob."
  - "Skip-adjacent transitions attribute to the first examined revision after the skip with no per-entry uncertainty marker (global skipped list permits client-side reconstruction)."
  - "Unbounded cold-cache prefetch memory: up to window(<=500) full revision snapshots buffered simultaneously before the walk; cap or chunk if multi-MB generated files with deep histories surface."
  - "NFR-10 test gates the provider await, not the blocking pool; the CPU-half guarantee rests on inspection of the spawn_blocking placement. A gated parse_file double would pin the mechanism."
  - "Synchronous shard reads in the async prefetch loop (up to 500 std::fs reads on a runtime worker); shard-level batching would remove the cost."
  - "config_identity hashes the whole RootConfig, so extraction-irrelevant knobs ([response].max_bytes, [daemon].*) over-invalidate fingerprint entries; compounds shard growth across config edits."
  - "binary_identity constant-fallback aliasing (two different builds both failing to stat their executable share identity 0); shards grow monotonically across rebuilds — 'safe to delete' covers it operationally."
  - "Rust lifetime-list mis-lex in the default fingerprint: <'a,'b> vs <'a, 'b> hash differently (char_literal_end accepts the 'a, ' run), so one rustfmt normalization reports modified — phase 8 per-language override territory."
  - "Dead in-walk literal_insensitive arm's wording diverges from the upfront rejection's; delete or align when phase 8 revives the mode."
  - "Shared key-builder for the two FingerprintKey construction sites would make field divergence structurally impossible."
  - "Unit-separator (\\u{1f}) in a hostile filename could theoretically alias key fields; identifiers and revs cannot contain it."
  - "D-0005 ledger confirmation ('produces Removed then Introduced') is half-observable for a rename persisting to HEAD — the old symbol ID leaves the graph, so only the introduced half is queryable; wording is aspirational, not wrong."
---

# Phase review: Symbol History

Reviewed `Plans/GraphPlatformExpansion/06-Symbol-History.md` at frozen
identity `2627a51..3ee9810` (planning content at `8fccc83`).
**Review mode:** independent lanes — four parallel, non-inheriting contexts
with isolated inputs, dispatched three times.

## Cycle history

- **Cycle 1** (endpoint `5bd386b`, planning `5bd386b`): three lanes
  PASS/Aligned; blind-spots returned Needs-changes. Material M1: the
  history walk parsed raw revision bytes without `preprocess` /
  `synthesize_symbols`, so every `[cpp].macro_*`-dependent symbol — the
  flagship UE configuration — silently reported an empty history and
  cached false tombstones. Confirmed minors: file-deletion commits
  downgraded to skips, `at_window_boundary` blind to skipped-oldest
  revisions, data-dependent `literal_insensitive` rejection, same-process
  shard temp-path collision, undocumented rename/duplicate-name/truncation
  caveats. Fixed as task 6.4 (`d6b3d9c`): the walk now mirrors the indexer
  pipeline exactly and the config's identity joined the fingerprint cache
  key (the invalidation story being half the fix).
- **Cycle 2** (endpoint `bfee8e8`): three lanes PASS/Aligned verifying all
  cycle-1 fixes sound; blind-spots found one NEW material M2 — the git
  provider mapped `find_blob` failure to `NotFound`, which task 6.4's
  deletion fix had just made the deterministic, permanently-cacheable
  absence signal: a partial-clone or corrupt-odb read failure would
  manufacture false `removed`/`introduced` transitions and a false
  tombstone keyed by the immutable rev that never heals. Fixed as task
  6.5 (`e747b14`): `Operation` for the blob arm (`NotFound` reserved for
  the `lookup_entry`-None arm), a gitlink regression test, the
  previously-undispositioned `history_truncated:true` wire test, and the
  `window_filled` doc repair.
- **Cycle 3** (this artifact): all four lanes PASS/Aligned. Two
  sub-material record inaccuracies (CLAUDE.md `window_filled` gloss, 6.5
  Trap skip-vs-tombstone wording) repaired at the planning revision
  `8fccc83`. Residual observations recorded above as follow-ups; none is
  material to the phase deliverable.

## Verification

- `make verify` at the endpoint: PASS (`exit 0`) — clippy `-D warnings`,
  rustfmt, full workspace tests (natively on Windows), pending-snapshot
  check, plugin-mirror sync.
- `cargo test -p code-graph-tools --test symbol_history` (13):
  transitions-only (AC-19/AC-20), no-temp-file (AC-37), removed +
  reintroduced, file-deletion `removed` with zero skips, case-only rename
  (D-0005), window boundary + clamp echo, provider truncation on the wire,
  skipped-oldest boundary ambiguity, macro-config real history (M1),
  success-shaped unavailability (FR-36), mode errors, cache idempotence,
  NFR-10 provider isolation.
- `cargo test -p code-graph-vcs-git` (14): including the gitlink
  unreadable-blob `Operation` pin and the `RevisionWindow` truncation
  contract.
- `cargo test -p code-graph-tools --lib fingerprint_cache` (7): hit,
  tombstone, key isolation (rev/symbol/mode/config), deleted directory,
  corrupt shard, graph-cache independence, config-identity determinism.

## Resolution Log

No open findings; the follow-ups above are accepted, recorded
non-blockers.
