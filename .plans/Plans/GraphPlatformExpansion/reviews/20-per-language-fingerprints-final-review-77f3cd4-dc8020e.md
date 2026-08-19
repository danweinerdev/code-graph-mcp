---
title: "Phase review: Per-Language Fingerprints"
type: review
status: resolved
created: 2026-08-19
updated: 2026-08-19
tags: [review, fingerprint, ast, literal-insensitive, phase-8, final]
related: ["Plans/GraphPlatformExpansion/08-Per-Language-Fingerprints.md"]
review_of: "Plans/GraphPlatformExpansion/08-Per-Language-Fingerprints.md"
rev: "77f3cd47aba9a0d7d3750218caf5fc4548ba6d1e..dc8020e837cb353a68790d23121d4f9878f4e022"
review_scope: phase
frozen: true
verdict: Aligned
reviewed_planning_revision: "3ed486509d2acb6e039079c18bdae67000b640ec"
review_mode: independent
lane_results:
  - lane: review_plan_drift
    result: PASS/Aligned
    reviewed_identity: "77f3cd47aba9a0d7d3750218caf5fc4548ba6d1e..dc8020e837cb353a68790d23121d4f9878f4e022"
    evidence: "Task 8.7's record is well-formed (the frontmatter tasks block parses sanely — a mid-entry YAML corruption during authoring was caught and repaired before commit; 8.6 and 8.7 each carry full fields) and its evidence block's claims all check out against f8553ba (5 files, the two boundary-pin tests, 10->6 predicate arms with the verification method documented, no residual 'later phase' text anywhere in crates). The verification filters are honest, including the deliberate cpp `fingerprint` (no `::`) spelling that selects both test modules. Whole-range coherence holds across 8.1-8.7; the accepted follow-ups are real and none is a downgraded material. One new minor (the 'pinned per language' overclaim for Python) — repaired at the planning revision."
  - lane: review_quality
    result: PASS/Aligned
    reviewed_identity: "77f3cd47aba9a0d7d3750218caf5fc4548ba6d1e..dc8020e837cb353a68790d23121d4f9878f4e022"
    evidence: "All six 8.7 fixes verified correct without regression: the Rust boundary pin fails either way if the extractor span widened (Normalized falls to the text default and mismatches; LiteralInsensitive panics on None); the C++ template pin's fixture genuinely produces the wrapper + inner definition and both arms discriminate; the phantom-kind removal and the remaining six-kind list verified against the vendored tree-sitter-rust 0.24.2 node-types.json (negative_literal correctly absent — the walk descends through it); the matrix-intro boundary statements verified against ast_fingerprint's raw byte-range hashing. New minors: the Python 'pinned per language' overclaim (repaired at the planning revision), the 0.24.0-vs-0.24.2 version spelling (repaired), the uncached literal_insensitive skip outcome (perf-only, filed), a negative_literal comment suggestion. Nothing material parked in the follow-up list."
  - lane: review_spec_compliance
    result: PASS/Aligned
    reviewed_identity: "77f3cd47aba9a0d7d3750218caf5fc4548ba6d1e..dc8020e837cb353a68790d23121d4f9878f4e022"
    evidence: "f8553ba regressed nothing: the diff is docs, help text, two boundary-pin tests, and dead-arm deletion (independently verified dead against the vendored grammar), so the AC-39/AC-38/FR-34 evidence from 8.1-8.6 stands. The new pins do not conflict with AC-38 — a derive or template-clause edit is not a reformat; their invisibility is an extractor-span-scope property now documented rather than contradicted. NFR-02 holds across the whole range (zero manifest/lockfile changes). NFR-11 holds with the matrix documenting both cross-language boundaries; the boundary nuance living in the matrix rather than the tool description is judged sufficient. The 'No language returns None' AC remains honest — the 8.7 candor concerns fingerprint COVERAGE, orthogonal to mode SUPPORT. Nothing found that would make ticking any AC checkbox dishonest."
  - lane: review_blind_spots
    result: PASS/Aligned
    reviewed_identity: "77f3cd47aba9a0d7d3750218caf5fc4548ba6d1e..dc8020e837cb353a68790d23121d4f9878f4e022"
    evidence: "The discrimination attack on both new pins dissolves: the overrides return None (not the text fallback) under LiteralInsensitive on locate failure and the test helpers .expect(), so a fallback-path pass is structurally impossible — the pins prove AST-path behavior, confirmed statically (Rust make_symbol records struct_item's start with attribute_item a grammar sibling; C++ records the inner def node with template_declaration used only for signature prefixing; inner_attribute_item verified a _declaration_statement subtype). The history walk parses exactly twice per revision for the single queried symbol; the mode-keyed cache is bounded per walk; no stale mode surface survives in the CLI, server descriptions, plugin mirrors, or docs. New sub-material findings filed: the sidecar never prunes dead-identity entries (growth doubled by the second mode), and the pins' discrimination rests implicitly on LiteralInsensitive's no-fallback property. Nothing material hides in the follow-up list."
findings: []
followups:
  - "Add a Python decorator-boundary pin (the extractor-span invisibility is test-pinned for Rust and C++ only; Python's decorator transparency follows the same convention but has no dedicated fingerprint test)."
  - "Consolidate the sextuplicated ~25-line fingerprint_symbol override body into a shared code-graph-lang::fingerprint helper before a seventh language lands."
  - "Unlocatable-span per-mode degradation and cross-parser-instance determinism are unit-tested only in the C++ suite; the other five carry identical but untested copies."
  - "No tool-level test drives the literal_insensitive skip arm end-to-end (the 'span not fingerprintable under mode' reason string is never asserted through symbol_history)."
  - "The literal_insensitive unfingerprintable-span outcome is not cached (unlike Fingerprint/Tombstone), so repeated walks re-parse those revisions each time — perf-only."
  - "The fingerprint sidecar never prunes entries keyed to dead binary/config identities; shards grow monotonically across rebuilds and the second live mode doubles the rate ('safe to delete' covers it operationally; pre-existing, phase 6 scope)."
  - "Theoretical hash-stream framing ambiguity in ast_fingerprint (leaf text containing a raw 0x1F plus a kind spelling could mimic a sibling boundary — not constructible from realistic edits; a leaf-length prefix would close it)."
  - "The boundary pins' AST-path-vs-fallback discrimination rests implicitly on LiteralInsensitive's no-fallback property; if that property ever changes, the discrimination silently vanishes while the pinned span-exclusion contract keeps holding."
  - "A one-line comment on rust_literal_kind noting negative_literal is deliberately absent (pattern wrapper the walk descends through) would prevent a future 'completeness fix' from wrongly adding it."
  - "The symbol_history tool description says 'changed literals and code are [reported]' — an outer-attribute or template-clause edit is colloquially code yet invisible (extractor-span boundary, documented in the matrix); add the caveat to the description only if agent confusion is observed (description is at length budget)."
---

# Phase review: Per-Language Fingerprints

Reviewed `Plans/GraphPlatformExpansion/08-Per-Language-Fingerprints.md` at
frozen identity `77f3cd4..dc8020e` (planning content at `3ed4865`).
**Review mode:** independent lanes — four parallel, non-inheriting contexts
with isolated inputs, dispatched twice.

## Cycle history

- **Cycle 1** (endpoint `b2219e6`): three lanes PASS/Aligned (drift found
  the filter-string discipline HELD this phase — all six `fingerprint::`
  filters select real in-lib modules; spec verified AC-39/AC-38 per
  language with the trailing-comma fixtures load-bearing); blind-spots
  returned Needs-changes with two materials, both
  documentation-as-production-behavior: (M1) the Rust "attributes are
  code — a derive change is a logic change" claim was false for OUTER
  attributes (`attribute_item` is a SIBLING of the item node in
  tree-sitter-rust, outside the extractor-recorded span — and the 8.2
  test had been written around the gap rather than at it); (M2)
  `locate_symbol_node`'s doc claimed `template_declaration` wrappers
  "fingerprint their whole declaration" — unreachable by construction,
  since every extractor records the INNER definition node's position.
  Plus minors: stale CLI `--mode` help, four phantom Rust literal-kind
  spellings, undocumented CRLF sensitivity. Fixed as task 8.7
  (`f8553ba`): both boundaries qualified in the docs AND pinned by tests
  that fail if the behavior ever changes (converting silent limitations
  into enforced contracts), help text fixed, phantom kinds removed with
  the verification method documented, boundaries added to the matrix
  intro.
- **Cycle 2** (this artifact): all four lanes PASS/Aligned. The
  discrimination attack on the new pins dissolved (a fallback-path pass
  is structurally impossible under LiteralInsensitive). Two sub-material
  record inaccuracies repaired at the planning revision `3ed4865` (the
  "pinned per language" overclaim for Python; the 0.24.0-vs-0.24.2
  version spelling). Residual observations recorded above as follow-ups;
  none is material to the phase deliverable.

## Verification

- `make verify` at the endpoint: PASS (`exit 0`) — clippy `-D warnings`,
  rustfmt, full workspace tests (natively on Windows), pending-snapshot
  check, plugin-mirror sync.
- Per-language fingerprint suites: C++ 7 (incl. the macro-strip
  determinism pin, per-mode degradation, and the template boundary pin),
  Rust 6 (incl. the lifetime-list supersession and the outer-attribute
  boundary pin), Go 4 (incl. the mandatory-trailing-comma reformat),
  Python 4 (incl. the structural-vs-cosmetic indentation discriminator
  and f-string token-level handling), C# 4 (incl. the dual literal-run
  spelling), Java 4.
- `cargo test -p code-graph-tools --test symbol_history` (13): including
  the reworked mode test driving `literal_insensitive` END TO END through
  the transition fixture.

## Resolution Log

No open findings; the follow-ups above are accepted, recorded
non-blockers.
