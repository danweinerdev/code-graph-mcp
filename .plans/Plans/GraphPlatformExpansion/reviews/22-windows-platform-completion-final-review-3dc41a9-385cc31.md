---
title: "Phase review: Windows Platform Completion"
type: review
status: resolved
created: 2026-08-20
updated: 2026-08-20
tags: [review, windows, daemon, named-pipe, dacl, certification, phase-11, final]
related: ["Plans/GraphPlatformExpansion/11-Windows-Platform-Completion.md"]
review_of: "Plans/GraphPlatformExpansion/11-Windows-Platform-Completion.md"
rev: "3dc41a9b0a6dc4ca61bcab8cf66dfc5fdb255d7f..385cc31ded8320dd54d0e094299e847bcaba1730"
review_scope: phase
frozen: true
verdict: Aligned
reviewed_planning_revision: "1f819e43e5afeb06fc6ab363b65c59b0169ee3a8"
review_mode: independent
code_identity_note: "The phase's CODE identity is the pull-forward series ccd9e11..89a2af2 (2026-08-18), reviewed under artifact 14's full-branch adversarial review and re-exercised by every phase 5-9 gate since; the frozen range here is the certification work (task 11.3 + gate repairs)."
lane_results:
  - lane: review_plan_drift
    result: PASS/Aligned
    reviewed_identity: "3dc41a9b0a6dc4ca61bcab8cf66dfc5fdb255d7f..385cc31ded8320dd54d0e094299e847bcaba1730"
    evidence: "All eleven cycle-1 repairs verified present and accurate against primary sources: the dated AC-60 amendment records both deltas and describes D-0014's ledger text correctly (including the self-critical note that D-0014's confirmation field had claimed a reconciliation the spec never received); the corrections entry, the bracketed Linux AC rewording, the four Linux-AC N/A rows, the watch-row swap to watch_race (verified: 3 tests, all via real watch_start), the scoped provenance paragraph, the auto-skip disclosure, the daemon-row label, CLAUDE.md's three v10->v11 sites (against CACHE_VERSION = 11), and the daemon_serve.rs comment. Cycle-2 minors (the phase doc's own evidence table still carrying the old watch citation; the matrix's 'modified' lead verb; the Notes' N/A undercount) — repaired at the planning revision."
  - lane: review_quality
    result: PASS/Aligned
    reviewed_identity: "3dc41a9b0a6dc4ca61bcab8cf66dfc5fdb255d7f..385cc31ded8320dd54d0e094299e847bcaba1730"
    evidence: "Cycle 1 re-executed six sampled matrix rows natively (every observed count matched exactly) and statically verified every non-re-run count including the cfg-aware daemon tallies (23 of 65 unit fns compile on Windows; proxy 15 of 18; persist 29 of 32); the umbrella arithmetic brackets the claimed 1,935. Cycle 2 verified the one code change is comment-only with an accurate D-0014 claim, re-executed both re-cited watch suites (watch_race 3/3 and watch_cpp_macro_strip 1/1, both on the real watch_start backend), and cold-read the repaired matrix: the pipe-SD sentence is accurate against both ServerOptions creation sites, the Linux-status paragraph is accurate against the pull-forward diffs, and no cycle-1-verified count or citation regressed. One cycle-2 minor (the N/A preamble's blanket 'Linux-scoped by their own text' was imprecise for AC-48) — repaired at the planning revision."
  - lane: review_spec_compliance
    result: PASS/Aligned
    reviewed_identity: "3dc41a9b0a6dc4ca61bcab8cf66dfc5fdb255d7f..385cc31ded8320dd54d0e094299e847bcaba1730"
    evidence: "Cycle 1 returned Needs-changes with three moderate certification-text findings (AC-60 never amended per D-0014; directory-DACL delivered where the text said pipe-SD; the Linux AC claiming an execution never performed) plus the missing Linux-AC N/A rows and a stale test comment — all resolved in 385cc31 and verified at cycle 2 against D-0014's exact ledger fields (its scope field does govern the spec, so the amendment's governance claim is accurate). The amendment judged a legitimate reconciliation under the plan-README precedents, not self-serving: it narrows the claim, discloses the pipe-SD gap, and matches D-0014's own confirmation language. All three phase AC lines dispositioned tickable-honestly, conditional only on this artifact being numbered 22 and recording the Linux re-run follow-up — both satisfied here. NFR-06 gained the D-0014 scope note closing the last cold-reader ambiguity."
  - lane: review_blind_spots
    result: PASS/Aligned
    reviewed_identity: "3dc41a9b0a6dc4ca61bcab8cf66dfc5fdb255d7f..385cc31ded8320dd54d0e094299e847bcaba1730"
    evidence: "Cycle 1 confirmed the certification's headline claims trace to real native evidence and identified the watch-row citation overclaim and the cfg-additive overreach (both fixed). Cycle 2 attacked the repairs: the pipe-SD->directory-DACL substitution rides D-0014's confirmation field (which names the directory check as the retained acceptance reference) and the residual uninspected-pipe exposure is exploitable only cross-account — exactly the scoped-out threat — with the gap disclosed, not hidden; the AC-48 rationale verified accurate (the rotation test is NOT unix-gated and runs the full auth/reject/rotate arc natively via the forcing seam); the scoped provenance verified against git dates (phases 6-9 entirely post-pull-forward; 5.1-5.3 pre-date it; 5.4-5.6 after); the DACL test asserts all four claimed properties; the parity suite asserts byte equality across the five shapes. Residual pin-pricks recorded as follow-ups; nothing material."
findings: []
followups:
  - "Linux re-verification: run full `make verify` on a Linux runner at (or after) this identity and record the result — the phase's Linux AC line is certified on the diff-reviewed basis only, and 'green on Linux' cannot be witnessed from the Windows host. This is THE follow-up the reworded AC line and the matrix's AC-60 disposition forward-reference."
  - "D-0014 ledger annotation: the pipe-SD->directory-DACL substitution is authorized implicitly by D-0014's confirmation field ('owner-only DACL inspection, no inherited ACEs') rather than its statement field; a one-line annotation making the substitution explicit under D-0014 would close the seam. Requires no code change; touch the ledger only with the user's awareness per the ledger's own conventions."
  - "Phase 10's doc carries the same original 'Linux acceptance suites remain unchanged and green' AC wording phase 11 just corrected; apply the same reword before that phase certifies (drift lane, out of this range)."
  - "NTFS case-insensitivity has no dedicated pin (two casings of one path resolving to one file); canonicalize-at-boundary resolves to on-disk casing for existing files, which is the operative protection. Recorded in the matrix as a known omission; a pin or a known-limitation line in CLAUDE.md would close it."
  - "The DACL test's 'names the invoking user' assertion checks USERNAME appears anywhere in the icacls listing rather than parsing the grant line specifically; sound on this runner (8.3 TEMP form cannot false-positive) and composite-sound with the other assertions, but a stricter grant-line parse would be more portable."
  - "8.3 short-form TEMP coverage is incidental (every tempdir fixture) rather than pinned; a dedicated short-form <-> long-form canonicalization-equivalence test would survive a TEMP relocation. (The path_normalization suite's short-form test partially covers this.)"
  - "The umbrella tally counts auto-skipped dogfood baselines as passed; disclosed next to the headline number. Initializing external/ submodules on the runner would convert those rows to real executions."
---

# Phase review: Windows Platform Completion

Reviewed `Plans/GraphPlatformExpansion/11-Windows-Platform-Completion.md`
at frozen identity `3dc41a9..385cc31` (planning content at `1f819e4`).
The phase's CODE identity is the pull-forward series `ccd9e11..89a2af2`
(reviewed under artifact 14; re-exercised by every phase 5–9 gate).
**Review mode:** independent lanes — four parallel, non-inheriting
contexts with isolated inputs, dispatched twice.

## Cycle history

- **Cycle 1** (endpoint `089db2c`): drift, quality, and blind-spots
  PASS/Aligned (quality re-executed six matrix rows natively and
  verified every count; blind-spots confirmed the headline claims trace
  to real evidence). The spec lane returned Needs-changes with three
  moderate certification-text findings — none a code defect: (1) AC-60's
  spec text still required "pipe-security-descriptor inspection and
  another-local-account denial" although D-0014 (user-decided
  2026-08-18, spec in scope) had removed the cross-account check and the
  delivered inspection covers the runtime DIRECTORY's DACL; D-0014's own
  confirmation field had claimed a reconciliation that never landed.
  (2) Covered by (1). (3) The phase's Linux AC ("remain unchanged and
  green") claimed a Linux execution never performed, and 11.1's
  "cfg-additive throughout" overstated (unix gates were restructured;
  two shared-code changes touch Linux behavior). Plus minors: missing
  Linux-AC N/A rows, a watcher-bypassing test cited as real-notification
  evidence, a phase-5 native-provenance overclaim, stale CLAUDE.md cache
  versions, and a stale test comment. All repaired in `385cc31`: the
  dated AC-60 amendment (both deltas recorded, corrections entry added),
  the honestly-reworded Linux AC line with the re-run follow-up, the
  four N/A rows, the watch_race citation (verified natively), the scoped
  provenance paragraph, and the precise Linux-status wording.
- **Cycle 2** (this artifact): all four lanes PASS/Aligned. The
  substitution and rewording attacks dissolved against D-0014's own
  fields and the pull-forward diffs. Cycle-2 sub-material findings
  (an un-fanned-out citation in the phase doc's own evidence table, the
  matrix's "modified" lead verb, the Notes' N/A undercount, the AC-48
  preamble imprecision, the NFR-06 cold-reader ambiguity) — repaired at
  the planning revision `1f819e4`. Residual observations recorded above
  as follow-ups; none is material to the phase deliverable.

## Verification

- `make verify` on the native Windows runner (10.0.26100.9106, rustc
  1.94.1): PASS (`exit 0`) — 1,935 passed, 0 failed across all workspace
  test binaries at `ca2e7a8`, with only docs and one comment-only edit
  after it.
- The certification matrix (`notes/11-windows-certification-matrix.md`):
  Windows-path contracts (both `#[cfg(windows)]` verbatim pins, PathTrie
  key semantics, the watch dispatch boundary, real-`ReadDirectoryChangesW`
  suites, `normalize_user_path`), daemon transport/security/lifecycle
  (23+15+9+1 natively, incl. the owner-only DACL inspection), cache v11,
  history (14+9+13), fingerprints (29), candidate count (4 + persist 29),
  CLI parity (5 + 6 byte-equality tests) — sampled rows re-executed by
  the quality lane in both cycles.

## Resolution Log

No open findings; the follow-ups above are accepted, recorded
non-blockers. The Linux re-verification follow-up is the one item the
phase's own AC line forward-references — it lives at the top of the
follow-ups list.
