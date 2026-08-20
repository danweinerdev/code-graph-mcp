# Phase 9 debrief — Resolver Candidate Count

Closed 2026-08-20. Three tasks, one gate cycle (all four lanes
PASS/Aligned on the first pass — second phase running), final aligned
review at
`reviews/21-resolver-candidate-count-final-review-f4b1779-824cfa2.md`.

## What happened

- **The capture-at-the-pick-site framing did the design work.** The whole
  phase rests on one fact: the candidate count exists for exactly one
  moment — inside the resolver, before the losing candidates are
  discarded. Everything else (the sibling-field decision, the v11 bump,
  the triple-returning resolver signatures, both resolve paths stamping)
  followed mechanically from refusing to let that moment pass
  unrecorded.
- **The phase trap was real and the plan's counter-position held.**
  Defaulting pre-bump caches to 1 would have made "unambiguous"
  indistinguishable from "unknown"; the v11 bump plus a non-default
  round-trip test (3, asserted in BOTH adjacency directions) closed it,
  and the field docs state that the serde default is fixture-only.
- **Three wire conventions for one concept, each surface-justified:**
  `CallChain.candidates` plain u32 (every hop was reached by a real
  edge), `PathHop.candidates` Option-null-for-source (mirroring
  `entered_by` exactly), `DiagramEdge.candidates` Option +
  skip_serializing_if (mirroring the `min_confidence` mode boundary).
  The gate's quality lane probed this as possible gratuitous
  inconsistency and endorsed all three.
- **9.3's "retire or demote" resolved to demote-and-document.** The
  inventory found exactly two one-bit surfaces (`entered_by`,
  `heuristic_hops`). Both stay: removal would break the phase's own
  additive AC, the axes are independent by design (the include resolver
  contract already produces Resolved+count-2; `Confidence` is
  `#[non_exhaustive]` for variants where call edges diverge too), and
  `heuristic_hops` is the tie-break cost. The spec lane's independent
  D-0007 judgment: genuine justification, not rationalization — FR-23
  explicitly sanctions `entered_by`. The demotion is in the prose: all
  four descriptions now lead with the count.
- **The gate caught the plan's own framing error.** The plan (and the
  Edge doc comment copied from it) listed Overrides among the
  declarative kinds; Overrides route through `resolve_call` and carry
  real counts when contested. The behavior shipped correctly — the DOCS
  overclaimed. Repaired in CLAUDE.md at the planning revision; the code
  doc comment is a recorded follow-up.

## What to carry forward

- **Plans can plant doc bugs.** The Overrides-as-declarative overclaim
  was written in the phase doc months before any code existed, then
  faithfully copied into a doc comment. When implementation contradicts
  a plan's framing, correct the plan's framing in the record — don't let
  the code's comments inherit it.
- **"Capture it where it exists or lose it forever" is a good test for
  cache-format changes.** The phase note pre-justified the only
  format-touching task in the plan with exactly that argument, and no
  lane contested the bump.
- **Verification-filter discipline, third iteration:** the reworded 9.1
  filters were right, but the rewording's RATIONALE overclaimed (the
  original `persist::` filter was actually functional). Corrections need
  the same primary-source check as the originals.
- **Open follow-ups** are in artifact 21: the Edge doc-comment
  alignment, the find_overrides description field list, the pre-existing
  watch-path Overrides non-resolution (its own fix, outside this
  phase's range), the count-staleness doc line, the impossible-state
  test fixtures, and a contested-N full-adapter snapshot. None gates
  phase 11.3, which this close unblocks (phases 4, 7, 8, 9 all
  complete).
