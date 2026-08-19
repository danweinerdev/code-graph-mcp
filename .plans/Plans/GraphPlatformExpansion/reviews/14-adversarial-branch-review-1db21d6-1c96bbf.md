---
title: "Adversarial full-branch review: phases 1-4 and in-flight phase 5"
type: review
status: resolved
created: 2026-08-18
updated: 2026-08-18
tags: [review, adversarial, full-range, windows, vcs]
related: ["Plans/GraphPlatformExpansion"]
review_of: "Plans/GraphPlatformExpansion"
rev: "1db21d6a2e676ddec7f74b265179f5fb95eb25db..1c96bbf69b225d8a8a4e97a46f912c57b5644d28"
review_scope: "full branch (phases 1-4 closed, phase 5 tasks 5.1-5.3), user-requested adversarial pass"
frozen: true
verdict: Needs changes
reviewed_planning_revision: "1c96bbf69b225d8a8a4e97a46f912c57b5644d28"
review_mode: independent
lane_results:
  - lane: review_plan_drift
    result: FAIL/Needs-changes
    reviewed_identity: "1db21d6a2e676ddec7f74b265179f5fb95eb25db..1c96bbf69b225d8a8a4e97a46f912c57b5644d28"
    evidence: "Code tracked the plan; the record did not — stale README Current State (phase 4 'being rolled back' vs complete), reverted task 4.4 still advertised as landed, phase 5 'planned' with three completed tasks, D-0009 unreconciled, Linux-MVP re-scope unledgered."
  - lane: review_quality
    result: PASS/Aligned
    reviewed_identity: "1db21d6a2e676ddec7f74b265179f5fb95eb25db..1c96bbf69b225d8a8a4e97a46f912c57b5644d28"
    evidence: "Queue/slot and daemon concurrency sound; two majors to fix before merge — Windows clippy -D warnings break from cfg-orphaned code, and a default-on daemon on platforms with zero integration coverage."
  - lane: review_spec_compliance
    result: PASS/Aligned
    reviewed_identity: "1db21d6a2e676ddec7f74b265179f5fb95eb25db..1c96bbf69b225d8a8a4e97a46f912c57b5644d28"
    evidence: "Daemon FRs, queue FRs, and VCS foundation FRs mapped to implementation with tests; FR-49/AC-58 deferred by the spec's own text; one minor NFR-01 letter-violation (tool-description snapshot rebaselines + get_coupling next_offset edge fix)."
  - lane: review_blind_spots
    result: FAIL/Needs-changes
    reviewed_identity: "1db21d6a2e676ddec7f74b265179f5fb95eb25db..1c96bbf69b225d8a8a4e97a46f912c57b5644d28"
    evidence: "M1 pipe transport without admission ack (silent exit-0 request loss), M2 zero Windows integration coverage behind a default-on daemon, M3 blame at:None contract vs gix committed-state behavior, M4 unbounded revisions_touching walk, M5 shallow-clone hard error; minors m1-m6 including the Windows blame path-separator failure."
findings:
  - id: M1
    severity: major
    title: "Windows named-pipe transport had no admission acknowledgement — saturation/idle races ended as silent exit-0 request loss"
    status: fixed
  - id: M2
    severity: major
    title: "Zero Windows integration coverage behind a default-on daemon; clippy -D warnings failed on Windows"
    status: fixed
  - id: M3
    severity: major
    title: "VcsProvider::blame doc promised working-tree attribution for at:None while gix blames committed state"
    status: fixed
  - id: M4
    severity: major
    title: "revisions_touching walked unbounded history with per-edge commit decodes — engine-scale CPU blowup for stale paths"
    status: fixed
  - id: M5
    severity: major
    title: "A shallow-clone boundary hard-errored the whole revisions_touching call instead of terminating like git log"
    status: fixed
  - id: m1
    severity: major
    title: "blame flattened repo-relative paths with native separators into gix tree paths — every nested-path blame failed on Windows (confirmed by failing test)"
    status: fixed
  - id: m2
    severity: minor
    title: "Windows lock recovery propagated NotFound from a remove/create race instead of retrying"
    status: fixed
  - id: m3
    severity: minor
    title: "icacls grants used bare USERNAME (domain/AzureAD-ambiguous) and inherited stdio"
    status: fixed
  - id: m4
    severity: minor
    title: "System::new_all full-system scans inside 50ms attach/replacement poll loops"
    status: fixed
  - id: m5
    severity: minor
    title: "Queued synchronous analyzes silently lose their progress channel (NoopProgressSink)"
    status: open
  - id: m6
    severity: minor
    title: "GitProvider::detect ignored its bound root and gix::discover walks to FS root — latent wrong-repo binding"
    status: fixed
followups:
  - "m5 (queued-sync progress sink) remains open; it predates phase 5 and belongs to the analyze-queue surface (phase 4 follow-up), not the phase 5 gate."
---

# Adversarial full-branch review — phases 1-4 and in-flight phase 5

**Reviewed state:** `1db21d6..1c96bbf` (frozen; a temporary detached worktree
at the endpoint served all four lanes). **Review mode:** independent lanes
(four parallel, non-inheriting contexts with isolated inputs).

Conducted 2026-08-18 at the user's request ("it hasn't had an adversarial
review yet on this branch"), before any of this session's commits. This
artifact persists that review; it was previously delivered only in-session,
which task 5.5 cited as "Review 2986df0-series" — an incorrect pointer this
document replaces.

## Disposition

- **M1/M2/m2/m3/m4** — resolved by the phase 11 pull-forward series
  (`3c6346d`, `8167e73`, `7acbc01`, `3be5b38`, `d3cafcb` and neighbors):
  pipe admission ack, mandatory-lock semantics, handle-inheritance seal,
  Windows-native daemon suites, SID-based ACLs, targeted liveness probes.
- **m1** — resolved by `ccd9e11` (gix tree paths joined with `/`).
- **M3/M4/M5** — resolved by phase 5 task 5.5 (`6dcbf28`): honest blame
  contract, `MAX_REVWALK_COMMITS` cap, shallow-boundary termination.
- **m6** — resolved by phase 5 task 5.6 (`5697222`): detection as an
  identity check plus per-operation owned-by-bound-repository verification.
- **m5** — open; recorded as a phase 4 follow-up above.
- Plan-record findings (stale README rows, D-0009, unledgered re-scope)
  were resolved across `a3bdd55`, `18206b9`, and the phase 5 lifecycle
  commits.

## Verification

- `cargo clippy -p code-graph-mcp --all-targets -- -D warnings` at the
  frozen endpoint: **failed with 5 errors** (confirmed M2's build break).
- `cargo test -p code-graph-vcs -p code-graph-vcs-git` at the endpoint:
  **1 failure** (`git_provider_matches_git_cli_...`, confirmed m1).
- Both reproduce commands are green on the fix commits and stay green
  through `make verify` at `5697222`.
