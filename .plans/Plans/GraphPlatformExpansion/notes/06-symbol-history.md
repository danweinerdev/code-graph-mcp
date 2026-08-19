# Phase 6 debrief — Symbol History

Closed 2026-08-19. Five tasks (three planned + two gate-driven fix tasks),
three gate cycles, final aligned review at
`reviews/18-symbol-history-final-review-2627a51-3ee9810.md`.

## What happened

- **6.1–6.3 landed cleanly in one session** (fingerprint hook, sidecar
  cache, transition walk + tool 25). The phase-5 follow-up this phase was
  told to pick up — the `revisions_touching` truncation signal — was
  consumed as designed: `RevisionWindow { commits, truncated }` landed as
  part of 6.3, and `history_truncated` rides the wire from day one.
- **The whitespace rule was refined at implementation.** The design's
  "collapse whitespace runs" would have failed AC-38's own canonical
  fixture (a line break after `(`); the shipped rule is token-aware — a
  separator survives only where two word tokens would merge. Recorded in
  the 6.1 notes as a deliberate deviation.
- **Cycle 1 found the phase's defining defect.** The walk parsed raw
  revision bytes without `preprocess`/`synthesize_symbols`, so every
  `[cpp].macro_*`-dependent symbol — the flagship UE configuration this
  repo exists to serve — silently reported an empty history AND cached
  false tombstones. The fix (6.4) had a second half that was easy to miss:
  the config's identity had to join the cache key, or the fix itself would
  have been poisoned by pre-fix tombstones and every future config edit
  would serve stale fingerprints.
- **Cycle 2 found a defect the cycle-1 fix CREATED.** Making `NotFound`
  the deterministic, permanently-cacheable absence signal (deletion →
  `removed`) turned the git provider's sloppy `find_blob → NotFound`
  mapping into a false-tombstone factory for partial clones and corrupt
  object stores. Fixed as 6.5: `Operation` for the blob arm, `NotFound`
  reserved for provable tree-entry absence.
- **Cycle 3 passed all four lanes** with two sub-material record
  inaccuracies repaired at the planning revision (CLAUDE.md
  `window_filled` gloss; the 6.5 Trap paragraph claiming early-arm
  `NotFound` lands in the skip channel when it is tombstone-bearing).

## What to carry forward

- **A fix that reclassifies an error can weaponize existing mappings.**
  M2 did not exist until M1's fix made `NotFound` cacheable. When a change
  promotes an error variant to a load-bearing, persisted semantic, audit
  every producer of that variant in the same change — the gate caught it
  one cycle late, at the cost of a full fix-and-regate round.
- **Cache-key completeness is part of any extraction-behavior fix.**
  Anything that changes what parses out of the same bytes (config, binary,
  future grammar bumps) must be in the fingerprint key. The
  `config_identity` serialization-hash pattern is cheap and covers future
  knobs automatically.
- **The intent-blind lane keeps earning its cost — fourth phase running.**
  Blind-spots found M1 (all plan-aware lanes passed the same diff) and M2
  (attacking the previous cycle's own fix). Do not skip it, and do point
  it at fixes, not just features.
- **Suite-level mutexes in async tests must be `tokio::sync::Mutex`.**
  Clippy's `await_holding_lock` rejects a std `MutexGuard` across awaits;
  starting there avoids a lint round-trip.
- **Windows fixture paths: pass provider inputs relative.** The 8.3
  short-form tempdir vs. canonical long-form root mismatch defeats the
  provider's ownership check; the vcs-git harness convention (relative
  paths against the bound root) exists for exactly this reason.
- **Open follow-ups** are recorded in artifact 18 (early-arm `NotFound`
  remap candidate, blame's `find_blob` arm inconsistency, skip-adjacent
  attribution marker, prefetch memory cap, NFR-10 blocking-pool test
  shape, shard-read batching, config over-invalidation, lifetime-lex
  fingerprint false positive). Phase 8 (per-language fingerprints) should
  pick up the lifetime-lex item and the dead in-walk `literal_insensitive`
  arm when it revives the mode.
