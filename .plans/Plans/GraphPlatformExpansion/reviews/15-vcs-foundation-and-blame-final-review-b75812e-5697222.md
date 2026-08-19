---
title: "Phase review: VCS Foundation and Blame"
type: review
status: resolved
created: 2026-08-18
updated: 2026-08-18
tags: [review, vcs, blame, phase-5, final]
related: ["Plans/GraphPlatformExpansion/05-Vcs-Foundation-And-Blame.md"]
review_of: "Plans/GraphPlatformExpansion/05-Vcs-Foundation-And-Blame.md"
rev: "b75812ea8819b2ec4382c456f24080b0817e9cd6..569722264dfb83cd781c72ee9c4444eb393aff57"
review_scope: phase
frozen: true
verdict: Aligned
reviewed_planning_revision: "665f327e19a1ddb85c6366401b6c94107c1fb7bc"
review_mode: independent
lane_results:
  - lane: review_plan_drift
    result: PASS/Aligned
    reviewed_identity: "b75812ea8819b2ec4382c456f24080b0817e9cd6..569722264dfb83cd781c72ee9c4444eb393aff57"
    evidence: "All six first-cycle drift defects verifiably resolved at the planning revision (artifact 14 persisted and correctly cited, AC-27 make-verify evidence recorded, checkbox/heading/design/phase-11 repairs landed); task 5.6 fully recorded with a conforming, scope-traceable evidence block; remainder is exclusively close-commit material."
  - lane: review_quality
    result: PASS/Aligned
    reviewed_identity: "b75812ea8819b2ec4382c456f24080b0817e9cd6..569722264dfb83cd781c72ee9c4444eb393aff57"
    evidence: "All three material first-cycle findings correctly fixed without regression: canonicalization-symmetric identity detect (subtree-safe), ownership checks inside spawn_blocking at all three call sites, ranged blame handling CRLF/empty/unterminated blobs with clamp and pre-decode clip, honest staleness tri-state; residuals are diagnostic nits (inert breadcrumb arm, mid-line-CR comment)."
  - lane: review_spec_compliance
    result: PASS/Aligned
    reviewed_identity: "b75812ea8819b2ec4382c456f24080b0817e9cd6..569722264dfb83cd781c72ee9c4444eb393aff57"
    evidence: "FR-27..32, FR-36, FR-47, NFR-02/10/11 and AC-21/22/23/24/34/35/36/44/45/56 all satisfied at the endpoint; the two first-cycle partials (unverifiable staleness reading as clean, AC-36 letter) are closed with pinning tests; the provider timeout remains the recorded OQ-D4 deferral."
  - lane: review_blind_spots
    result: PASS/Aligned
    reviewed_identity: "b75812ea8819b2ec4382c456f24080b0817e9cd6..569722264dfb83cd781c72ee9c4444eb393aff57"
    evidence: "The fixes hold under attack in every mainline path: identity-detect closes the F1 registry hole, ranged blame with pre-decode clipping closes F3/F4, the stale tri-state is exhaustively consistent with no silent unverified state; residual BS2-1/BS2-2 windows require multiple concurrent misfortunes and are filed as follow-ups, not blockers."
findings: []
followups:
  - "BS2-1: harden `require_owned_by_bound_repository`'s fail-open arms — refuse rather than pass when parent discovery errors AND the path no longer exists on disk (deleted nested clone / broken gitlink over an outer-repo shadow copy)."
  - "BS2-2: honest unavailability wording when the indexed root is a git tree the provider is not bound to (say 'provider bound elsewhere', not 'no supported VCS'), and call a linked worktree of the bound repo a different checkout rather than a different repository."
  - "Inert F8 breadcrumb: `GitProvider::open` maps every failure to `Unavailable`, so main.rs's non-Unavailable breadcrumb arm cannot fire — differentiate error kinds or drop the arm."
  - "Inline `vcs.detect` cost: a synchronous gix::discover runs on the async executor once per blame_symbol call (plus rediscovery inside each blocking op); fold detection into the blocking pool or cache it if a network-bound provider ever makes detection expensive."
  - "Provider-neutral default revision: core/history.rs resolves the git-specific \"HEAD\" spec for the rev echo; a `default_rev()`-style trait hook would close the leak before a second provider ships (phase 6)."
  - "Empty-span signal shadowing: the dedicated 'no attributable lines' note is unreachable in its motivating scenario (divergence reason wins); fold the empty-hunk hint into the divergence wording if the distinction matters to agents."
  - "m5 from artifact 14 (queued-sync analyzes lose their progress sink) remains open on the phase 4 surface."
---

# Phase review: VCS Foundation and Blame

Reviewed `Plans/GraphPlatformExpansion/05-Vcs-Foundation-And-Blame.md` at
frozen identity `b75812e..5697222` (planning content at `665f327`).
**Review mode:** independent lanes — four parallel, non-inheriting contexts
with isolated inputs, dispatched twice.

## Cycle history

- **Cycle 1** (endpoint `0dd9115`, planning `83f1146`): plan-drift and
  blind-spots returned Needs-changes. Material findings: F1
  detect-vs-bound-root split (misleading unavailability; wrong-repo
  attribution in nested checkouts), F2 byte-exact staleness permanently
  stale on autocrlf checkouts, F3 whole-file blame cost, plus F4/F5/F6/F8
  and record defects (unpersisted M3/M4/M5 review, missing AC-27 evidence,
  bookkeeping slips). Fixed as task 5.6 (`5697222`) and the record-repair
  commit (`665f327`).
- **Cycle 2** (this artifact): all four lanes PASS/Aligned. Residual
  observations are recorded above as follow-ups; none is material to the
  phase deliverable.

## Verification

- `make verify` at the endpoint: PASS (`exit 0`) — clippy `-D warnings`,
  rustfmt, full workspace tests (1,856 passed, 0 failed, natively on
  Windows), pending-snapshot check, plugin-mirror sync.
- `cargo test -p code-graph-vcs-git` (13) and
  `cargo test -p code-graph-tools --test blame_symbol` (9): PASS — foreign
  repository refusal, EOF clamping, CRLF-clean staleness, unverified
  staleness reporting, registry selection alongside a second double,
  shallow boundary, and the `git blame --porcelain` oracle.

## Resolution Log

No open findings; the follow-ups above are accepted, recorded
non-blockers.
