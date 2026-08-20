---
title: "Phase review: Resolver Candidate Count"
type: review
status: resolved
created: 2026-08-20
updated: 2026-08-20
tags: [review, resolver, candidate-count, confidence, cache, phase-9, final]
related: ["Plans/GraphPlatformExpansion/09-Resolver-Candidate-Count.md"]
review_of: "Plans/GraphPlatformExpansion/09-Resolver-Candidate-Count.md"
rev: "f4b177931ef3ac3a87a09c7d0c331ca887088a1d..824cfa2f74b733f824a7dbfec8627bbd01cab15a"
review_scope: phase
frozen: true
verdict: Aligned
reviewed_planning_revision: "030057680a943b3ef1f70dc4fdbecb43fd20ae6f"
review_mode: independent
lane_results:
  - lane: review_plan_drift
    result: PASS/Aligned
    reviewed_identity: "f4b177931ef3ac3a87a09c7d0c331ca887088a1d..824cfa2f74b733f824a7dbfec8627bbd01cab15a"
    evidence: "All seven range commits accounted for; every checked subtask maps to code (the declarative-edge decision recorded in Notes and verifiable at the Rust override; the 9.3 inventory independently re-verified complete — exactly PathHop.entered_by and heuristic_hops carry the one-bit tag on the wire); evidence blocks conform with file counts 20/12/7 and test counts reconciling against source (29 persist on the Windows host, 65 lang, 4+59+33); the CACHE_VERSION bump is implemented and documented against the phase trap. One minor (the 9.1 verification-rewording rationale was overbroad — the original persist:: filter WAS functional; only the lang half was drift) — repaired at the planning revision, plus the stale updated date."
  - lane: review_quality
    result: PASS/Aligned
    reviewed_identity: "f4b177931ef3ac3a87a09c7d0c331ca887088a1d..824cfa2f74b733f824a7dbfec8627bbd01cab15a"
    evidence: "Count capture is correct (per-language keyed lookup, len captured before scoring, sane u32 saturation); adj/radj cannot diverge (one push site copies into both, pinned by the non-default round-trip plus the reverse-adjacency integration test); the v11 history entry's claim is accurate against the load path's header->endian->version->bytecheck ordering; no production path reads EdgeEntry from JSON, so the serde default is fixture-only as documented; the three wire conventions are each surface-justified; candidate_count.rs is hermetic and robust to either scope-rule pick; all four description claims spot-check true. Minors: the Edge.candidates doc overclaims Overrides as declarative (behavior is right — real N surfaces; the doc echoes the plan's framing; CLAUDE.md repaired at the planning revision, the code doc-comment filed as follow-up); the suffix-disambiguation example was unobservable on the wire as worded (repaired); fixtures pairing Heuristic with candidates:1 encode an impossible state (nit, filed)."
  - lane: review_spec_compliance
    result: PASS/Aligned
    reviewed_identity: "f4b177931ef3ac3a87a09c7d0c331ca887088a1d..824cfa2f74b733f824a7dbfec8627bbd01cab15a"
    evidence: "FR-48/AC-57 SATISFIED on all four named surfaces with the real 2-candidate contest driven end to end; cache round-trip + pre-bump re-index SATISFIED (non-default 3 in both adjacency directions; version-mismatch test; trap rejected in field docs); additive shapes SATISFIED (all 12 snapshot diffs insertion-only; file=/class= diagram snapshots byte-identical, confirming the skip_serializing_if boundary); NFR-11 SATISFIED with the min_confidence relationship stated operationally and verified exactly true against the BFS filter and the sole resolve_call implementation. Independent D-0007 judgment: the demote-and-document disposition is genuine justification, not rationalization — FR-23's spec text explicitly sanctions entered_by (verify the weak link without a second round-trip), the independent-axes argument is grounded in real code, and the demotion is real (all four descriptions lead with candidates). One minor: find_overrides' description enumerates a now-incomplete field list — filed as follow-up."
  - lane: review_blind_spots
    result: PASS/Aligned
    reviewed_identity: "f4b177931ef3ac3a87a09c7d0c331ca887088a1d..824cfa2f74b733f824a7dbfec8627bbd01cab15a"
    evidence: "Nine attack surfaces probed; nothing material. Cross-scope and watch-path count staleness inherit confidence's accepted mechanism byte-for-byte (doc-line follow-up filed); the unresolved-Overrides radj path CAN surface a provisional candidates:1 for a bare-token query (fringe, pre-existing ordering, indexer comment slightly overclaims — filed); Inherits' provisional 1 never surfaces on any wire shape (acceptable boundary); no hidden coupling to hashing/stats/status; CLI parity holds by construction with the human renderer absorbing the new column; the contested-N path is pinned at the core layer with literal JSON key assertions (a full-adapter contested snapshot filed as a nice-to-have); u32::MAX saturation is inert (no code compares the count); the include-count invisibility wording repaired at the planning revision. One adjacent pre-existing defect OUTSIDE the range: watch.rs never resolves Overrides edges — filed for the backlog."
findings: []
followups:
  - "Edge.candidates' doc comment (code-graph-core) repeats the plan's Overrides-as-declarative overclaim; align it with the corrected CLAUDE.md wording (Overrides route through resolve_call and carry real counts when contested)."
  - "find_overrides' tool description enumerates {symbol_id, file, line, depth} while the wire now emits candidates on every row; rebaseline the description + tools-list snapshot on the next description touch."
  - "Watch-path Overrides non-resolution (pre-existing, outside the frozen range): watch.rs's edge match has no Overrides arm, so a watch-reindexed file's override edges keep bare `to` + provisional Resolved/1 until the next analyze — find_overrides misses them. Track as its own fix."
  - "Cross-scope/watch count staleness mirrors confidence staleness (cached edges never re-resolve, so a later scoped analyze adding a same-named definition leaves old counts underreporting N); one sentence in CLAUDE.md's cross-scope bullet would disclose both."
  - "Unresolved-Overrides radj rows: find_overrides on a bare-token key returns rows stamped candidates:1 for edges the resolver found zero candidates for (requires deliberately passing the bare form); the indexer comment 'unresolved edges don't surface' overclaims for this path."
  - "Test fixtures pairing Confidence::Heuristic with candidates:1 encode a state the production resolver cannot emit (handlers/query.rs, callgraph.rs test helper); harmless for what they test, worth normalizing to 2."
  - "No snapshot pins a contested N ≥ 2 through the full rmcp adapter path (candidate_count.rs asserts the literal JSON keys at the core layer, which catches renames); a single contested-fixture snapshot would close the residual handler-layer exposure."
  - "The 'drops exactly the candidates ≥ 2' description claim rests on the invariant that no plugin overrides resolve_call; CLAUDE.md hedges with 'for call edges TODAY' — revisit if a per-language override ever lands."
---

# Phase review: Resolver Candidate Count

Reviewed `Plans/GraphPlatformExpansion/09-Resolver-Candidate-Count.md` at
frozen identity `f4b1779..824cfa2` (planning content at `0300576`).
**Review mode:** independent lanes — four parallel, non-inheriting contexts
with isolated inputs, dispatched once.

## Cycle history

- **Cycle 1** (this artifact): all four lanes PASS/Aligned on the first
  cycle. Sub-material record inaccuracies repaired at the planning
  revision `0300576`: CLAUDE.md's Resolved bullet no longer lists
  Overrides as declarative (they route through `resolve_call` and carry
  real counts when contested); the suffix-disambiguation justification
  now names the include RESOLVER CONTRACT and states the count dies at
  merge (no wire surface exhibits `Resolved`+count-2 today); the 9.1
  verification-rewording rationale no longer overclaims (the original
  `persist::` filter was functional — only the lang half was drift).
  Residual observations recorded above as follow-ups; none is material
  to the phase deliverable.

## Verification

- `make verify` at the endpoint: PASS (`exit 0`) — clippy `-D warnings`,
  rustfmt, full workspace tests (natively on Windows), pending-snapshot
  check, plugin-mirror sync.
- `cargo test -p code-graph-graph persist` (29 on the Windows host):
  including `round_trip_preserves_candidate_count` (non-default 3, both
  adjacency directions) and `load_version_mismatch_returns_false`
  (pre-v11 silent re-index).
- `cargo test -p code-graph-lang` (65): resolver suites asserting count
  1 for sole-candidate picks, 2 for contested picks, and 2 with
  `Resolved` for suffix-disambiguated includes.
- `cargo test -p code-graph-tools --test candidate_count` (4): AC-57 end
  to end on one fixture — callees distinguish 1 from 2 in a single
  response, callers carry the contested count through reverse adjacency,
  find_path hops mirror `entered_by`'s null-for-source convention,
  diagram edges report the real N.
- Snapshot suites: 8 response + 4 tools-list rebaselines, each diff
  reviewed individually — insertion-only.

## Resolution Log

No open findings; the follow-ups above are accepted, recorded
non-blockers.
