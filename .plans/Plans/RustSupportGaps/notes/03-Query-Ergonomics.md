---
title: "Phase 3 Debrief: Query Ergonomics"
type: debrief
plan: "RustSupportGaps"
phase: 3
phase_title: "Query Ergonomics"
status: complete
created: 2026-05-22
updated: 2026-05-22
tags: [rust, parser, edges, namespace, response-shape, dogfood]
---

# Phase 3 Debrief: Query Ergonomics

Six commits on `rust-main` (three implementer + three quality-scanner follow-ups, ~50% follow-up rate matching Phases 1 and 2). All five gates green throughout (`lint`/`fmt-check`/`test --workspace`/`snapshot-clean`/`leak-scan`). Both targeted dogfooding issues (4: resolved-only callers/callees; 7: non-callable soft hint) resolved, plus a sibling-path bug in `diagram_call_graph` caught by the scanner and folded into 3.1's follow-up.

## Decisions Made

- **Scope expansion to `diagram_call_graph` (3.1 follow-up).** The scanner caught that `diagram_call_graph`'s BFS had the same `visited`-set pollution bug 3.1's design intent (Decision 7) was supposed to fix — but 3.1 only patched `Graph::bfs`. User approved scope expansion; both `visited.insert` sites in the diagram BFS (forward + reverse arms) got the same `is_resolved_node` guard. `mermaid_label` survives as documented defense-in-depth.
- **Non-callable kind set: 5 not 6 (3.2).** Design Decision 8 specified `{Struct, Enum, Trait, Typedef, Field, Interface}` but `SymbolKind` has no `Field` variant. Implementer correctly adapted to the as-shipped enum (5 kinds); design doc not amended but the discrepancy is captured in `is_non_callable_kind`'s doc comment with a `#[non_exhaustive]` future-maintainer warning.
- **Reusable `fn article(word)` helper (3.2 follow-up) over per-kind hardcoded strings.** Composes with future kinds; cleaner than coupling message construction to the kind set; the helper itself is six lines.
- **Two new DETECT regex patterns in `scripts/leak-scan.sh` (3.2 follow-up).** Added `[Pp]hase[-\s][0-9]+` (covers hyphenated) and ` \([0-9]+\.[0-9]+\)` (covers parenthesized). The leading space on the parenthesized form is load-bearing — it prevents `static_cast<int>(3.14)` false positives.
- **Sweep-in-commit for 13 pre-existing leaks (3.2 follow-up).** When the tighter regex surfaced 13 pre-existing plan-pointer leaks across the workspace, the implementer correctly judged them as small mechanical behavioral-rot and swept them all in the same commit per the user's standing "fix all" pattern, rather than asking for permission per leak.
- **Consolidated CLAUDE.md bullet in 3.3.** Three Response-shapes contracts (resolved-only filter, soft-hint, `CallChain` field semantics) written as one consolidated bullet rather than three separate ones. Trade-off: longer single bullet for thematic cohesion. Scanner accepted the structure.
- **`CallChain` entry written fresh in 3.3 (not 2.4 as planned).** The plan-doc said 2.4 would write it; `grep` confirmed 2.4 didn't. Task 3.3's brief explicitly anticipated this case ("If 2.4 left this entry partial OR missing, complete it now"). Smooth recovery; no plan-level damage.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| `get_callers`/`get_callees` return only resolved project symbols | Met | Filter at `Graph::bfs` BEFORE `visited.insert` via shared `Graph::is_resolved_node` predicate. Cross-language uniform. |
| Filter parity with `generate_diagram` | Met (and extended) | 3.1 follow-up applied the same predicate to `diagram_call_graph`'s BFS expansion at both forward + reverse arms — closed a sibling-path gap. |
| `total`/`next_offset` self-consistent on the filtered set | Met | Handler computes `total = chains.len()` post-graph-call; filtering inside the graph makes pagination naturally correct. |
| Callable with no resolved hops → empty envelope (not error) | Met | `callers_known_symbol_with_no_callers_returns_empty_envelope` + new `get_callers_on_function_with_only_unresolved_callees_still_returns_empty_envelope` both green. |
| Non-callable target → actionable soft-hint success | Met | 5 end-to-end advisory tests (Struct, Enum, Trait, Typedef, Interface); each verifies `is_error: false`, kind name in text, and routed alternative tool. |
| CLAUDE.md Response-shapes coverage (filter + soft-hint + CallChain) | Met | Single consolidated bullet at `CLAUDE.md:95`. Field semantics, trichotomy, kind set, alt-tool routing, and BFS-layer filter all documented. |
| Structural gate + cold-read sweep | Met | All 5 gates green; no sibling-section contradictions. |

## Deviations

- **`SymbolKind::Field` doesn't exist in the enum.** Design Decision 8 listed `Field` in the non-callable set; the actual enum has 5 callable + 5 non-callable variants (no `Field`). Implementer adapted to the as-shipped code. The design-vs-code mismatch is documented in `is_non_callable_kind`'s doc comment as a future-maintainer note.
- **`diagram_call_graph` filter site was not in the 3.1 plan but in the 3.1 design intent.** Phase 3's plan scoped 3.1 to `Graph::callers`/`callees`. The scanner identified `diagram_call_graph`'s BFS as a sibling code path with the same residual bug the design's "parity with `generate_diagram`" language implied should be fixed. Scope expansion in 3.1 follow-up. Cost: ~15 extra LoC; gain: closed a real `visited`-pollution defect in the diagram tool.
- **Leak-scan DETECT regex tightened a third time.** 1.3 widened the file glob to `Cargo.toml`; 1.6 added `CLAUDE.md`; 3.2 tightened the regex itself to catch `Phase-N` (hyphenated) and `(N.N)` (parenthesized) forms. Not anticipated by the original plan; emergent from the 3.2 scanner finding two such leaks in the same commit.
- **2.4 didn't actually write the CallChain Response-shapes entry the plan said it would.** The forward-reference chain was: design listed the edit → 2.4's task spec said "write CallChain entry" → 4.2's task spec said "Phase 4.2 only verifies it landed, written in 3.3" — but 2.4's implementer didn't write it, and the 2.4 quality scanner didn't catch the omission. 3.3's audit caught it; 3.3 wrote it fresh.

## Risks & Issues Encountered

- **`visited`-set pollution in `diagram_call_graph` BFS (3.1 → 3.1 follow-up).** The scanner's FOCUS_LIST asked for sibling-code-path checking and identified the bug: `Graph::bfs` was filtered, `diagram_call_graph` was not. A `generate_diagram` call on two resolved arms both touching the same unresolved token plus a shared resolved descendant would have suppressed the second arm. Resolution: apply `is_resolved_node` filter to both `visited.insert` sites in `diagram_call_graph`'s BFS. Verified load-bearing by manual revert (regression test fails with the expected `D → C` drop).

- **Leak-scan regex evasion (3.2 → 3.2 follow-up).** The scanner found two plan-pointer leaks in the same commit that introduced them: `Phase-3.1` (hyphenated, evading `[Pp]hase [0-9]+`) and `contract (3.1)` (parenthesized, no DETECT pattern). Resolution: widen the regex with two new patterns; sweep the 13 pre-existing leaks the wider scan surfaced (spread across `crates/code-graph-graph/`, `crates/code-graph-lang/`, `crates/code-graph-lang-{go,python,rust}/`, `crates/code-graph-tools/src/handlers/`, and 4 test files).

- **"a enum"/"a interface" grammar bug in agent-facing advisory text (3.2 → 3.2 follow-up).** The format string `"{basename} is a {kind_name}"` used fixed article `"a"`, producing `"Color is a enum"` and `"X is a interface"`. The Enum test acknowledged the bug with a lenient `"a enum"` or `"an enum"` either-or assertion, confirming the bug was live. Resolution: add `fn article(word) -> &'static str` helper; tighten the Enum test assertion.

- **Missing `CallChain` Response-shapes entry in CLAUDE.md (3.3 cold-read audit).** Plan said 2.4 would write it; 2.4 didn't. The plan-doc forward-reference created the illusion that the entry existed somewhere. Resolution: 3.3 wrote it fresh, bundled with the other Response-shapes Phase-3 content into one consolidated bullet.

- **Doc inaccuracies in the new 3.3 CLAUDE.md bullet (3.3 → 3.3 follow-up).** The scanner caught (a) alt-tool list claimed structural kinds route to `get_class_hierarchy` only when the actual advisory recommends both `get_class_hierarchy` AND `get_symbol_detail`; (b) the resolved-only filter was attributed to `mermaid_label` (post-BFS defense-in-depth) when the actual primary gate is `is_resolved_node` (BFS-expansion). Resolution: two single-phrase doc edits in a single follow-up.

## Lessons Learned

- **Per-task quality-scanner sustained its hit rate.** Phase 3: 3.1 → 1 Major + 1 Minor; 3.2 → 4 Minors; 3.3 → 2 Minors. Total: 1 Major (the orphaned diagram filter, identified BEFORE shipping) and 8 Minors across 3 tasks. The "intent-blind" framing keeps catching real defects the plan-aware implementer misses.

- **The "tighten + sweep" cycle is a productive convention for hygiene gates.** Three rounds of leak-scan widening (1.3, 1.6, 3.2) each surfaced real pre-existing rot. The widenings were emergent (none was in the original plan), and the per-commit sweeps prevented the rot from accumulating between phases. Codebase plan-pointer hygiene is materially higher post-Phase-3 than at Phase-1 start.

- **Plan-doc forward-references need active verification.** "Earlier phase wrote X" is unverified by default. 3.3 caught one such gap (the missing CallChain entry). A debrief-time or phase-readiness `grep` check on forward-referenced symbols/files would catch these earlier. The same pattern caught the design's `Field`-not-in-`SymbolKind` mismatch — but that one survived because nobody actually checked the enum's variants before the implementer hit it.

- **Sibling-path bugs are a real category caught by explicit FOCUS_LIST entries.** The 3.1 scanner's prompt asked "find sibling code paths with the same shape and verify the fix applies there too" — and it found the diagram filter gap. Without that explicit prompt, the standard "review for correctness" framing would have missed it. The scanner's quality is bounded by the orchestrator's FOCUS_LIST authoring effort.

- **Scope expansion at scanner-review time has worked consistently well across phases.** 3.1 (diagram filter), 3.2 (leak-scan regex + 13-leak sweep), and earlier 1.6 (CLAUDE.md leak-scan widening + 2 leaks) all expanded scope at review-time with the user's "fix all" choice. Each expansion landed cleanly with no follow-on defects. The pattern suggests the user's standing "fix all" preference is not over-correcting — the expansions reliably uncover real bugs that would have shipped silently otherwise.

- **`#[non_exhaustive]` + `matches!` is a documented footgun.** Future non-callable `SymbolKind` variants will silently degrade the soft-hint behavior (default to empty-envelope) with no compile-time signal. `is_non_callable_kind`'s doc comment instructs maintainers to extend the predicate, but there's no compiler enforcement.

- **Cross-language uniformity assertions lack cross-language tests.** Phase 3's filter is uniform across all 6 plugins; no test pins this. A future Rust-specific shortcut path or a C++ implementation that diverges would not be caught. The risk is low (the predicate is one shared `nodes.contains_key` check), but worth a single cross-language regression test as defense.

- **The 4-option AskUserQuestion pattern after every scanner has been load-bearing for compound improvements.** Across Phases 1, 2, 3 the user chose "Fix all (Recommended)" almost every time. Each round produced 2-5 fixes; cumulative effect across 18 scanner rounds is the difference between shipping a correct Phase-3 and shipping one with the orphaned diagram filter + wrong-grammar advisory + missing CallChain entry + leak-scan evasion forms.

## Impact on Subsequent Phases

- **Phase 4.2 (CLAUDE.md cold-read sweep) is lighter than planned.** The `get_callers`/`get_callees` Response-shapes content was written in 3.3 (consolidated bullet at line 95). 4.2 only verifies it; doesn't write it. The `CallChain` entry also lives there.

- **Phase 4.1 (`server.rs` tool-description sweep) should mention the trichotomy.** The `get_callers`/`get_callees` description strings must note: (a) results are resolved-only (parity with `generate_diagram`); (b) non-callable target → soft-hint success (not error, not empty envelope); (c) `CallChain.file` vs `symbol_id` semantics at depth ≥ 2. These are all user-visible contracts agents pattern-match on.

- **`scripts/leak-scan.sh` DETECT regex is tighter for Phase 4.** Any plan-pointer leaks in Phase 4's CLAUDE.md additions or `server.rs` description-string edits will be caught immediately by the new patterns (`Phase-N` and ` (N.N)`). Phase 4 implementers should describe behavior, not provenance.

- **`Graph::is_resolved_node` is now documented public surface.** Phase 4 doesn't need to touch it but should reference it correctly if any new CLAUDE.md edit talks about filter behavior.

- **`SymbolKind::Field` is documented as not-in-enum.** If Phase 4 (or a future plan) adds a non-callable variant, the `is_non_callable_kind` doc comment instructs to extend the predicate explicitly.

- **Cumulative dogfooding-issue resolution after Phases 1+2+3:** issues 1, 2, 3, 4, 5, 6, 7 done. Issue 8 (CallChain semantics) was written in 3.3's CLAUDE.md bullet — Phase 4 only needs to verify it and write the matching `server.rs` tool-description string. Issue 9 (`suggestions` docs) remains Phase 4's main outstanding item.

## Skill Opportunities

### Leak-scan tightening + sweep convention

- **What you did repeatedly:** Three times now in this plan, the leak-scan scope was widened — 1.3 added `Cargo.toml`; 1.6 added `CLAUDE.md`; 3.2 tightened the DETECT regex. Each widening surfaced pre-existing plan-pointer rot (1.3: 1 leak; 1.6: 2 CppMacroStrip pointers; 3.2: 13 leaks across 9 files). The widenings were emergent — none was in the original plan — and the standard response was "fix all surfaced leaks in the same commit as the widening." Without that convention, the rot would have accumulated across phases.

- **Where it belongs:** Either (a) a formal note in `scripts/leak-scan.sh`'s header comment documenting the "if you widen, sweep in commit" rule, or (b) a `make leak-scan-audit` target that runs leak-scan with a maximally-permissive DETECT pattern (catches the next round of evasions) and surfaces a candidate-rewrite list for review. Option (a) is documentation; option (b) is a periodic check the user can run between phases to preempt the "scanner catches an evasion mid-task" surprise.

- **Why a skill:** Three widenings in one plan suggests this is going to happen again. Codifying the convention prevents the "widen but defer sweep" failure mode (which would accumulate rot in subsequent phases). The cost is one comment block or one new Make target; the gain is hygiene predictability.

- **Rough shape (option b):** `make leak-scan-audit` runs leak-scan with extra DETECT patterns covering common evasion forms (case variation, additional punctuation around numbers, e.g. `T.N`/`T:N`/`#N.N`/`v?N\.N`). Output is a candidate-rewrite list grouped by file. The user (or implementer) reviews and either rewrites in-place, or tightens the production DETECT regex to actually catch the new forms. Invoke at the start of every `/implement` phase, or as a manual sanity check after a long doc-touching commit.

### Plan-doc cross-reference verification

- **What you did repeatedly:** Phase-doc forward-references like "Phase M.N writes X" or "Phase 4.2 only verifies X (written in 3.3)" are unverified by default. 3.3's cold-read sweep caught one real gap: 2.4 was supposed to write the `CallChain` Response-shapes entry but didn't, and the 2.4 quality scanner didn't catch the omission. The same pattern caught `SymbolKind::Field` not existing in the enum despite Decision 8 listing it. Both gaps survived for a phase or more before audit; both required ad-hoc grep work to detect.

- **Where it belongs:** A new step in `/implement`'s "Verify Task Readiness" (step 4 in the skill spec): when a task's body or notes reference symbols/files/entries said to exist or to have been written, run `grep` to confirm they exist before launching the wave. Specifically for phase-doc cross-references: when phase N's task says "phase M wrote X in CLAUDE.md," `grep -F "X" CLAUDE.md` must return at least one match. If not, flag to the user before proceeding.

- **Why a skill:** The forward-reference pattern is structurally fragile — it relies on every prior phase faithfully delivering on its plan-doc promises, with no enforcement. Three plan-doc gaps caught in Phases 1-3 (the design's `index_directory` cache-merge mis-statement, the missing `CallChain` entry, the non-existent `SymbolKind::Field`) suggest the rate is non-trivial. A grep check at phase-readiness time is cheap and would catch the same class of gap.

- **Rough shape:** Extend `/implement`'s step-4 readiness audit:
  ```
  for each task in this phase:
    extract phase-references like "Phase M.N", "M.N wrote/writes X", "M.N verifies X"
    for each referenced X (file path, symbol name, CLAUDE.md section):
      if X is a file path: confirm with `test -f X`
      if X is a symbol: grep the codebase
      if X is a CLAUDE.md section: grep CLAUDE.md
    if any reference is unverified: flag + ask user (similar to the verification-field audit)
  ```
  Output integrates with the existing readiness audit's user-facing question ("Verification issues" + "Forward references"); just adds a third "Unverified cross-references" category.

### `mk_crate_layout` test helper (reinforced from Phase 1)

- **What you did repeatedly:** Phase 1 (tasks 1.3 and 1.4) and Phase 3 (tasks 3.1, 3.2, 3.3) and Phase 2 (tasks 2.2, 2.3) all built tempfile-based Rust crate fixtures from scratch: `TempDir::new()`, write a `Cargo.toml`, write `src/lib.rs` + variants, construct file paths, build `Vec<FileGraph>`. ~30-50 lines of boilerplate per test that needs a real Rust crate on disk. Phase 1's debrief flagged this as a skill opportunity; Phase 2 + Phase 3 confirm the pattern is plan-wide.

- **Where it belongs:** A shared `pub(crate) mod test_crate_layout` test-helper module in `crates/code-graph-tools` (analogous to the existing `test_recording_plugin` module, established in Phase 1's 1.1 follow-up). Same shape as proposed in Phase 1's debrief: `mk_crate_layout(crate_name, files: &[(&str, &str)]) -> CrateLayout` with fields for `root: TempDir`, `crate_name: String`, `graphs: Vec<FileGraph>`, `file_index: FileIndex`. Plus `mk_crateless_layout(files: &[(&str, &str)])` for the no-Cargo.toml fallback path.

- **Why a skill:** Eight tasks across three phases reinvented this fixture pattern. Phase 4 is mostly doc work and likely won't need it, but any future plan touching Rust parser or graph tests will pay the same cost. The helper's specific shape is well-established now (tempfile, Cargo.toml + `src/` tree, `analyze_codebase`-ready output) — it's effectively a known good API that just hasn't been extracted.

- **Rough shape:** Same as Phase 1's debrief specification:
  ```rust
  pub(crate) struct CrateLayout {
      pub root: TempDir,             // owns the on-disk fixture
      pub crate_name: String,        // pre-normalized; "-" already → "_"
      pub graphs: Vec<FileGraph>,    // post-parse_file, pre-post_index
      pub file_index: FileIndex,     // built over `graphs`
  }
  pub(crate) fn mk_crate_layout(
      crate_name: &str,
      files: &[(&str, &str)],       // (src-relative path, file content)
  ) -> CrateLayout;
  pub(crate) fn mk_crateless_layout(
      files: &[(&str, &str)],
  ) -> CrateLayout;
  ```
  Phase 3 specifically would have saved boilerplate in 3.1's depth-2 pollution tests and 3.2's non-callable kind fixtures. Worth elevating priority: implement as a standalone interlude commit before Phase 4 starts (Phase 4 won't use it, but the next plan that touches Rust tests will).
