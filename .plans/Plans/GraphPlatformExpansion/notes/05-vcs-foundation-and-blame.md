# Phase 5 debrief — VCS Foundation and Blame

Closed 2026-08-18. Six tasks (four planned + two added mid-phase), two gate
cycles, final aligned review at
`reviews/15-vcs-foundation-and-blame-final-review-b75812e-5697222.md`.

## What happened

- **5.1–5.3 landed before this session** (trait crate, gix provider,
  hermetic harness) and were solid — the abstraction survived contact with
  a second, integer-revision provider unchanged, exactly as D-0002 wanted.
- **5.5 was created by an adversarial review, not by planning.** The
  user-requested full-branch review (artifact 14) found the provider's
  three scale/contract hazards (unbounded revwalk, shallow-clone hard
  error, working-tree blame promise gix cannot keep) before `blame_symbol`
  consumed them. Sequencing the fix task ahead of the tool task was the
  right call: the tool's staleness contract fell out of the corrected
  provider contract, not the other way around.
- **5.4's staleness mechanism diverged from the design and the divergence
  was an improvement.** The design's mtime accessor does not exist in the
  in-memory graph; the shipped disk-vs-blamed-revision content comparison
  detects the actual misattribution hazard directly. The design carries a
  dated reconciliation note.
- **The first gate cycle failed usefully.** Blind-spots found the
  detect-vs-bound-root split (F1) and — on this project's own platform —
  the autocrlf permanent-stale defect (F2); plan-drift found that the very
  review justifying 5.5 had never been persisted. All material findings
  were fixed as task 5.6 within the same phase and re-gated to four PASS
  lanes.

## What to carry forward

- **The intent-blind lane keeps earning its cost.** For the third phase
  running, blind-spots found the defect the plan-aware lanes accepted
  (F1/F2 here; the typed-core cross-crate gap in phase 2; the icacls
  stdout injection in the Windows work). Do not skip it.
- **Reviews that live only in chat are drift.** Task 5.5's justification
  dead-ended until artifact 14 was written. Persist a review artifact in
  the same session the review runs.
- **Provider work should assume the graph's path discipline.** Both real
  bugs in the provider seam (8.3/verbatim form mismatch, native-separator
  tree paths) came from feeding dunce-canonicalized graph paths into a
  crate that had only ever seen its own fixture paths. New provider crates
  should test with graph-shaped inputs from day one.
- **Open follow-ups** are recorded in artifact 15 (fail-open ownership
  arms, unavailability wording, inline detect cost, provider-neutral
  default revision, m5 progress sink). Phase 6 (`symbol_history`) should
  pick up the truncation-flag requirement the revwalk cap doc names, and
  the `default_rev()` trait question, before building on
  `revisions_touching`.
