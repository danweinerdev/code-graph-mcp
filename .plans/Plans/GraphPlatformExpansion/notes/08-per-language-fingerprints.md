# Phase 8 debrief — Per-Language Fingerprints

Closed 2026-08-19. Seven tasks (six planned + one gate-driven fix task),
two gate cycles, final aligned review at
`reviews/20-per-language-fingerprints-final-review-77f3cd4-dc8020e.md`.

## What happened

- **8.1 earned its "go first" slot twice.** The C++ task settled the
  preprocess interaction exactly as the plan predicted (the history walk
  now passes the SAME bytes the parse saw, stated on the trait), and the
  shared walk it established (`locate_symbol_node` + `ast_fingerprint` in
  `code-graph-lang::fingerprint`) made 8.3–8.6 genuinely mechanical.
- **8.2 caught the shared-walk gap the plan said it would.** The
  rustfmt-shaped reformat fixture (multiline + trailing comma) exposed
  that the walk hashed comma tokens — a reformat-only commit would have
  reported `modified`. Fixed once in the shared walk; Go then proved the
  fix load-bearing (gofmt REQUIRES the trailing comma).
- **Grammar reality beat grammar assumptions twice.** C#'s literal text
  runs are spelled differently per string form (`string_literal_content`
  vs `string_content` — found by a failing test, verified by sexp dump),
  and four Rust literal kinds I assumed existed are phantoms (v0.24.2
  folds byte/C-string forms into the covered kinds). Fixture-first
  development caught both.
- **The gate's materials were both documentation-as-production-behavior.**
  Blind-spots cycle 1 found the Rust "a derive change is a logic change"
  claim false for OUTER attributes (`attribute_item` is a sibling of the
  item node, outside the fingerprinted span — and the 8.2 test had been
  written AROUND the gap), and `locate_symbol_node`'s template-wrapper
  claim unreachable by construction (extractors record the inner node).
  Task 8.7 qualified both claims and pinned both boundaries with tests
  that fail if the behavior ever changes — converting silent limitations
  into enforced contracts.
- **Cycle 2 confirmed the pins prove AST-path behavior, not luck:** a
  fallback-path pass is structurally impossible under LiteralInsensitive
  (the overrides return `None` on locate failure and the test helpers
  `.expect()`).

## What to carry forward

- **Test the claim, not around it.** The 8.2 attribute test knowingly
  used an inner attribute because the outer case would have disproved the
  doc. Writing the test AT the boundary (as 8.7 did) is cheaper than the
  gate cycle it cost. If a fixture has to dodge a case to pass, the doc
  is wrong, not the fixture.
- **Sexp-dump before predicate-writing.** Ten minutes of dumping the
  actual grammar's node kinds beats guessing from another language's
  conventions — two of the six predicates needed correction discovered by
  failing tests.
- **Shared abstractions surface their gaps in language #2, not #6.** The
  plan's sequencing note ("if the abstraction is awkward it will show in
  8.2 — fix it there rather than replicating four more times") was
  exactly right; the comma fix landed before three copies existed.
- **The intent-blind lane: fifth phase running.** Both materials were in
  documentation the plan-aware lanes had already read approvingly.
- **Open follow-ups** are in artifact 20: the Python decorator pin, the
  sextuplicated override body (consolidate before a seventh language),
  the C++-only degradation/determinism unit tests, the tool-level
  literal_insensitive skip-arm test, the uncached skip outcome, sidecar
  shard growth (phase 6 scope), and the theoretical hash-framing
  ambiguity. None gates phase 9.
