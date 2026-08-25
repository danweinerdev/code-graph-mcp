---
title: "Known Issues"
type: known-issues
status: open
created: 2026-08-21
updated: 2026-08-24
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
**Status:** FIXED 2026-08-24 (`68abe00`) — Option B implemented for all five
generic-resolver languages, plus a SelfReceiver refinement: a parse-time
`CallShape` (Free / SelfReceiver / Receiver) gates the sole-candidate
shortcut in `default_scope_aware_resolve`. Receiver-typed sole-candidate
picks are now `Heuristic/1` (machine-detectable, filterable); `self.`/`this`
calls stay `Resolved/1` iff the sole candidate's parent matches the caller's
parent (the receiver type IS the enclosing class); free/qualified calls are
byte-identical to before (pinned). No `CACHE_VERSION` bump (v13 unreleased,
decision 2026-08-24) — pre-fix dev caches keep the old tags until their
files re-parse; refresh with `analyze_codebase(force=true)`. Option C
(receiver type inference) remains the long-term direction for turning
`Heuristic/1` receiver picks into verified `Resolved` edges — its data cost
is quantified in `Research/option-c-type-inference-memory-impact.md`
(memory is a non-blocker: ~+3-4% cache with a type-name interner; prefer
landing its cache shape before v13 ships). History:
surfaced 2026-08-21 (build-mcp smoke test); Go was fixed separately in
`ecb3ca6`; sibling findings F1/F3 landed in `fb4b01e` / `936aca9`, and F3's
first step in `cd4e19b`. The sections below are the historical analysis.

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
  extraction in the five affected `code-graph-lang-*` crates. The Calls arms
  in `indexer::resolve_edges_with_indexes` and
  `handlers::watch::try_reindex_file` already build `CallContext` per edge, so
  the context-side change is local. Prefer parse-only plumbing (the shape is
  consumed at resolve time, immediately after parse, on both the analyze and
  watch paths) so `PackedEdge` and the cache layout stay untouched.
- **Resolver change.** In `default_scope_aware_resolve`, when the call is
  receiver-typed, skip the `candidates.len() == 1 → Resolved, 1` shortcut and
  return `Heuristic, 1` for the sole candidate. Unqualified free calls keep
  today's shortcut. (C++: "unqualified call whose caller is a method" should
  count as receiver-typed per the implicit-`this` note above — flag this as a
  per-language decision at implementation time.)
- **Cache.** NO `CACHE_VERSION` bump (decided 2026-08-24: v13 has not shipped
  to users, so pre-release semantics changes land within v13). Consequence:
  `confidence` persisted in pre-fix v13 dev caches keeps the old `Resolved/1`
  tags on receiver-typed edges until the owning file re-parses — refresh with
  `analyze_codebase(force=true)` after upgrading a dev checkout. If call-shape
  plumbing stays parse-only (preferred above), the cache layout does not
  change at all.
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

1. ~~`get_callers(AdapterRegistry::is_empty, min_confidence="resolved")` on
   build-mcp returns exactly `server.rs:398`.~~ CORRECTED at implementation:
   `registry.is_empty()` is itself a receiver-typed call the generic
   resolver cannot verify, so as written this criterion is achievable only
   under Option C. The implemented B outcome (pinned end-to-end by
   `crates/code-graph-tools/tests/receiver_shape_resolution.rs`):
   `min_confidence="resolved"` returns exactly the parent-verified
   `self.is_empty()` caller; every unverified receiver caller — true or
   false — is excluded, restoring "resolved = confident".
2. ~~Under the default `min_confidence="any"`, the false callers still
   appear but are tagged `Heuristic, candidates: 1`.~~ DONE (`68abe00`):
   pinned by the same integration test (the `any` page keeps every
   receiver-typed caller) and by the resolver unit pins
   (`f2_receiver_sole_candidate_downgrades_to_heuristic`,
   `f2_self_receiver_sole_candidate_without_parent_match_downgrades`).
3. ~~Unqualified free-function sole-candidate resolution is byte-identical
   (`Resolved, 1`).~~ DONE: pinned by `f2_free_sole_candidate_stays_resolved`
   plus per-parser shape pins asserting direct/qualified/implicit-`this`
   calls extract as `Free`.
4. ~~NO `CACHE_VERSION` bump; docs land with the resolver change.~~ DONE:
   no bump (parse-only `Edge.shape`, cache layout untouched); CLAUDE.md's
   confidence section, D-0007 disposition, and `min_confidence` semantics
   were rewritten in `68abe00`.

### Accepted residuals (post-fix)

Documented conservative or out-of-scope leftovers, accepted pending Option C
(receiver type inference):

1. **Callable-value parameters in Rust, C++, C#, and Java.** A bare call to
   a function-typed parameter or local (`fn f(cb: fn()) { cb() }`) still
   classifies `Free`, so a sole same-named indexed symbol resolves
   `Resolved/1` (phase-12 review, blind-spots cycle-5 F2). Python is FIXED
   for parameter- and body-bound bare calls
   (`python_locally_bound_callable` degrades them to Receiver) — but NOT
   for module-scope value re-bindings (`cb = get()` at module level, then
   `cb()` anywhere): those stay `Free` by the same design trade-off that
   keeps `from utils import cbx; cbx()` resolvable (degrading module-scope
   names would gut Python's dominant cross-module call pattern; quality
   lane cycle-6 observation). The statically typed languages are accepted debt —
   their type systems narrow the hazard (a shadowing local is visible at the
   declaration) but the syntactic resolver does not consult it. Option C's
   local-binding tracking is the structural fix.
2. **C++ inline-vs-out-of-line nested-parent mismatch.** An in-class-declared
   member records parent `Inner` while an out-of-line qualified caller ID
   carries `Outer::Inner`; the SelfReceiver compare fails and the true edge
   downgrades to `Heuristic/1` — a false DOWNGRADE, never a false
   `Resolved` (documented at `caller_id_full_parent`).
3. **C++ implicit-`this` with an unindexed inherited member.** An unqualified
   call inside a method stays `Free`; if the sole indexed candidate is a
   global while an UNINDEXED inherited member actually shadows it, the pick
   is wrong but count-1 sole-candidate picks of this shape are otherwise
   overwhelmingly correct (documented at `cpp_call_shape`).
4. **Python degenerate rebindings the scan cannot see** (e.g. `exec`,
   attribute-injected globals) remain theoretically able to shadow a
   receiver; every statically visible binding construct is covered and
   pinned.

### Interim mitigation (obsolete)

Superseded by the fix: `Resolved/1` on a call edge now means verified. The
one residual caveat is pre-fix v13 dev caches, which keep the old tags until
the owning files re-parse — run `analyze_codebase(force=true)` after
upgrading a checkout.

---

## F3 — Unresolved provisional call markers persist in the graph cache
**Status:** first step landed 2026-08-24 (`cd4e19b`): opportunities 1 and 2
below are closed — `@bound-receiver::` markers are discarded at resolve time
via the new `LanguagePlugin::discard_unresolved_call` hook (default: retain),
the retention invariant is documented at both resolve loops, and the
misleading watch-path comment is corrected. Remaining: opportunity 3
(measure marker counts / cache bytes on a large Go repository before any
broader compaction policy). No `CACHE_VERSION` bump was taken — v13 is
unreleased, so pre-fix dev caches refresh via `force=true` or natural
re-parse. Originally surfaced 2026-08-22 during the shared-process
adversarial review; no public query ever exposed these markers.

### Symptom and evidence

The Go parser uses internal call-target markers to preserve syntax that the
resolver needs:

- `@method-receiver::` for a statically named receiver type;
- `@bound-receiver::` for a local/function-value or otherwise shadowing
  binding that must not fall through to a same-named project symbol;
- `@dot-import::` for a bare call whose provenance may be a dot import.

When `resolve_call` returns `None`, the shared resolve loop intentionally keeps
the provisional target — EXCEPT markers the owning plugin declares terminal
via `discard_unresolved_call` (Go: `@bound-receiver::`, which its resolver
maps to `None` unconditionally). Retained markers (`@method-receiver::`,
`@dot-import::`) still serialize through `PackedEdge` into
`Graph.adj`/`Graph.radj` and `.code-graph-cache.db` until their source file is
re-parsed, because scoped cache growth can add the missing target later.
Public call-graph, diagram, path, and community traversals filter targets that
are not graph nodes, so retention remains an internal storage/diagnostic
matter rather than a wire-contract defect.

### Follow-up opportunities

1. ~~Document the internal invariant.~~ DONE (`cd4e19b`): both resolve loops
   state that unresolved Calls edges keep their provisional token and that
   clients never receive non-node targets; the watch-path comment no longer
   implies all stored edges are resolved.
2. ~~Drop permanently terminal markers.~~ DONE (`cd4e19b`):
   `@bound-receiver::` is discarded after resolution in both the indexer and
   watch loops. `@method-receiver::` / `@dot-import::` remain retained by
   design.
3. **Measure before broader cleanup.** Record marker counts and packed-cache
   bytes on a large Go repository before adding a generalized unresolved-edge
   compaction policy.

### Recommended next step

Only opportunity 3 remains. Treat any broader compaction as a separate design
because it changes cached-edge retention semantics.

### Acceptance criteria

1. ~~No `@bound-receiver::` target survives the resolve/merge path or a cache
   round trip.~~ Pinned by `indexer::resolve_all_edges_discards_terminal_markers_and_retains_other_unresolved`,
   `go_resolution::go_terminal_bound_receiver_markers_never_reach_the_cache`,
   and `watch_go_reindex::watch_go_reindex_discards_terminal_bound_receiver_markers`.
2. ~~Resolvable `@method-receiver::` and `@dot-import::` behavior remains
   unchanged, including scoped-analysis cases.~~ Same pins assert the
   method-receiver marker is retained; existing go_resolution suites cover
   resolvable cases.
3. ~~All public traversals continue to emit only real graph nodes.~~ Pinned
   (empty-callee assertions in both new integration tests).
4. ~~The watch-path and cache documentation accurately distinguish resolved
   edges from retained provisional tokens.~~ DONE (`cd4e19b`).

---

## F4 — Go manifest changes conservatively rebuild every indexed Go file

**Status:** open performance opportunity (tracked from D-0017's deferred
optimization boundary; correctness behavior is intentional).

### Current behavior and reason

A watched `go.mod` create, modify, or remove event atomically reparses and
re-resolves every indexed Go file. This is conservative but correctness-
preserving: a changed nested module can alter ownership or newly enable an
importer outside the manifest's directory, while previously unresolved imports
leave no reverse graph edge from which to select affected files.

The resolver currently persists each file's declared package name and
package-level value bindings, but it does not persist reverse manifest-
dependency metadata.
Without that reverse index, narrowing the transaction by directory or by
existing import edges can silently miss external importers and imports that
were unresolved under the old manifest universe.

### Replacement criterion

Replace the all-Go transaction only after the cache can identify, for each
manifest change, every indexed Go file whose module ownership, package import
binding, or previously unresolved import may change. The metadata must remain
coherent across scoped analyzes, nested modules, cache reloads, manifest
removal, and watch failures; the selected subset must publish atomically just
as the current all-Go transaction does.

### Acceptance criteria

1. Nested-module create/modify/remove tests prove importers both inside and
   outside the changed manifest directory are selected.
2. A previously unresolved import that becomes resolvable after a manifest
   change is selected despite having no old reverse edge.
3. Scoped-cache and cold-restart tests prove the reverse metadata survives and
   remains coherent with cached symbols.
4. Benchmarks on a multi-module repository demonstrate a material watch-
   latency improvement over the all-Go rebuild before the added cache and
   invalidation complexity is accepted.

---

## F5 — Non-watch analyze does not detect manifest-only Go changes

**Status:** known limitation (surfaced 2026-08-22 during resolution
verification). Use watch mode or `analyze_codebase(<project_root>, force=true)`
after editing `go.mod`.

### Current behavior

The non-forced analyze fast path compares indexed source-file mtimes and
discovers uncached source files. No language plugin claims `go.mod`, and the
cache does not persist manifest mtimes, so a manifest-only edit can take the
fast path and retain the prior module-qualified Go namespaces. The graph stays
internally consistent under the old module model; the limitation is stale
metadata rather than mixed identities or malformed edges.

Watch-delivered manifest events perform the all-Go transaction when they
acquire the index lock. A contended event records a pending transaction that
the next Go source or manifest event retries; without a later Go event, a
forced analyze at the project root is the guaranteed refresh. Scoped force
only rebuilds that invocation's subtree. A future automatic fix requires
persisting enough manifest identity (path plus mtime or content identity) to
include the relevant manifest set in analyze staleness without weakening
scoped-cache and nested-module behavior.

---

## F6 — Daemon idle-shutdown unit test livelocks under WSL2

**Status:** open environment incompatibility (surfaced 2026-08-25 while
producing phase-12's Linux verification). NOT a phase-12 regression — it
reproduces identically at the phase range start (`0b41bbd`) and at the final
candidate (`f9ca2ca`); the same suites pass natively on Windows, and the
owner's Linux runs have not reported it outside WSL.

### Symptom

`cargo test -p code-graph-mcp --bin code-graph-mcp
run_until_uses_the_idle_future_to_close_and_cleanup_the_listener` fails
deterministically (3/3) on WSL2 (Fedora, kernel
`6.18.33.2-microsoft-standard-WSL2`, tokio 1.52.1 and 1.53.1, Rust 1.98):
the test's outer 5s `tokio::time::timeout` fires (`Elapsed`) while
`run_until`'s 20ms idle timeout never does. Everything else in `make verify`
passes in the same environment (1,243 of 1,244 workspace tests; clippy
`-D warnings`, fmt, snapshots green).

### Diagnostics gathered (all in a disposable WSL clone)

- Probes: lock acquired → listener bound (UDS via the procfd alias, no
  fallback breadcrumbs) → `serve_uds`'s select polled exactly TWICE → then
  the runtime thread burns 100% CPU (`utime` +200 ticks in 2s, `wchan=0`,
  empty syscall — spinning in userspace) with ZERO further task polls and
  ZERO timer deliveries for ~5s.
- Isolation probes all PASS in the same binary/environment: a bare 20ms
  sleep on a current-thread runtime; sleep-vs-accept on a freshly bound
  procfd-aliased UDS listener; the full nested select/loop shape of
  `serve_uds` reconstructed minimally.
- Component bisects all still FAIL: ownership watchdog replaced with
  `pending()`, shutdown-request poller disabled, `wait_for_idle_shutdown`
  replaced with a bare `sleep(20ms)` (which then never fires), the Linux
  retained-root/procfd machinery disabled.
- tokio 1.53.1: identical failure. `multi_thread` flavor: identical failure
  — so it is not a current-thread-runtime starvation story; the stall
  travels with the composition.

The spin sits below the task layer (driver level) and only in the FULL
`run_until` composition; no minimal reconstruction reproduces it. The
observed shape — no timer delivery, no task polls, 100% CPU, then the 5s
timer firing late — points at a WSL2 kernel/epoll interaction rather than a
defect in this repo's code, but the responsible syscall could not be
identified without strace/gdb (not installed; no passwordless sudo).

### Follow-up

1. Reproduce on a real Linux kernel to confirm the environmental
   classification (expected: passes — the composition is exercised by the
   sibling daemon tests that pass everywhere).
2. If WSL2 support matters, retry the diagnosis with strace available and
   file upstream (tokio or WSL2 kernel) with the minimal reproduction once
   the storming fd is identified.
3. Also observed in the same environment: `make plugin-sync-check` needs
   diffutils (fixed — the script now fails honestly with an install hint,
   `622949a`).
