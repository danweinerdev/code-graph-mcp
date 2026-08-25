---
title: "Phase review: Accepted Technical Debt Closure"
type: review
status: resolved
created: 2026-08-25
updated: 2026-08-25
tags: [review]
related: ["Plans/GraphPlatformExpansion/12-Accepted-Technical-Debt-Closure.md"]
review_of: "Plans/GraphPlatformExpansion/12-Accepted-Technical-Debt-Closure.md"
rev: "0b41bbd32315c6638ad0c224e249ee0c25eb05d8..f9ca2caf78ea02a5b491f2120eacfd24e4848fe8"
review_scope: phase
frozen: true
verdict: Aligned
reviewed_planning_revision: "162cdd2871406bb2382ab7b5b983ed3e746bcc72"
review_mode: independent
lane_results:
  - lane: review_plan_drift
    result: PASS/Aligned
    reviewed_identity: "0b41bbd32315c6638ad0c224e249ee0c25eb05d8..f9ca2caf78ea02a5b491f2120eacfd24e4848fe8"
    evidence: "PASS/Aligned at frozen 0b41bbd..f9ca2ca after five cycles. Cycle 1 (endpoint b5fadbe) found the lane's single genuine drift: task 12.8's Notes omitted 32e5b9a, the commit carrying the production canonicalize_allowing_missing fix the short/long-form pins exercise (\"partially closes the task 12.8 short/long-form seam\" in its own message); repaired in f2d001d and verified against git show 32e5b9a --stat. Cycles 2-5 confirmed: the phase artifact blob stayed byte-identical from f2d001d through f9ca2ca (blob 67c3fc36bb76a80de0930f7ab6319048d97940ef); the 12.8 Notes at f9ca2ca still cite 32e5b9a/1b833e4/5725dc2 verbatim (line 469); every tail commit's file list matched its message claims one-to-one (f2d001d 16 files, 3c6f4fe 13, e38081e 2, 498f41a 2, f9ca2ca 7 — no extraneous or missing files); git log 0b41bbd..f9ca2ca -- .plans/Plans/GraphPlatformExpansion/reviews/ is empty across the entire range, upholding the frozen-review isolation invariant. Debt Coverage was cross-referenced item-by-item against reviews 15-22's own followups frontmatter with no gaps; all ten single-commit tasks' Revision checkpoints point at their fix commits; the four already-resolved exclusion commits are confirmed ancestors of range-start 0b41bbd."
  - lane: review_quality
    result: PASS/Aligned
    reviewed_identity: "0b41bbd32315c6638ad0c224e249ee0c25eb05d8..f9ca2caf78ea02a5b491f2120eacfd24e4848fe8"
    evidence: "PASS/Aligned at f9ca2ca after six cycles. Cycles 1-2 verified the F2 CallShape gate (default_scope_aware_resolve), all five per-language shape extractors, both resolve-loop integration points, canonicalize_allowing_missing, and the 12.8 Windows tests; re-executed the pinned suites natively (f2_ 5/6/3, receiver_shape_resolution 1, go_resolution 16, snapshot suites 60+33, vcs-git 24, path_normalization 3) plus workspace clippy -D warnings, all green; root-caused the find_path nodes_examined 5->2 snapshot delta to the fewer-heuristics-first frontier reordering (counting semantics unchanged). Cycle 3 proved the as_pattern_target arm dead (grammar alias of expression, compared by text) leaving except/with-as false-verified — repaired e38081e; cycle 4 proved four statement-level binding constructs (import self, from os import self, del self, type self = int) — repaired 498f41a; cycle 5 verified those repairs and this final cycle verified f9ca2ca's @staticmethod gate (fires before first-param matching, stacked-decorator safe), splat rejection (precedes identifier-inside extraction), and python_locally_bound_callable (terminating, panic-free, closure-capture covered) via an out-of-tree probe driving the production parse_file entry: 11/11 assertions including *self, **self, param/body/closure/lambda-bound callables, with @classmethod, clean self, unbound bare call, and module-level controls holding. One new observation — module-scope rebound callables stay Free — is the same unverifiable-callable-value family as KNOWN_ISSUES F2 accepted residual 1 and does not contradict the fix's precisely scoped claims. Minor notes dispositioned: caller_id_parent doc restored (f2d001d fix verified), Makefile dogfood echo hard-codes counts (accepted, gate itself honest), utf8-failure fallback direction cosmetic-unreachable."
  - lane: review_spec_compliance
    result: PASS/Aligned
    reviewed_identity: "0b41bbd32315c6638ad0c224e249ee0c25eb05d8..f9ca2caf78ea02a5b491f2120eacfd24e4848fe8"
    evidence: "Verified at f9ca2ca: grep for stale '= unambiguous' and 'unambiguous count' wording across CLAUDE.md, crates/code-graph-tools/src/server.rs, crates/code-graph-core/src/lib.rs, crates/code-graph-graph/src/callgraph.rs, crates/code-graph-graph/src/graph.rs, plugin/skills/code-graph-callgraph/SKILL.md, and the four tools_list snapshots returned zero hits; cargo test -p code-graph-tools --test snapshot_tools_list passed 33/33 and make plugin-sync-check passed. Earlier lane passes: at b5fadbe the four top-level tool descriptions in server.rs, the callgraph.rs D-0007 disposition comment, the core Confidence variant docs, the resolve_call trait contract, and SKILL.md still derived Heuristic from candidate count (repaired in f2d001d, snapshots regenerated); at f2d001d the four min_confidence argument-schema descriptions in server.rs plus internal comments in core lib.rs, callgraph.rs, and graph.rs still collapsed the axes (repaired in 3c6f4fe); at 498f41a one Medium remained, CLAUDE.md line 128 saying 1 = unambiguous (repaired in f9ca2ca). Standing checks: CACHE_VERSION is 13 in crates/code-graph-graph/src/persist/packed.rs with PackedEdge fields unchanged and Edge.shape parse-only in crates/code-graph-core/src/lib.rs; the daemon_serve DACL test validates the invoking user's SID with exactly one full-control allow ACE and makes no cross-account claim, matching D-0014; the per-task audit at f2d001d dispositioned tasks 12.1 through 12.11 tickable on their recorded checkpoints."
  - lane: review_blind_spots
    result: PASS/Aligned
    reviewed_identity: "0b41bbd32315c6638ad0c224e249ee0c25eb05d8..f9ca2caf78ea02a5b491f2120eacfd24e4848fe8"
    evidence: "Verified at f9ca2ca: cargo test -p code-graph-lang-python f2_ passed 5/5 including f2_staticmethod_variadic_and_local_callables_degrade (staticmethod-with-self, variadic *self, parameter-bound cb(), body-rebound cb2() all degrade to Receiver; unbound helper() stays Free; clean self.fine() stays SelfReceiver); cargo test -p code-graph-lang-java f2_ passed 3/3 including f2_method_references_classify_by_receiver (this::e SelfReceiver, obj::f and String::g Receiver); cargo test -p code-graph-lang f2_ passed 6/6 and receiver_shape_resolution 1/1. Earlier lane passes: at b5fadbe the lane proved the Python cls-shadowing false-verify, the Java static-import false Resolved/1, and the C++ qualified-parent spurious downgrade (repaired in f2d001d with caller_id_full_parent and the first-parameter gate); at 3c6f4fe it proved lambda-shadowing and walrus/assignment/for-target rebindings plus the dead as_pattern_target arm (repaired across e38081e); at 498f41a it proved staticmethod/variadic false-verifies and the parameter-bound bare-callable false Resolved/1 via scratch probes against the production parse_file entry (repaired in f9ca2ca), and confirmed via Rust compiler probes that let self = ... is rejected (E0424) so no Rust shadowing blocker exists. The independent quality-lane probe at f9ca2ca re-exercised the exact repairs with 11/11 assertions including **self, closure-bound, and lambda-bound callables. The remaining callable-value residual for Rust/C++/C#/Java and the conservative C++ nested-parent and implicit-this residuals are recorded in KNOWN_ISSUES F2's Accepted-residuals section at lines 205-231, satisfying the lane's scope-into-documented-debt disposition."
findings: []
followups: []
---

# Phase review: Accepted Technical Debt Closure

Reviewed `Plans/GraphPlatformExpansion/12-Accepted-Technical-Debt-Closure.md` at frozen identity `0b41bbd32315c6638ad0c224e249ee0c25eb05d8..f9ca2caf78ea02a5b491f2120eacfd24e4848fe8`.

## Findings

None.

## Resolution Log

None.
