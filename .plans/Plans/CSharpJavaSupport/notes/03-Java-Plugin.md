---
title: "Phase 3 Debrief: Java Plugin"
type: debrief
plan: "CSharpJavaSupport"
phase: 3
phase_title: "Java Plugin"
status: complete
created: 2026-05-11
---

# Phase 3 Debrief: Java Plugin

Seven tasks (3.1–3.7), 10 commits, fewer review-fix-cycle interventions than Phase 2 because lessons carry-forwarded cleanly. Final crate state: 79 inline tests + 22 corpus tests = 101 tests, all four structural gates green, zero `#[allow(dead_code)]` remaining, zero `unsafe` blocks, dogfood baseline pinned at 4598 symbols against `apache/commons-lang` `rel/commons-lang-3.20.0`.

## Decisions Made

- **tree-sitter-java pinned at `=0.23.5`.** Compatibility with workspace `tree-sitter` core 0.26 confirmed during Phase 1's combined probe. No version conflicts.
- **`record_declaration` in the query from day one.** The C# 2.2 records-leak bug (orphan methods inside records) was carry-forward'd into the 3.2 brief. The Java implementer added `record_declaration` to BOTH the definition query AND `enclosing_named_type_name` upfront. Anti-regression test pinned this case. No silent-skip class of bug recurred.
- **Body-presence discriminator for Decision 11 instead of modifier check.** The 3.2 brief said "match `default` OR `static` modifier inside `interface_declaration`." The implementer chose body-presence instead — same as the C# 2.2 pattern — which cleanly subsumes `default`, `static`, AND Java-9+ `private` interface methods. The brief was strictly less general; the implementer correctly recognized the better rule. Java-9+ private case was missing from the 3.2 brief's test list; added in the 3.2 fix-up commit `b6f45ab` after the quality scan flagged the coverage gap.
- **Two helpers for the enclosing-named-type walk.** `enclosing_named_type_kind` (returns the kind name string for method-vs-function dispatch) + `enclosing_named_type_name` (returns the parent string). Split rather than collapsed to avoid String allocation on the common interface path. The split is reasonable; cross-plugin grep is fine because the names are descriptive.
- **Anonymous-class transparency for the parent walk.** Per Decision 4, methods inside `new Runnable() { void run() {...} }` take the enclosing named entity's parent as parent — NOT a synthetic `Anonymous$1`. The walk skips past `object_creation_expression` boundaries. Same transparency applies to enum-constant boundaries per Decision 12. Documented in both helpers.
- **`Type::new` method references documented as a known limitation.** Java method references like `String::length` (identifier RHS) extract cleanly via `(method_reference "::" (identifier) @call.name)`. Constructor references like `Type::new` have a `new` keyword token as the RHS — awkward to handle and rare in practice. Documented as a limitation; no-edge test pinned.
- **No Java analog to C#'s `nameof` filter.** The 3.3 probe confirmed: cast/instanceof/synchronized/array-creation/annotations all parse as their own dedicated node kinds. Four explicit no-edge tests (`cast_expression_produces_no_call_edge`, `instanceof_and_synchronized_produce_no_call_edges`, `array_creation_produces_no_call_edge`, `annotations_produce_no_call_edges`) pin this.
- **`this(...)` and `super(...)` are legitimate constructor-chain calls.** Java's `explicit_constructor_invocation` parses with the `this`/`super` keyword as the callee. The extractor records `to = "this"` or `to = "super"` per the syntactic contract. Pinned by two dedicated tests.
- **Single-segment imports defensive coverage.** `import Foo;` (no dotted path, just an identifier) is grammatically valid but rare. The 3.4 implementer's grammar probe confirmed all three named-child kinds (`identifier`, `scoped_identifier`, `asterisk`) and added defensive coverage. Carry-forward of the C# 2.4 `alias_qualified_name` silent-skip lesson.
- **5 inheritance patterns reflect Java's grammar asymmetry.** Unlike C#'s uniform `base_list`, Java uses three different inheritance node shapes: `superclass` (single-child, NO `type_list` wrapper) on class; `super_interfaces` (with `type_list`) on class/record/enum; `extends_interfaces` (with `type_list`, UNNAMED-field child) on interface. The query has 5 patterns total. Documented per-form in `queries.rs`.
- **Generic-constraint behavior diverges from C#.** Java's `class Foo<T extends Comparable<T>> extends Bar<T>` produces `from = "Foo<T extends Comparable<T>>"` — the constraint rides along inside `type_parameters`. C#'s `where T : Comparable<T>` clause is a sibling node, so C#'s `from` is the cleaner `"Foo<T>"`. Both honor Decision 9 ("preserved verbatim"); Java's verbatim is just verbose. Documented in the 3.5 fix-up commit `acfd78c` (initial commit had a self-contradicting test comment that asserted the C# behavior).
- **Generic-class hierarchy lookup gap accepted as documented limitation** (same as C# 2.5, same as Rust). `Symbol.name` is bare `"Foo"`; `Inherits.from` is `"Foo<T>"`. `Graph::class_hierarchy` cannot bridge them. Phase 4.4's CLAUDE.md `## Java Parser Limitations` section should document this.
- **Sealed types' `permits` clause IGNORED.** Per Decision 6. The `permits:` field is a sibling of `extends_interfaces`/`super_interfaces`; the query doesn't target it. Pinned by `sealed_interface_permits_clause_is_ignored` test.
- **Records CANNOT extend** (Java grammar treats this as a syntax error). `extract_inheritance` handles ERROR nodes gracefully via `has_error()` check (mirrors C#).
- **Broken.java recovered-symbol count: 5** (measured, not pre-guessed). Tree-sitter-java 0.23.5 recovers MORE aggressively than tree-sitter-c-sharp 0.23.5 — Java extracts the malformed `bar` method despite the syntax error; C# drops the equivalent malformed `Bar(`. The 5 names (`Broken`, `bar`, `good`, `AlsoGood`, `run`) are now all pinned in `broken_file_recovers_around_error_nodes_without_panic` (added `bar` by name in the post-3.6 fix-up commit `4925d1b` after the quality scan flagged it).
- **Watch-test discriminator: anonymous-class lifecycle (Decision 4).** Java has no partial-class analog (the C# discriminator), so the Decision 4 anonymous-class behavior takes its place. Test pins both the `run` Method symbol prune AND the resolved Calls-edge prune when the file is removed. Sentinel-before-discriminator pattern present.
- **commons-lang pinned at `rel/commons-lang-3.20.0` (commit `598dfc1`).** Apache's tag-naming convention changed at v3.10 to `rel/commons-lang-X.Y.Z` (the original `LANG_3_X_X` tags only exist up to 3.8.1). Documented in `.gitmodules` and CLAUDE.md table. Walked subdirectory: `src/main/java` (matches design suggestion). Baseline: 4598 symbols ±10%.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| `crates/code-graph-lang-java/` exists with canonical scaffold | Met | Same shape as `crates/code-graph-lang-python/` |
| All four extractors implemented | Met | 6 def patterns, 7 call patterns, 1 import pattern, 5 inheritance patterns |
| Inline tests for all 4 extractors and all 6 Decisions | Met | 79 inline tests covering Decisions 2, 4, 6, 9, 11, 12 plus body-presence refinement |
| `testdata/java/` + corpus regression locks aggregate counts | Met | Pinned counts; Broken.java run-and-recorded at 5; all 5 names pinned by identity |
| `crates/code-graph-tools/tests/watch_java_reindex.rs` covers Inherits + Calls pruning + Java-specific discriminator (Decision 4) | Met | 3 tests; sentinel-before-discriminator pattern |
| `external/commons-lang` submodule pinned; baseline recorded; dogfood test auto-skips | Met | `rel/commons-lang-3.20.0` commit `598dfc1`; 4598 symbols ±10% |
| Phase 3 debrief written | Met | This document |
| All structural gates pass | Met | 101 tests, zero clippy warnings, fmt clean, audit clean, snapshot-clean |

## Deviations

- **Body-presence discriminator instead of modifier check for Decision 11.** The 3.2 brief said "match `default` OR `static` modifier"; the implementer chose body-presence instead. Body-presence is strictly more general — covers `default`, `static`, AND Java-9+ `private` interface methods cleanly. The deviation was an improvement; the missing Java-9+ private test case was added in the 3.2 fix-up.
- **Records-leak bug did NOT recur.** Java 3.2's implementer applied the C# 2.2 lesson preemptively — added `record_declaration` to BOTH the query AND `enclosing_named_type_name`. The bug class was prevented at implementation time, not caught at review time. **This is the validated carry-forward pattern working.**
- **Type::new method references documented as limitation, not implemented.** The 3.3 brief left room for this judgment call ("If the shape is awkward, document as a known limitation"). The implementer correctly documented and pinned a no-edge test.
- **Apache commons-lang tag-naming convention** — pinned `rel/commons-lang-3.20.0` rather than `LANG_3_X_X` (which doesn't exist past 3.8.1). Documented in `.gitmodules`/baseline/CLAUDE.md table.
- **Watch-test fixture uses top-level package-private Java classes** (multiple classes per file) instead of public-nested-class structure. Java's "one public top-level class per file" rule makes the flat top-level form the cleaner mirror of C#'s symbol-ID shape.
- **Two commits for the 3.6 dogfood**: initial `39ed511` + pin-fix `ad3bacd`. `git submodule add` records the default-branch HEAD; the subsequent `git checkout <tag>` doesn't auto-restage. The two-commit shape is correct per the Git Safety Protocol (no `--amend` on existing commits).

## Risks & Issues Encountered

- **Worktree isolation broken from Wave 2 onward (same as Phase 2).** Sandbox-mode issue blocked `git reset --hard rust-main` inside agent worktrees. All Java tasks ran sequentially against the orchestrator's main tree. Resolution: dispatch sequentially, accept the calendar-time cost. No data loss; no rework.
- **No grammar compatibility blockers.** `tree-sitter-java =0.23.5` resolved cleanly against `tree-sitter` 0.26.
- **The "Phase 4.4 batches" directive surfaced one bookkeeping gap.** The CLAUDE.md `init all six` count language is now 2 increments stale (efcore + commons-lang both added). Phase 4.4 will set it to `init all eight`. The 3.6 commit message accurately described the deferral as "all seven" being deferred, but the actual CLAUDE.md text is still at "all six" — both 2.6 and 3.6 left it stale. The deferral chain is intentional; the commit message inaccuracy is cosmetic.
- **Workspace fmt drift remains.** Same 7–8 pre-existing dirty files Phase 2 documented. Phase 4.4 owns the sweep.

## Lessons Learned

- **Carry-forward pattern is the highest-leverage validated practice.** Phase 3 ran ~50% faster per-task than Phase 2 because every Wave 2-6 lesson was carry-forward'd into the corresponding Java brief. Records-leak prevention is the canonical example. The validated-3x `/planner:refresh-brief` skill would automate this.
- **Java grammar asymmetry (`superclass`/`super_interfaces`/`extends_interfaces`) is real and worse than C#'s uniform `base_list`.** Required 5 query patterns for inheritance vs C#'s 4. Worth documenting in any future "language plugin onboarding" CLAUDE.md section as a sample of grammar-variance complexity.
- **The body-presence Decision 11 refinement is the right rule for both languages.** Java's `default`/`static`/`private` interface methods all converge on "has body". C#'s C# 8+ default interface methods also use body-presence (no `default` keyword). The brief should have said this from the start; both implementers independently arrived at it.
- **Defensive `_ => {}` catch-all in match-on-node-kind helpers** — Java 3.4 implementer added this proactively after the C# 2.4 review caught a silent-skip. The pattern works and should be a CLAUDE.md coding convention.
- **The `Type::new` limitation pattern is a useful template.** When a grammar produces an awkward shape for a specific case, the right move is: (a) skip it cleanly via filter, (b) document as a Known Limitation in the plugin's lib.rs, (c) pin a no-edge test, (d) carry-forward to the Phase 4.4 CLAUDE.md `## Parser Limitations` section. This decision pattern can apply to future language plugins.
- **Two-commit shape for submodule pinning is the correct safety pattern.** `git submodule add` + `git checkout <tag>` requires a follow-up commit to record the tag SHA in the parent index. The implementer correctly avoided `--amend` and committed the pin-fix as a clear follow-up.

## Impact on Subsequent Phases

- **Phase 4 (Integration and Cutover) has nine documentation obligations** (enumerated in the Phase 2 debrief). Java-specific ones overlap heavily with C#:
  1. **`## Java Parser Limitations` section in CLAUDE.md** — should document: heuristic call resolution; generic-class hierarchy lookup gap; generic-constraint riding-along in `from` (divergence from C#); records-cannot-extend; `Type::new` method-reference limitation; anonymous-class collision per Decision 4; records/sealed simplification per Decision 6.
  2. **`init all six` → `init all eight`** in CLAUDE.md (batched).
  3. **Architecture table + sentence-level updates** ("all four languages" → "all six languages").
  4. **`[extensions]` block growth** (already includes csharp/java from Phase 1's task 1.2; verify intact).
  5. **README supported-languages table 4 → 6 rows.**
- **5-way collision regression in `mixed_language.rs`** — Phase 4.2 widens the existing 3-way `cross_language_init_callers_stay_isolated` to 5-way. Java fixture should use lowercase `init` (Java idiomatic uses camelCase `init` not PascalCase, which works for this test) per the design's load-bearing-test note.
- **Wire-format snapshots** — Phase 4.3 generates 5 new `.snap` files for the Java plugin. Combined with the 5 C# `.snap` files = 10 total new snapshots.

## Skill Opportunities

### 1. Carry-forward pattern validated (re-validated again)

Phase 3 ran more smoothly than Phase 2 because every wave's lessons carry-forward'd cleanly. The pattern's value is well-established — Phase 7 of RustRewrite validated 5x; Phase 2 of CSharpJavaSupport validated again; Phase 3 of CSharpJavaSupport validated a 7th time. The `/planner:carry-forward` skill remains the right next implementation.

### 2. Defensive `_ => {}` catch-all should be a project-level convention

Captured in Phase 2's debrief; re-validated in Phase 3 (Java 3.4 implementer applied it proactively). Should be added to CLAUDE.md as a coding convention under the "C++ Parser Limitations" / equivalent sections OR a new generic "Plugin development conventions" section.

### 3. Two-commit shape for submodule pinning

A small but real recipe: `git submodule add` + `git checkout <tag>` + `git commit` (the pin-fix). Could be a Makefile target `make add-submodule URL=<x> TAG=<y> PATH=external/<name>` that runs the three steps. Mostly useful for future-language-plugin work (Phase 8+ of a hypothetical KotlinScalaSupport plan). Low priority; documented in the commit history is sufficient.

### 4. Brief should NOT enumerate test names exhaustively

The 3.2/3.3/3.5 briefs enumerated specific test function names (e.g., `single_extends_records_inheritance_edge`, `multi_implements_produces_multi_edges`). In every wave, the implementer added 2-3 bonus tests beyond the brief's enumeration (records-leak anti-regression, Java-9+ private case, where-clause non-pollution). The enumeration is therefore a lower bound, not a contract. Future briefs should say "cover the following CONTRACTS" with bullet points naming behaviors, not test function names — implementers can choose naming and may add bonus tests.

### 5. Phase-status doc sweep is a recurring catch

Every C# task (2.2–2.6) AND every Java task (3.2–3.6) had at least one stale phase-status comment caught by review. The sweep pattern (grep for "upcoming"/"wires"/"will land"/"future") is mechanical; could be a `make doc-stale-check` Makefile target that runs the grep against active phase docs. Or — better — a CLAUDE.md "Documentation read cold" lens checklist item for any commit that touches plugin doc comments.

### 6. The "batched-doc-update directive" worked (re-validated)

Same as Phase 2's lesson #6. Phase 3 honored the directive across both 3.2/3.3/3.4/3.5/3.6. The pattern is now ready to elevate to a plan-template convention. Wording suggestion for future plans: "Designate one task as the documentation-batch owner for cross-task doc tables. Contributing tasks leave the doc visibly stale; the batch task does all updates at once."
