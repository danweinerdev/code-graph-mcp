# Phase 11 debrief — Windows Platform Completion

Closed 2026-08-20. Three tasks (two landed in the 2026-08-18 pull-forward
series, one certification task), two gate cycles, final aligned review at
`reviews/22-windows-platform-completion-final-review-3dc41a9-385cc31.md`.

## What happened

- **The pull-forward decision paid off.** Deferring Windows to "after the
  Linux MVP" would have meant certifying five phases of history/CLI/
  fingerprint work on Linux and then re-discovering their Windows
  failures months later. Instead, phases 5–9 were BUILT on the Windows
  runner after the 11.1/11.2 seams landed, so the certification task
  found a surface that already worked — the matrix largely RECORDS
  continuous native verification rather than performing a big-bang port.
- **Certification is a documentation discipline, not a test run.** The
  gate's spec lane found no code defects at all; every finding was a
  claim whose evidence didn't match its wording: a spec AC that D-0014
  had amended in intent but nobody had edited, a "pipe-security-
  descriptor inspection" satisfied by a directory-DACL check, a Linux
  "unchanged and green" AC claiming an execution never performed, and a
  watch row citing a test that deliberately bypasses the OS watcher.
  The remediation was entirely textual — amend, disclose, re-scope —
  and the record is stronger for stating exactly what was and wasn't
  witnessed.
- **The honest-boundary pattern recurred.** Verbatim-UNC passthrough,
  the uninspected default pipe SD, NTFS case-insensitivity, the
  auto-skipping dogfood baselines: each is now a disclosed boundary or
  known omission in the matrix rather than an implied capability.
- **D-0014's confirmation field had drifted from reality.** The ledger
  claimed the spec reconciliation had happened; it hadn't. The gate
  caught it because the spec lane read the SPEC, not the ledger's
  summary of it — the same read-the-primary-source lesson as phase 7's
  design review.

## What to carry forward

- **When a decision amends a spec, edit the spec in the same commit.**
  A ledger entry whose confirmation field claims an edit that never
  landed is drift wearing a decided-truth costume.
- **Certification matrices should cite the test that exercises the
  claim, not the test that shares its topic.** `watch_dangling_edges`
  passes natively and touches watch code — and proves nothing about
  `ReadDirectoryChangesW`, because it bypasses the watcher by design.
  The fix was knowing which suites call `watch_start`.
- **State the provenance you have, not the provenance that sounds
  better.** "Phases 1–4 and 5.1–5.3 certify as members of today's green
  umbrella" is a weaker-sounding but fully defensible claim; "everything
  ran natively" was falsified by one commit message. The weaker claim
  survives audit; the stronger one cost a gate finding.
- **Cross-platform ACs need per-platform witnesses.** "Linux suites
  remain unchanged and green" was written when Linux was the dev host;
  once the host flipped to Windows, half the AC became unwitnessable.
  Phase 10 carries the same wording and should be reworded before it
  certifies (recorded in artifact 22).
- **Open follow-ups** are in artifact 22, led by the Linux
  re-verification (`make verify` on a Linux runner at or after this
  identity), then the D-0014 ledger annotation for the DACL
  substitution, the phase 10 AC reword, the NTFS case pin, the DACL
  grant-line parse, the 8.3 short-form dedicated pin, and initializing
  the dogfood submodules on the runner.

## Plan state after this close

Phases 1–9 and 11 are complete and frozen-reviewed. Phase 10 (macOS) is
the only remaining phase, deferred by design and requiring a native
macOS runner. The GraphPlatformExpansion plan is complete for every
platform this workspace can currently witness.
