---
title: "Code Review: Unpushed feature/shared-process Change Set"
type: review
tags: [review, go-resolver, search, index, cache-v12]
related: [KNOWN_ISSUES.md, Decisions/decisions.md, Designs/RustSupportGaps/README.md]
review_of: "Plans/RustSupportGaps"
rev: "14e268180aa3f61a12e211168ec59cd257340b34..57d6ee5c52b13de7a710b1822d42212e7c437c34"
reviewed_range: origin/feature/shared-process..HEAD (57d6ee5)
review_mode: four independent lanes + primary verification
verdict: Needs changes
---

# Code Review: Unpushed feature/shared-process Change Set

**Reviewed range:** `origin/feature/shared-process..HEAD` — 8 commits, 29 files,
+2571/−204, endpoint `57d6ee5`.
**Review mode:** Four independently-dispatched lanes (quality, blind-spots,
spec-compliance, plan-drift) with input isolation, plus primary-agent
verification (full `make verify` run, direct code traces, one empirical
scratch test, deleted after use). Lane outputs were consolidated; no lane
was passed plan/spec material it was not entitled to (quality and blind-spots
saw diff + code only).
**Verdict: Needs changes** — one proven false-edge defect (F-1, major) that
falls into exactly the defect class the repo has already declared
unacceptable in KNOWN_ISSUES F2. Everything else is minor/nit and can land
as follow-ups.

## Change-set summary
| Commit | Content |
|---|---|
| `980db06` | Plugin maintenance: `argument-hint` quoting fix in all three trees, Go dependency-guidance text, cache v8→v12 doc in `cg-index.md`, OpenCode command-discovery change (plugin no longer registers the commands dir; README points at a local installer), `code-graph.js` registration removal |
| `fb4b01e` | Rust smoke fix F1: `extend_symbol_index` registers de-genericized `Parent::name` / `Namespace::Parent::name` keys (`strip_generic_params`), so unparameterized `Page::new` against `impl<T> Page<T>` resolves |
| `936aca9` | Search smoke fix F3: `Graph::search` pattern tested against the short name AND, for methods, the qualified `Parent::name` form (OR); `pattern_matches_symbol` helper shared by materializing + `count_only` branches; tool description + tools/list snapshot updated |
| `89b30fe` | CLAUDE.md: documents both fixes + the absent-on-empty `warnings` serialization |
| `ccc1cd0` | New `LanguagePlugin::prepare_resolution` trait hook (object-safe, default no-op); called from `resolve_edges_with_indexes`, watch reindex; `resolve_edges_with_indexes` signature gains the full resolution universe |
| `ecb3ca6` | Go package-aware call + import resolution: `GoResolutionState` (package identities, package value bindings, imported packages, deterministic representatives), `resolve_call`/`resolve_include` overrides, extraction-time call-shape markers (`@method-receiver::`, `@bound-receiver::`, `@dot-import::`, `import::name`), `_test.go` visibility rules, 831-line `go_resolution.rs` integration suite, corpus counts 41→36, **CACHE_VERSION 11→12** |
| `c805df8` | `.plans/KNOWN_ISSUES.md` created: F2 (generic-resolver receiver false `Resolved/1` edges) with root cause, options A/B/C, recommended fix B, acceptance criteria |
| `57d6ee5` | Plugin Go dependency guidance + remaining doc sync |

Also in the working tree (NOT in the commit set): untracked
`scripts/local-install-opencode-plugin.sh` — see F-7.

## Verification (actual results, 2026-08-22)
- `make verify` — **PASS end to end**: clippy `--workspace --all-targets
  -- -D warnings` (clean), `cargo fmt --all --check` (clean),
  `cargo test --workspace` (88 test binaries, all `test result: ok`, zero
  failures), `snapshot-clean` (no pending snapshots), `plugin-sync-check`
  (mirrors in sync with `plugin/`).
- `git status`: clean except the untracked installer script.
- Scratch empirical test (written under `crates/code-graph-lang-go/tests/`,
  run, deleted): proved F-1 — see below. Not part of the repo.

## Findings
| # | Severity | Source lens | Location | Issue | Recommendation |
|---|---|---|---|---|---|
| F-1 | **major** | blind-spots (missed by all lanes; proved by primary trace + empirical test) | `crates/code-graph-lang-go/src/lib.rs` `binding_scope_started` | `rposition(b':')` byte-scan misparses if/switch init declarations when a string literal or comment containing `:` sits between the declaration and a call in the condition part — a same-name-shadowed receiver call is attributed to the OUTER binding and can resolve to the wrong symbol as `Resolved/1` | Fix with AST token positions (the `;` separating init from condition is a tree node), add regression test |
| F-2 | minor (quality said major) | quality + blind-spots (corroborated) | `prepare_resolution` + `GoModuleModel::discover` | Every watch edit rebuilds Go resolver state over the complete graph: O(N×depth) `go.mod` ancestor probes + manifest re-reads, under `index_lock`, delaying subsequent watch events (excess dropped) | Memoize module discovery / per-directory go.mod lookups; follow-up with benchmark |
| F-3 | minor | blind-spots (proven) | `indexer.rs` Calls arm, `Graph::merge_file_graph`, `packed.rs` | Unresolved Go calls persist into the Graph and rkyv cache with internal marker tokens (`@bound-receiver::…` etc.). Pre-existing retention behavior; queries are safe (`is_resolved_node` BFS filter; community aggregation skips non-node targets; includes dropped) | Document that unresolved call edges store provisional tokens; optionally drop `@bound-receiver::` edges at resolve (provably never resolvable) |
| F-4 | minor | quality | `core/analyze.rs` ~527 | `cached_snapshot` is fully cloned by `file_graphs_snapshot()`, then re-cloned into `resolution_graphs` via `iter().filter().cloned()` — two full copies of the cached universe peak simultaneously | `cached_snapshot.into_iter().filter(…).collect()` (snapshot unused afterwards) moves instead of clones |
| F-5 | minor | spec | `packed.rs:79-80` | v12 rationale comment names only Go semantics; the de-genericized index-key change also alters resolution for non-Go generic parents (Rust proven by the smoke fix; C#/Java latent — their method parents are currently bare names) | Expand comment to name both causes |
| F-6 | minor | plan-drift | CLAUDE.md Go section | `_test.go` visibility/isolation (internal tests see production, never the reverse; external `x_test` isolated) is implemented and integration-tested (`go_resolution.rs:621-720`) but undocumented | One line in Go "Supported" |
| F-7 | minor | plan-drift + primary (corroborated) | `opencode-plugin/README.md:52-55`, untracked `scripts/local-install-opencode-plugin.sh:6-7` | Committed README references "this repository's local installer" but the script is untracked; the script's header claims it is "listed in `.git/info/exclude`" but `git check-ignore` reports NOT-IGNORED — fresh clones lack the installer, and `git add -A` would sweep the script in | Commit the script or reword the README; and either register the exclude the comment claims or fix the comment |
| F-8 | minor | spec (verified) | `graph.rs:100`, `callgraph.rs:47` | Comments cite "the v11 bump" as the operative cache-safety mechanism; current version is 12 (the v11 entries in `packed.rs:70-77` / `persist/mod.rs:600` are historical changelog lines and are correct as history) | Bump the two present-tense references to v12 |
| F-9 | minor | spec (verified) | `.plans/KNOWN_ISSUES.md:124-125` | F2's implementation notes cite `indexer.rs:494` (CallContext construction is actually `:500-504`) and `watch.rs:427,453` (actually `~:424`, `~:450`) — stale at commit time in a doc designed to be picked up cold | Fix citations, or cite by symbol name |
| F-10 | minor | quality | `tests/corpus.rs` + `testdata/go/MANIFEST.md` | Corpus test counts PROVISIONAL extraction edges (no resolution pass is run — zero `resolve*` references in the file), while MANIFEST prose describes resolved targets (`init -> utils.Add`). Counts happen to agree because the 5 dropped edges were dropped at extraction; a future edge dropped at RESOLUTION time would not move the corpus count | Label the MANIFEST edge table as extraction-stage counts |
| F-11 | minor | quality + blind-spots (corroborated) | `GoParser.metadata` | `metadata` map is inserted for every parsed path and never evicted (no removal on delete/sweep/rename); unbounded over a long daemon session, though real re-parses do overwrite (true-insert) vs disk-loaded (`or_insert`) | Prune to the active Go-path set inside `prepare_resolution` (the set is already computed) |
| F-12 | minor | quality | `go_resolution.rs` | The documented cold-cache staleness seam (CLAUDE.md "Cold-cache Go resolver metadata") has no pinning test; the Go watch test has no `go.mod` and exercises neither module discovery nor package-value metadata | Add restart-with-stale-sibling fixture; watch fixture with module import + sibling package-level binding |
| F-13 | nit | spec + plan-drift (corroborated) | commits `980db06`, `57d6ee5` | `980db06` "Updated issues in the plugins" carries the OpenCode command-discovery behavior change and the `argument-hint` quoting fix unnamed; `57d6ee5` also carries the v8→v12 cache-doc fix. Content is fine; messages under-describe | Future commits: name the behavior change |
| F-14 | nit | spec (pre-existing) | CLAUDE.md:427 | "Unique same-package cross-file calls resolve **before the generic fallback**" — Go overrides `resolve_call` entirely; no generic fallback exists for Go anymore | Reword ("via the package-aware resolver") |
| F-15 | nit | spec (pre-existing) | `server.rs` `search_symbols` description | Lens checklist "every named arg documented with default + ceiling": `near` and `count_only` lack explicit defaults (`offset`/`query` have no ceiling to state). Pre-existing; the diff only touched the `query` sentence | Optional cleanup, out of scope for this change set |
| F-16 | nit | spec (pre-existing) | `docs/SMOKE_TEST.md:67` | Still says `.code-graph-cache.json` "should be loaded directly"; current behavior treats the legacy JSON cache as not-present. Not touched by this diff | Fix in a docs pass |

## Detailed analysis
### F-1 — `binding_scope_started` misattributes shadowed receivers (major, proven)

**Code.** `crates/code-graph-lang-go/src/lib.rs`, `binding_scope_started`:
for an `if`/`switch` scope whose call is NOT inside the body block, the
function decides "is the short-init declaration in scope at the call
position" by byte-scanning the source between the scope start and the call
start:

```rust
let header = &content[scope.start_byte()..call.start_byte()];
match header.iter().rposition(|&byte| byte == b':') {
    Some(declaration) => header[declaration..].contains(&b';'),
    None => true,
}
```

The `rposition` for `':'` is not token-aware: a colon inside a **string
literal or comment** in the condition part becomes the "declaration colon",
and the trailing-`;` check then fails, so the init declaration is treated
as **not** in scope at the call.

**Empirical proof** (scratch integration test, run against `ecb3ca6`,
deleted afterwards). Same shape, differing only in a string literal:

```go
// control
func outer(s *Outer) { if s := inner(); cond && s.Check() {} }
// variant
func outer(s *Outer) { if s := inner(); strings.Contains(x, "a:b") && s.Check() {} }
```

Extraction targets for `s.Check()`:

- control:  `@bound-receiver::Check`            (marked unknown → dropped at resolve — safe)
- variant:  `@method-receiver::\u{1f}Outer\u{1f}Check`   (attributed to the OUTER `s *Outer` parameter)

Mechanism: with the if-arm disabled, `lexical_binding_exists` continues the
ancestor walk and finds the outer `s` parameter; `method_receiver_type`
then returns `Outer` from the `function_declaration` arm. If `Outer` has a
`Check` method in the caller's package, `resolve_go_method` finds exactly
one visible candidate and the edge lands as **`Resolved`, `candidates: 1`**
— pointing at the wrong symbol, tagged with maximal trust.

**Why this is the top finding.** This is precisely the defect class
KNOWN_ISSUES F2 declares unacceptable for the other five languages
("A confidently-wrong edge is worse than a visibly-guessed one"), and it
occurs in Go — the language this change set positions as the worked example
("Go now carries package/receiver provenance and suppresses unknown
selectors", `c805df8`). The F2 entry's Option-B acceptance criteria cannot
pass for Go while this path exists.

**Trigger surface (narrow but real):** (a) a call in the *condition part*
of an `if`/`switch` (not the body — body calls are protected by the block
byte check), (b) an init declaration whose name shadows an outer binding of
a different type, (c) a `:` inside a string literal or comment between the
declaration and the call. A comment with a colon triggers it identically.
Secondary direction (no outer binding): the inner declaration is not
recognized, so a *direct* call `s()` falls through to
`resolve_go_free` and can attach a same-named package-level free function —
another false edge; selector calls in this direction degrade safely to
`@bound-receiver`.

**All four lanes missed it** (blind-spots checked byte-scan false positives
but focused on the `identifiers_before` delimiters, which are correct — the
delimiters occur structurally before the RHS). Flagging explicitly: this
finding is primary-agent, proven by code trace + execution, with no lane
corroboration.

**Fix (small, AST-based):** the `;` separating init from condition is a
token in the tree. Replace the byte scan with: locate the
`short_var_declaration` (or `assignment_statement`) child of the scope,
then compare `call.start_byte()` against the byte of the following `;`
token (or, equivalently, the start byte of the condition child). No
`content` slicing at all. Then add a regression test generalizing the
scratch probe (string colon, comment colon, switch init, body-part control).

### F-2 — watch-mode resolver rebuild cost (minor, corroborated)

Each watched `.go` create/modify runs `prepare_resolution` over the
COMPLETE graph: sort/dedup all Go paths, `GoModuleModel::discover`
(ancestor walk with `go.mod.is_file()` per directory level for every file,
then `read_to_string` of every discovered manifest), disk-read + full
tree-sitter parse of every cache-loaded Go path missing metadata (once per
session), then rebuild all four maps. This happens while `index_lock` is
held, so it delays the next watch event (debouncer-dropped events).
Cost: fine at the `external/logrus` scale (~200 Go files); at a 10k-file Go
repo it is a real per-edit latency tax (tens of thousands of syscalls per
edit). Quality rated this major; blind-spots minor. Consensus here: **minor
follow-up** — no correctness impact, no observed failure — but it is the top
performance item: memoize the module model (or at least per-directory
`go.mod` presence) across prepare calls, invalidating on universe change.

### F-3 — marker tokens persist in Graph and cache (minor, proven)

The resolve loop (`indexer.rs` Calls arm) only rewrites `edge.to` when
`resolve_call` returns `Some`; unresolved calls are **retained verbatim** —
pre-existing behavior for all languages (the arm's own comment documents it
for bare tokens). New with this diff: retained Go tokens can be internal
markers (`@bound-receiver::Start`, `@dot-import::Func`). Verified exposure:

- **Query surfaces: none.** `Graph::is_resolved_node` requires the target
  to be a known node; BFS traversals (callers/callees/find_path/diagram)
  drop non-node targets. `community.rs:139` (`aggregate_file_edges`) skips
  edges whose target is not in `nodes`. Unresolved includes are dropped at
  resolve, not retained.
- **Persistence: yes.** `PackedEdge` serializes whatever `to` holds, so
  marker strings ride in the v12 cache, and the cross-scope
  never-re-resolve rule keeps them there until the file is re-parsed.
- **Doc drift:** `watch.rs:379-384` ("The existing graph's edges are
  already stored as resolved edge entries") overstates — it predates this
  diff, but marker retention makes the overstatement more visible.

Options: (a) document "unresolved call edges store their provisional token
(all languages; Go tokens may be internal markers, filtered at BFS time)";
(b) drop `@bound-receiver::` edges at resolve time — they are provably
never resolvable (`resolve_call` unconditionally returns `None` for that
prefix), unlike `@dot-import::`/`@method-receiver::` which can resolve.
Either is a follow-up; (a) alone is acceptable.

### F-4 — double clone of the cached universe (minor)

`run_analyze_job` builds `cached_snapshot` (a full clone of the cached
graphs), then `cached_snapshot.iter().filter(…).cloned().collect()` clones
the surviving graphs **again** into `resolution_graphs`. The snapshot is
unused after the index build (verified: nothing below `build_file_index`
reads it), so `into_iter().filter(…).collect()` moves instead of cloning
and halves the peak extra memory for scoped analyzes on large caches. The
`fresh_graphs.iter().cloned()` is unavoidable (mutable borrow of
`fresh_graphs` in the same `resolve_edges_with_indexes` call requires
separate allocations).

### F-5 — v12 rationale comment (minor)

`packed.rs` changelog: "v12: Go call and import resolution semantics
changed. Re-index rather than retain call edges resolved under the
package-agnostic fallback." The same commit range also changed
`extend_symbol_index` (de-genericized keys), which changes resolution for
any language whose method parents carry generic parameter lists — Rust
proven (the `fb4b01e` smoke fix), C#/Java latent (their method-parent
extraction currently yields bare names, verified against `csharp/lib.rs`
and `java/lib.rs` parent extraction — the plan-drift lane confirmed the
change is structurally language-agnostic but presently Rust-effective).
The bump is justified and correctly co-located with the behavior change in
`ecb3ca6`; the comment should name the second cause so a future v13
reasoner knows what v12 actually covered.

### F-7 — committed README ↔ untracked installer (minor)

`opencode-plugin/README.md` now says: "OpenCode discovers commands only
from its configured command directories; when using this repository's local
installer, it links these files into `~/.config/opencode/commands/`." The
installer exists only as untracked
`scripts/local-install-opencode-plugin.sh`, whose header (lines 6-7) claims
it is "deliberately UNTRACKED in code-graph-mcp (listed in
`.git/info/exclude`)" — `git check-ignore` reports **NOT-IGNORED** and
`.git/info/exclude` has no such entry, so on this machine the script's own
documentation is false and `git status` shows it as a pending untracked
file (one `git add -A` from making it into the repo). The script itself is
well-constructed (sanity gates, refuse-to-replace checks, verification
pass). Fix: pick a stance — commit it (it is referenced by committed docs)
or (i) make the exclude the header claims, and (ii) mark the README
sentence explicitly developer-machine-local.

### F-10 — corpus/MANIFEST stage mismatch (minor)

`corpus.rs` contains no resolution call (verified: zero `resolve*` /
`prepare_resolution` references). Its 36-edge contract is an
**extraction-stage** contract, but MANIFEST prose describes resolved
targets ("4 Calls: `init -> utils.Add` (from the aliased selector
`umath.Add`)"). They agree today because the five removed edges
(`Apply -> op`, `Map -> f`, `Filter -> pred`, `KV::Lookup -> ok`,
`withLog -> inner`) were dropped at **extraction** (lexical-binding
`continue`), but a future fix that drops an edge at resolve time (e.g. the
F-1 class, or F-3 option (b)) would change on-disk graph edges without
moving the corpus count — the regression contract silently weaker than its
prose suggests. Cheapest fix: one MANIFEST header line — "edge counts are
provisional extraction edges; resolution behavior is pinned by
`tests/go_resolution.rs`".

## Lane results and disagreements
- **Quality** (6 findings): F-1 n/a; F-2 (major→consensus minor), F-4,
  F-10, F-11, F-12. Independently verified the cold-cache staleness seam is
  a **documented** known limitation (CLAUDE.md "Cold-cache Go resolver
  metadata has an out-of-scope staleness seam") — the lane's "major"
  downgrades to "documented, accepted; F-12 pins it in tests".
- **Blind-spots** (3 proven findings + 9 negative checks): F-3, F-2, F-11.
  Actively cleared (no counterexample found, code-traced): metadata
  read-lock deadlock; production→test-helper visibility bypass; deleted or
  non-Go path during disk metadata load; path-form mismatch between
  `ctx.caller_file` and `state.files` keys; multi-module/broken-`go.mod`
  panics; search OR-matching false positives; de-genericized-key
  false-resolution (Go requires exactly one visible candidate — collisions
  omit rather than mis-pick). Did NOT clear `binding_scope_started` (F-1).
- **Spec-compliance** (8 findings): F-5, F-8, F-9, F-13, F-14, F-15, F-16;
  its "major" on the search_symbols description lens (F-15) is pre-existing
  and overstated — the diff only changed the `query` sentence, and the
  changed sentence itself passes the lens (verified against
  `pattern_matches_symbol`). Verified compliant: description↔code↔snapshot
  agreement; all three plugin trees byte-identical on changed commands and
  in sync (also `plugin-sync-check` green); no stale Go-resolution claims
  anywhere in the repo (grepped all trees); F2's Go-suppression claim
  matches code.
- **Plan-drift** (4 findings): F-6, F-7, F-13, plus a documentation-completeness
  point (package-level value-binding suppression is only partially covered
  by the "direct calls through locally bound function values/parameters are
  omitted" line — package-level `var` bindings are not "local"; folded into
  F-6's recommended doc line). Verified as-planned: F1/F3 fixes match their
  documented root causes with tests; turbofish follow-up documented
  (CLAUDE.md Rust section); `prepare_resolution` contract + all three call
  sites present; cache bump co-located with behavior; **F2 boundary intact**
  (no `Heuristic/1` emission, `CallContext` unchanged, no plumbing in the
  five generic-resolver crates — F2 is cleanly deferred).

**Disagreements surfaced, not silently resolved:** F-2 severity (quality:
major vs blind-spots: minor → consensus minor, top perf follow-up);
cold-cache staleness (quality: major vs primary: documented-accepted);
F-15 (spec: major vs primary: pre-existing nit).

## Verified correct (checked, no finding)
- **Concurrency:** `metadata`/`resolution` lock discipline is sound — parse
  threads write per-file metadata; `prepare_resolution` builds state
  lock-free, publishes via one write; the metadata read guard is scoped and
  released before the resolution write (no nested-lock cycle with
  `parse_file`); analyze awaits `index_lock`, watch `try_lock`s and drops
  contending events — single-flight holds.
- **`strip_generic_params`:** nested/constrained/unbalanced cases correct
  (unit-pinned); language-agnostic key registration cannot create
  false-positives in Go (uniqueness requirement) and accumulates candidates
  (FR-48 count) elsewhere.
- **`pattern_matches_symbol`:** strict superset of pre-fix matches
  (qualified target still tested for methods), short-name-first allocation
  skip, `count_only` parity, both graph-level and handler-level tests.
- **`package_visible` / `_test.go` rules:** internal tests see production
  but never the reverse; external `x_test` isolated by declared-package
  comparison; representatives exclude test files (blind-spots traced).
- **Cache coherence:** v12 bump in the same commit as the behavior change;
  all four CLAUDE.md version references updated; snapshot updated in the
  same commit as the description.
- **Unresolved-marker query safety** (F-3 boundary): no MCP surface exposes
  marker tokens; file-level aggregation skips non-node targets.
- **Plugin trees:** canonical/mirror parity for all changed commands;
  `code-graph.js` change consistent with the installer and with
  CLAUDE.md's hand-maintained/generated split.

## Recommended actions before push
1. **Fix F-1** (AST-based `binding_scope_started` + regression test).
   This is the only blocker-class item. If it must slip, it needs its own
   KNOWN_ISSUES entry — F2's framing ("confidently-wrong edges are worse
   than visibly-guessed ones") applies to it verbatim.
2. Fold in the cheap doc fixes while touching the tree: F-5 (v12 comment),
   F-6 (+ package-value-binding line), F-8 (two v11 references), F-9 (F2
   citations), F-10 (MANIFEST header line), F-14 (reword).
3. Decide F-7's stance (commit the installer or make its exclude claim true
   - mark the README sentence developer-local).
4. Queue follow-ups (not push-blocking): F-2 (module-model memoization,
   benchmarked), F-3(a) (document retained-token storage), F-4 (move
   instead of clone), F-11 (metadata prune), F-12 (seam-pinning tests).

## Resolution Log
The review was re-evaluated against current `HEAD` on 2026-08-22 and moved
from the orphan root-level `.plans/Reviews/` directory into this plan-owned
review directory. `Plans/RustSupportGaps` is the nearest owning plan: the
review range begins with its post-completion Rust smoke remediations, and the
Go resolver work was the concrete parser-specific response to the same
receiver-resolution defect class.

| Finding | Disposition | Rationale / action |
|---|---|---|
| F-1 | Fixed | Replaced token-blind `:`/`;` scanning with explicit tree-sitter boundaries for `if`, expression switch, `for`/`range`, type-switch aliases, and select receive cases. Calls on declaration RHSs retain the outer binding; calls after each construct's real scope boundary use the new binding, while `=` range/receive assignments continue using the existing variable. Added string/comment-colon, switch, loop, range, type-switch, select, and end-to-end false-`Resolved/1` regressions. Bumped cache v12→v13 so cached false edges cannot survive the fix. Final blind-spot review found the original report understated the affected constructs. |
| F-2 | Fixed in follow-up | `GoParser` now shares one module model between namespace rewriting and resolution, keyed by the active Go path universe plus a manifest epoch, and reuses derived resolution state when resolver-relevant metadata is unchanged. Ordinary body-only watch edits take that cached path. `go.mod` events bypass source-extension filtering and atomically rebuild all indexed Go files (D-0017); explicit analyzes invalidate the epoch, and failed manifest batches stay dirty for retry. Cache-identity tests pin the avoided rebuilds; watcher tests cover create/modify/remove, nested ownership, external importers, live dispatch, and failed-batch recovery. |
| F-3 | Tracked follow-up | Added a dedicated `KNOWN_ISSUES.md` entry with marker taxonomy, persistence/filtering evidence, staged opportunities, a selective `@bound-receiver::` recommendation, and acceptance criteria. |
| F-4 | Fixed | Consume `cached_snapshot` with `into_iter()` instead of cloning the cached graph universe a second time. |
| F-5 | Fixed | Expanded the v12 cache history to include de-genericized associated-call lookup keys as well as Go resolver semantics. |
| F-6 | Fixed | Documented package-level value suppression and asymmetric internal/external Go test visibility. |
| F-7 | Fixed without shipping the local script | Reworded the committed OpenCode README so it no longer promises a repository installer. Added the explicitly machine-local script to `.git/info/exclude`, matching its header. |
| F-8 | Rejected | The v11 references describe when `candidates` entered the cache format. They are historical field-safety references, not claims that the current cache version is v11; changing them to v12 would be less accurate. |
| F-9 | Fixed | Replaced fragile line-number citations with symbol-level references to the two Calls-resolution arms. |
| F-10 | Fixed | Corrected crate paths and explicitly labeled corpus edge counts/targets as provisional extraction output. |
| F-11 | Fixed | Prune parse metadata to the active Go path universe on every preparation pass and clear it for an empty universe; added unit coverage. |
| F-12 | Accepted limitation; stale-sibling test deferred | Cold-cache out-of-scope divergence is already an explicit contract requiring persisted metadata and another cache bump to eliminate. The F-2 follow-up now covers module-backed watch invalidation thoroughly, including nested manifests and external importers. A cold-restart stale-sibling characterization remains useful follow-up coverage; the production fix is still persistence of per-file Go resolver metadata. |
| F-13 | No action | Rewording existing commit subjects requires history rewriting and adds no product value. Future commits should continue using behavior-specific subjects. |
| F-14 | Fixed | Reworded Go call resolution to name the package-aware resolver rather than a nonexistent generic fallback. |
| F-15 | Fixed | Documented `near=false`, `max_distance` defaults/ceiling, `count_only=false`, and `subtree` behavior in the production tool description and schema; updated the tool-list snapshot. |
| F-16 | Fixed | Updated smoke-test cache instructions to the project-root rkyv `.code-graph-cache.db`, clarified that no response cache-hit flag exists, corrected stale crate/binary paths, and stated that legacy JSON caches are ignored. |

Verification after the fixes:

- `cargo test -p code-graph-lang-go --lib`: 91 passed.
- `cargo test -p code-graph-tools --test go_resolution --test watch_go_reindex`: 16 passed.
- `cargo test -p code-graph-graph persist::tests::load_version_mismatch_returns_false`: passed.
- `cargo clippy -p code-graph-lang-go -p code-graph-tools -p code-graph-graph --all-targets -- -D warnings`: passed.
- `make test`: passed after rerunning with a timeout long enough for daemon integration tests.
- `make lint`, `make fmt-check`, `make snapshot-clean`, `make plugin-sync-check`: passed.
- `sdd doctor --check --json` and `git diff --check`: passed.

The original blocker and F-2 follow-up are resolved. F-12's cold-cache
metadata persistence remains non-blocking follow-up scope for the reasons
above.
