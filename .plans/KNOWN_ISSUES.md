---
title: "Known Issues"
type: known-issues
status: open
created: 2026-08-21
updated: 2026-08-22
tags: [resolver, confidence, edges, limitations]
related: [Decisions/decisions.md, Designs/RustSupportGaps/README.md]
---

# Known Issues

Issues that are understood, deliberately not (yet) fixed, and recorded for
follow-up. Each entry: status, how it surfaced, evidence, root cause, options,
recommended fix, and acceptance criteria so a future session can pick it up
cold.

---

## F2 — Generic-resolver receiver calls can resolve as false `Resolved/1` edges

**Status:** open for C++, Rust, Python, C#, and Java (surfaced 2026-08-21,
build-mcp smoke test). Go now carries package/receiver provenance and suppresses
unknown selectors (`ecb3ca6`); sibling findings F1 and F3 were fixed in
`fb4b01e` / `936aca9` and documented in the docs commit.

### Symptom

`get_callers(AdapterRegistry::is_empty)` on the build-mcp codebase returns ~124
callers; exactly one is real. Every false edge is tagged `Resolved` with
`candidates: 1` — i.e. it reads as **maximally trustworthy**, so
`min_confidence=resolved` (the one filter the protocol offers for unverified
picks) cannot remove it. A confidently-wrong edge is worse than a visibly-guessed
one: the `Confidence` protocol exists precisely so agents know how much to
trust an edge, and this case defeats that.

### Evidence (build-mcp smoke test)

- Exactly one `is_empty` symbol exists in the project:
  `crates/build-mcp-service/src/registry.rs:61` (`AdapterRegistry::is_empty`).
- 124 receiver-typed `.is_empty()` call sites under `crates/` (verified by
  `rg '\.is_empty\(\)'`), all on std/external receiver types — `Vec`,
  `String`, `Option`, `HashMap`, `&[u8]`, `impl Iterator`, … — none of which
  are indexed (their `is_empty` lives in std).
- The single **true** caller: `crates/build-mcp/src/server.rs:398`
  (`registry.is_empty()` where `registry: &AdapterRegistry`).
- Even the definition's own body is a false edge: `registry.rs:63`
  (`self.adapters.is_empty()`, receiver is a `HashMap`).
- So ~123 false `Calls` edges land on one method, each stamped
  `Resolved, candidates=1`.

### Root cause

Two structural gaps compound on the generic resolver path
(`crates/code-graph-lang/src/lib.rs`):

1. **The resolver cannot see the call shape.** `CallContext` (lib.rs:88-100)
   carries only `caller_id`, `caller_file`, `language`. There is no signal for
   whether the call was `foo()` (free) or `x.foo()` (method on a value whose
   type is unknown to the index). The provisional `Edge` struct
   (`crates/code-graph-core/src/lib.rs:161`) likewise has no call-shape field,
   though every parser knows the shape at extraction time.
2. **The sole-candidate shortcut fires before any scope reasoning.**
   `default_scope_aware_resolve` (lib.rs:217-219): `candidates.len() == 1 →
   (id, Resolved, 1)`. The scope rule (same file > same parent > same
   namespace > any) only runs for ≥2 candidates. So when a name has exactly
   one indexed definition project-wide, **every** call site bearing that name
   resolves to it as `Resolved/1` — including receiver-typed calls whose true
   target is an unindexed std/external method.

The underlying stance: "the only indexed thing with that name" is treated as
unambiguous by definition. That stance is defensible for free-function calls
(a bare `foo()` with no indexed `foo` at all simply stays unresolved), but it
breaks for receiver-typed calls, where the receiver's type — not the name —
selects the true target, and the receiver's type is frequently outside the
index.

**Scope: the five languages still using generic call resolution.** The
receiver-typed vs. free distinction exists at extraction time in Rust
(`field_expression`/scoped/turbofish), C++ (method/arrow/qualified), Python
(attribute), C# (member-access), and Java (member-access). Go is no longer in
scope: its package-aware resolver preserves known receiver types and drops
unknown/chained selectors rather than invoking the generic fallback. C++ has a
subtlety that *strengthens* the remaining case: an
unqualified call inside a method is an implicit-`this` member call, yet is
genuinely ambiguous with a same-named global — so "unqualified call in method
context" is unverified even when the name has one indexed definition.

### Options

| # | Option | What it does | Cost | Residual |
|---|--------|--------------|------|----------|
| A | Document as a known limitation | CLAUDE.md only | one docs commit | behavior unchanged; false edges still `Resolved/1`, unfilterable |
| B | Downgrade receiver-typed sole-candidate picks to `Heuristic/1` | false edges become machine-detectable via `min_confidence=resolved` | medium (below) | edges still surface under the default `min_confidence=any`; the false target is still *chosen* when it is the only candidate |
| C | Receiver type inference | infer receiver type from decls/params/returns/fields; resolve only on type match | phase-scale (the `#[non_exhaustive]` hook on `Confidence` for "future per-language type-inference variants" is exactly this) | none — but large |

### Recommended fix: **B** (with A's documentation folded in; C is the long-term direction)

Rationale:

1. B restores the protocol's invariant that `Resolved` means "confident this
   is the target". A pick whose receiver type was never checked is not
   confident, whatever the candidate count.
2. `Heuristic/1` is an anticipated wire state, not a protocol break: the
   D-0007 disposition (CLAUDE.md, "Why both signals stay on the wire") already
   holds the axes independent by design — the include resolver emits
   `Resolved/2` today, so a new combination is within the established
   precedent. The stated invariant "for call edges TODAY `Resolved` ⇔ count 1"
   must be reworded, which is a doc edit, not a wire break.
3. B is the lowest-cost precursor to C: `Heuristic/1` *is* the "receiver
   unverified" state that C's type inference will later refine into
   `Resolved` on type match. Doing B first makes the interim behavior honest
   and gives C a clear migration target.
4. A alone leaves the worst failure mode (confidently-wrong edges) in place;
   C alone is months away.

### Implementation notes for B

- **Call-shape plumbing.** For the five generic-resolver languages, add a
  parse-time call-shape field (e.g.
  `receiver_typed: bool`, or a small enum: free / receiver / qualified) to the
  provisional `Edge` or to a per-edge extension of `CallContext`. Set at
  extraction in the five affected `code-graph-lang-*` crates. The resolve loop already
  builds `CallContext` per edge (`crates/code-graph-tools/src/indexer.rs:494`,
  mirrored in `handlers/watch.rs:427,453`), so the ctx side is local. Decide
  whether the field rides into the rkyv cache or is parse-only — if the cache
  encoder serializes `Edge` directly, the version bump below subsumes it.
- **Resolver change.** In `default_scope_aware_resolve`, when the call is
  receiver-typed, skip the `candidates.len() == 1 → Resolved, 1` shortcut and
  return `Heuristic, 1` for the sole candidate. Unqualified free calls keep
  today's shortcut. (C++: "unqualified call whose caller is a method" should
  count as receiver-typed per the implicit-`this` note above — flag this as a
  per-language decision at implementation time.)
- **Cache.** `confidence` is persisted in the v12 cache, and mtime-based
  invalidation does NOT re-resolve unchanged files — so the tag flip will not
  propagate to cached edges without a `CACHE_VERSION` bump (12 → 13,
  `crates/code-graph-graph/src/persist/packed.rs:86`). A bump forces a one-time
  full re-index per project on next run; that is the price of making the fix
  real rather than force-only.
- **Go precedent.** Go now encodes selector provenance in provisional call
  targets, resolves statically known receiver types within the package, and
  drops unknown/chained receivers. That parser-specific approach is evidence
  for long-term Option C, but it does not repair the shared fallback used by
  the other five languages.
- **Docs.** Update CLAUDE.md: the D-0007 "both signals stay on the wire"
  paragraph (the `Resolved ⇔ count 1` sentence), the `Confidence` bullet list
  (add `Heuristic/1` = "sole candidate, but the call is receiver-typed and the
  receiver type was not verifiable against the index"), and the per-language
  "Call resolution heuristic" limitation entries.
- **Tests.** Unit pin for the new shortcut branch (receiver-typed sole →
  `Heuristic/1`; free sole → `Resolved/1` unchanged); a multi-candidate
  receiver-typed case must be unchanged (already `Heuristic/N`); a
  smoke-style integration test mirroring the build-mcp shape (one indexed
  method named `is_empty`, N receiver-typed call sites on unindexed receivers,
  one true caller) asserting `get_callers(min_confidence=resolved)` returns
  only the true caller.

### Acceptance criteria (for the follow-up)

1. `get_callers(AdapterRegistry::is_empty, min_confidence="resolved")` on
   build-mcp returns exactly `server.rs:398`.
2. Under the default `min_confidence="any"`, the ~123 false callers still
   appear but are tagged `Heuristic, candidates: 1` — visible as unverified,
   filterable, and correctly excluded from `find_path`'s
   `min_confidence="resolved"` traversals and the resolved-only filters on
   `get_callees` / `generate_diagram(symbol=…)`.
3. Unqualified free-function sole-candidate resolution is byte-identical to
   today (`Resolved, 1`) — pinned by a test so a future "safety" change cannot
   silently widen the downgrade.
4. `CACHE_VERSION` bumped to 13; the divergence note and CLAUDE.md updates land in
   the same commit as the resolver change (wire-visible behavior change →
   docs move with it).

### Interim mitigation (until B lands)

Agents querying callers of common method names (`is_empty`, `len`, `push`,
`new`) on codebases where the name is also defined in-project should treat
`Resolved/1` callers skeptically and cross-check the receiver type at the call
site. Optional zero-risk first step, independent of B: land Option A's
CLAUDE.md limitation entry so the hazard is at least documented.
