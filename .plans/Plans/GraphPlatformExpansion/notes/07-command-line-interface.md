# Phase 7 debrief — Command-Line Interface

Closed 2026-08-19. Three tasks (design + binary + parity), one gate cycle
(all four lanes PASS/Aligned on the first pass), final aligned review at
`reviews/19-command-line-interface-final-review-10a6253-35aa82f.md`.

## What happened

- **The deferred design paid for itself in one review round.** Decision 8
  of RepoLocalDaemon postponed the CLI design until the typed core
  existed; designing against landed signatures made the doc short and
  concrete. The design review's Critical was the phase's most valuable
  finding: the draft claimed "attach-only" while delegating attachment to
  the unmodified proxy, whose actual contract is attach-or-spawn-or-
  replace — a drive-by CLI query would have left a resident daemon behind
  or hard-killed another session's daemon. The fix (`--attach-only` proxy
  mode) was cheap because it was caught at design time, before any CLI
  code assumed the wrong contract.
- **Parity was made structural, then tested.** Machine mode is DEFINED as
  the MCP payload (same `serde_json::to_string`, or the daemon's payload
  text verbatim), and human mode renders FROM that payload — so AC-11 and
  AC-40 collapse into one property the six-test parity suite pins against
  regression rather than establishes.
- **The `indexed` asymmetry was a real FR-18 violation found on paper.**
  A daemon never loads the cache at startup, so "daemon running but not
  yet analyzed" would have answered differently than standalone. Decision
  7's unindexed-daemon fallback (byte-exact match on the guard text,
  computed from `core::require_indexed(false)` rather than duplicated)
  restored identical-output structurally; the test drives the actual
  window.
- **The gate passed first-cycle with only sub-material findings** — the
  first phase in this plan to do so. Two record inaccuracies (a
  verification filter string that selects zero tests; a design row
  contradicting the implemented spawn-failure-only retry) were repaired
  at the planning revision; the rest are recorded follow-ups in artifact
  19, the most concrete being attach-only's lack of stale-metadata
  cleanup (permanent double hop after a daemon crash) and the unversioned
  Decision 7 wording match across split-install binaries.

## What to carry forward

- **Review designs against primary sources, not against another design's
  summary of them.** The attach-only Critical existed because the draft
  trusted RepoLocalDaemon's one-line description of the proxy instead of
  reading `daemon.rs`. The reviewer that read the code caught it.
- **"Defaults live in one place" needs an explicit adapter inventory.**
  clap declaring no defaults pushed almost everything into core, but six
  bool resolutions live in the MCP adapter layer; the CLI had to mirror
  them verbatim, and the evidence records exactly which. Any future
  front-end should start from that list — or better, push those unwraps
  into core and delete the class of drift.
- **Computed constants beat duplicated strings.** The Decision 7 trigger
  extracts the not-indexed message by calling the guard, so the CLI can
  never drift from the server's wording within one build. The recorded
  follow-up (cross-build skew) is the boundary of that technique, not a
  flaw in it.
- **Test-filter strings in verification fields must select something.**
  Second phase in a row where a frontmatter filter (`symbol_history::`,
  then `parity::`) matched zero tests while the real verification ran
  unfiltered. Write `--test <file>` filters, not module-path guesses.
- **Open follow-ups** are in artifact 19: attach-only stale-metadata
  cleanup, versioning the Decision 7 trigger (serverInfo is already in
  hand and discarded), the owner-liveness test arm, the optional-bool
  positional-swallowing clap trap, the guard test's hardcoded file list,
  the daemon-test RAII guard, and render cosmetics. None gate phase 8 or
  9, which have no CLI dependency.
