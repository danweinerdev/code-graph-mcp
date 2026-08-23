---
title: "Code Review: Go Resolver Hardening and Cache v13 Change Set"
type: review
tags: [review, go-resolver, watch, cache-v13, cli]
related: [KNOWN_ISSUES.md, Decisions/decisions.md, Plans/RustSupportGaps/reviews/05-shared-process-adversarial-review-57d6ee5.md]
review_of: "Plans/RustSupportGaps"
rev: "57d6ee5c52b13de7a710b1822d42212e7c437c34..bfb0bc6ae4b887177adec34260023cc391dd2ef7"
reviewed_range: 57d6ee5..HEAD (bfb0bc6)
review_mode: four independent lanes + primary verification
verdict: Approve with follow-ups
---

# Code Review: Go Resolver Hardening and Cache v13 Change Set

**Reviewed range:** `57d6ee5..bfb0bc6` — 5 commits, 24 files, +2729/−269,
endpoint `bfb0bc6`. This is the first full-range adversarial review of the
range; the post-fix spot reviews recorded in the prior document's Resolution
Log are not this pass.
**Review mode:** Four independently-dispatched lanes (quality, blind-spots,
spec-compliance, plan-drift) with input isolation, plus primary-agent
verification (full `make verify` run, direct code traces, grammar-field
verification against the pinned tree-sitter-go 0.25.0 `node-types.json`,
CLI/decode error-path traces). No lane was passed plan/spec material it was
not entitled to (quality and blind-spots saw diff + code only and were
forbidden from reading `.plans/`). Lane sandboxes denied `cargo test`
execution, so lane findings are code-trace evidence; the primary agent
re-verified every load-bearing claim before consolidation.
**Verdict: Approve with follow-ups** — no blocker-class defects. One
documented retry-contract break (F-1) and one documented CLI exit-class
violation (F-2), both with narrow triggers and self-healing consequences,
plus a cluster of minor/nit items. None blocks push; F-1/F-2/F-3 are the
worthwhile near-term fixes because they live in the same two files.

## Change-set summary
| Commit | Content |
|---|---|
| `ef52f26` | F-1 fix from the prior review: token-blind `:`/`;` byte-scan replaced by tree-field-based `control_binding_is_active` (if/switch `initializer`, type-switch `alias`, `for_clause`/`range_clause`, `communication_case`); **CACHE_VERSION 12→13** with rationale comment; v12 comment expanded (prior F-5); `GoFileMetadata` → core `ResolverMetadata`; plugin-owned `module_model` cache (sorted-path-universe + manifest-epoch key) shared by `post_index` and `prepare_resolution`; `GoResolutionCache` keyed by (paths, metadata snapshot, module key) with body-only-edit reuse; five new `LanguagePlugin` hooks; `try_reindex_go_manifest` atomic all-Go rescan (D-0017); `process_event_batch` manifest-first restructure + retry gate; D-0016 (superseded) + D-0017 recorded; CLAUDE.md/MANIFEST/corpus/plugin-cmd doc updates; 14 new unit tests (8 scope-boundary, 5 module/resolution cache-identity, 1 metadata-prune) + 3 integration tests |
| `8f0fad3` | `search_symbols` description + `near` schema: documents `subtree`, `near=false`, `max_distance` adaptive default/ceiling, `count_only=false` (prior F-15); tools-list snapshot updated; SMOKE_TEST crate-name fixes + cache-hit expectation rewrite (prior F-16) |
| `fa411ed` | OpenCode README installer sentence reworded to "local installations must link or copy" (prior F-7) |
| `c5c9a45` | KNOWN_ISSUES: F2 citations made symbol-level, future bump 12→13 renumbered to 13→14, new F3 entry (marker persistence); prior review's Resolution Log added |
| `bfb0bc6` | F-12 from the prior review: sparse Go resolver metadata persisted in a framed extension appended after the unchanged v13 rkyv root (footer: `main_len`, `extension_start`, magic `CGMETA01`); `Graph.resolver_metadata` PathTrie with merge/remove/subtree/clear lifecycle; analyze fast-path gated on `files_missing_resolver_metadata`, plugin restore on both fast and slow paths, footerless-v13 migration via new `index_directory_with_extra`/`index_files`; watch per-file and manifest-batch metadata publication; 1 new unit test (hook clone/restore/sort), 4 new integration tests (both stale-sibling directions, footerless migration, fast-path hydration), 3 new persist-format tests; the pre-existing `go_scoped_analyze_uses_cached_sibling_package_bindings` test strengthened with a post-cache-write divergence scenario |

Working tree is clean (`git status` empty). The machine-local
`scripts/local-install-opencode-plugin.sh` is present, and its header's
`.git/info/exclude` claim is TRUE on this machine (`.git/info/exclude:23`,
`git check-ignore` exit 0) — prior F-7 fully resolved.

## Verification (actual results, 2026-08-22)
- `make verify` — **PASS end to end** on `bfb0bc6`: clippy
  `--workspace --all-targets -- -D warnings`, `cargo fmt --all --check`,
  `cargo test --workspace` (full suite, zero failures), `snapshot-clean`
  (no pending snapshots), `plugin-sync-check` (mirrors in sync). The final
  gate line prints only after all five sequential steps succeed
  (`Makefile:43-54`).
- Grammar-field verification: every field the F-1 fix relies on exists in
  the pinned tree-sitter-go 0.25.0 `node-types.json` — `if_statement`
  fields `{alternative, condition, consequence, initializer}`,
  `expression_switch_statement` `{initializer, value}`,
  `type_switch_statement` `{alias, initializer, value}`,
  `for_statement` `{body}`, `for_clause` `{condition, initializer,
  update}`, `communication_case` `{communication}`; `:=` is a direct
  (anonymous) child of `range_clause`/`receive_statement`, matching
  `has_immediate_child(node, ":=")`.
- Error-path traces (primary): `DecodeError → PersistError::CorruptedCache`
  (`packed.rs:866-872`); `Graph::load → load_and_stale → with_validated_archive`
  propagates the decode error as `Err` (not `Ok(false)`) for the two new
  metadata variants; CLI `bootstrap` maps `Err` → `CliError::Operational`
  (exit 2) (`cli/exec.rs:114-126`). `max_distance_for_query` unit-pinned at
  `0..=1 => 0` (`handlers/symbols.rs:224-231` + test at `:1653-1661`).
  `git show ef52f26` confirms the v12→v13 bump and both changelog edits
  landed in that commit; `bfb0bc6` added the extension without a bump.

## Findings
| # | Severity | Source lens | Location | Issue | Recommendation |
|---|---|---|---|---|---|
| F-1 | **minor** (both lanes rated major) | quality + blind-spots (independently, different consequences; primary trace) | `watch.rs:183-203` (gate), `:552-556` (invalidate-before-IO), `lang-go/lib.rs:938-946` (`resolution_cache_invalidated`), `:233-262` (`module_model_for_paths`), `watch.rs:369-382` (all-plugin prepare) | A failed go.mod transaction's "dirty" state exists only as a stale module-cache epoch, and ANY successful `prepare_resolution` consumes it — including one triggered by a NON-Go reindex, which runs `post_index`+`prepare_resolution` for every plugin over the full graph. The documented "failed manifest batches stay dirty for retry" guarantee (watch comment `watch.rs:183-188`, prior-review Resolution Log F-2 row, D-0017 intent) is therefore broken two ways: (a) with no later Go event the old module identity persists indefinitely with no retry; (b) after a non-Go edit consumes the flag, the next Go edit takes the ordinary single-file path — the edited file's symbols get rewritten against the NEW module model while every sibling keeps OLD namespaces in the graph | Track transaction dirtiness separately (e.g. an `AtomicBool`/state set by `invalidate_resolution_for_path`, cleared only after a SUCCESSFUL `try_reindex_go_manifest` publication); the gate checks that flag, not entry-epoch staleness. Add a regression inserting a non-Go reindex between a failed manifest transaction and the next Go event |
| F-2 | **minor** (quality rated major) | quality (primary verified the mapping) | `persist/mod.rs:387-397` (`decode_archived ?` in the `with_validated_archive` closure), `packed.rs:744-755,866-872`, `cli/exec.rs:114-126` | A structurally-valid but semantically inconsistent metadata extension (one entry's path ID pointing at a non-Go or absent file) fails `decode_archived` with `MetadataPathNotIndexed`/`MetadataLanguageMismatch`, which propagates as `Err(CorruptedCache)` from `Graph::load`. The CLI maps that `Err` to `Operational` (exit 2), but the documented contract — CLAUDE.md CLI section ("A readable-but-corrupt or version-mismatched cache is 'not present' → honest-unindexed → 1, NOT 2") and the call-site's OWN comment (`exec.rs:115-118`: "Ok(false) covers … readable-but-corrupt caches") — classifies it as unindexed (exit 1). MCP/analyze is unaffected: `load_and_stale(...).unwrap_or((false, Vec::new()))` swallows the `Err` and self-heals by re-indexing | Classify the two metadata `DecodeError` variants as extension-absent (`Ok(None)` for the extension, keep the intact main archive) — strictly better than whole-cache rejection, because `files_missing_resolver_metadata` + the existing migration path already re-parse the Go files and rewrite the extension. Add corruption tests pinning the classification for invalid path/name IDs and language mismatch |
| F-3 | minor | blind-spots | `graph.rs:579-583` (`set_resolver_metadata`) | The public API checks `files.contains_path` but NOT the file's language: `set_resolver_metadata(non_go_path, ResolverMetadata::Go{..})` is accepted, the encoder serializes it, and the decoder then rejects the whole cache on load (`MetadataLanguageMismatch`) — a legal mutation sequence producing an unloadable cache, and a second independent route into F-2 | Enforce language compatibility in `set_resolver_metadata` (check `FileEntry.language`, or return `Result`); the decoder's existing validation then stays defense-in-depth |
| F-4 | minor | spec (primary verified) | `server.rs:1497` (tool description) + `:921-923` (`near` schema) | The NEW `max_distance` text says "1 edit at length 2-11, 2 at 12-17, 3 at 18+" but the code defaults lengths 0–1 to 0 (`max_distance_for_query`, unit-pinned `max_distance_for_query(1) == 0`), and a one-character query is a valid plain identifier in `near` mode — so the documented default omits real behavior for the shortest queries | State the full adaptive range in both strings ("0 at length ≤ 1, 1 at 2–11, 2 at 12–17, 3 at 18+"); update the tools-list snapshot in the same commit |
| F-5 | minor | plan-drift (primary verified) | `.plans/Decisions/decisions.md` D-0017 rationale | "A conservative all-Go rebuild is the smallest correctness-preserving watch behavior **until reverse manifest dependency metadata exists**" — that dependency is not tracked anywhere (no KNOWN_ISSUES entry, no plan task; the only repo occurrence is D-0017 itself) | One KNOWN_ISSUES entry (or plan task) naming the future metadata and its intended replacement of the all-Go rebuild, or drop the clause |
| F-6 | nit | plan-drift | commit `ef52f26` | Subject "harden resolver state across watch edits" under-describes: the commit also carries the F-1 scope-resolution fix, the v12→v13 cache bump, the five trait hooks, and D-0016/D-0017 | Future subjects name the cache-format/scope behavior, not only the watcher |
| F-7 | nit | primary | `watch.rs:826-840` (`go_rescanned` skip) | A `.go` write that lands between a successful manifest rescan's disk read and the `has_file` skip check is consumed without re-reading the file: the comment's "Any write racing after this rescan produces a subsequent filesystem event" does not cover a write whose event is ALREADY in this batch — the graph keeps pre-write bytes until the next edit to that file (millisecond window; requires a go.mod event in the same debounce batch) | Optional: when skipping a path that has an event in the batch, re-read it through the ordinary path; or note the window in the comment |
| F-8 | nit | primary | `lang-go/lib.rs:233-262`, `indexer.rs:246` | Within one ANALYZE pass the module-model cache can never hit: `post_index` (invoked from `index_files` over the FRESH set only) keys the entry by the fresh Go paths, then `prepare_resolution` (full universe in `resolve_edges_with_indexes`) misses and rebuilds — so every analyze performs two `GoModuleModel::discover` runs. Watch pays zero (both calls see the full universe → hit). Not a regression — the pre-diff code also discovered twice — and the F-2 watch-latency goal is met | If analyze-time discovery ever matters: per-directory `go.mod`-presence memo inside the model (the original F-2 recommendation) instead of whole-universe keying |

## Detailed analysis
### F-1 — failed manifest transaction dirtiness is consumed by non-Go edits (minor, both lanes + primary trace)

**Mechanism (traced).** `try_reindex_go_manifest` deliberately runs
`invalidate_resolution_for_path(manifest_path)` BEFORE any fallible IO
(`watch.rs:552-556`), so a batch that fails during read/parse leaves the
epoch advanced with no publication. Dirtiness is then OBSERVED exclusively
through `resolution_cache_invalidated()` (`lang-go/lib.rs:938-946`), which
is true only while the module-cache entry's epoch lags `manifest_epoch`.
But the epoch is also the cache KEY: any `prepare_resolution` call that
misses the key rebuilds the entry at the current epoch and thereby "heals"
the observation. The per-file watch path runs `post_index` +
`prepare_resolution` for **all** plugins over the **full** graph
(`watch.rs:369-382`) regardless of the edited file's language, so a C++
save after a failed go.mod transaction rebuilds the Go module entry at the
new epoch without publishing any Go graph update.

**Consequence (a) — no-event staleness.** If no further event arrives, the
graph keeps the pre-go.mod namespaces/edges indefinitely; the only signal
is an `eprintln!` breadcrumb in daemon stderr. Softer consequence, same
root.

**Consequence (b) — mixed module identities.** The next Go edit passes the
gate (flag consumed) and takes the ordinary path: `post_index` rewrites
ONLY `new_fg`'s namespaces in place (the snapshot clones of sibling files
are discarded at merge), so the edited file publishes under the new module
identity while every sibling retains old namespaces. Query-visible via
`search_symbols(namespace=…)`, `get_symbol_summary` grouping, and stale
namespace text on sibling symbols. **No false edges**: symbol IDs are
`path:name` (namespace-free) and stored edges are path/symbol-id based, so
the mix is stale-metadata, not mis-resolution — which is why the primary
agent downgrades both lanes' "major" to "minor" (bounded, self-healing on
the next go.mod event or analyze, narrow trigger: transient I/O failure
during a manifest transaction). The documented retry contract is still
broken, and the fix is small: a dedicated dirty flag set on invalidation
and cleared only on successful publication, checked by the gate.

**Why the trait doc papers over it.** `resolution_cache_invalidated`'s
trait comment (`code-graph-lang/src/lib.rs:577-583`) defines consumption as
"a successful preparation pass" — which a non-Go reindex's prepare IS. The
watch module comment and D-0017 intend consumption = "the all-Go
transaction completed." Reconcile the two by making the flag
transaction-scoped.

### F-2 — semantic metadata decode failure exits the CLI as operational (minor, quality + primary trace)

`decode_archived` is called inside the `with_validated_archive` closure
with `?` (`persist/mod.rs:387-389`), so a `DecodeError` becomes
`Err(PersistError::CorruptedCache)` from `Graph::load` — unlike structural
rkyv validation failures, which `with_validated_archive` itself maps to
`Ok(None)` ("not present"). Of the seven `DecodeError` variants, five
predate this diff (same exposure class, rarer triggers); the diff adds
`MetadataPathNotIndexed` and `MetadataLanguageMismatch`, which fire
whenever a metadata entry's interner ID survives rkyv validation but
references the wrong kind or an absent file — a plausible single-field
corruption shape for the new extension, and also reachable via F-3's public
API.

Classification impact, by front-end:
- **MCP/analyze:** `load_and_stale(...).unwrap_or((false, Vec::new()))`
  (`core/analyze.rs:274-276`) → silent full re-index. Correct and
  self-healing.
- **CLI:** `bootstrap` maps `Err` → `CliError::Operational` (exit 2)
  (`cli/exec.rs:114-126`) — contradicting FR-20 as documented in CLAUDE.md
  and the comment two lines above the mapping. The read-only CLI cannot
  re-index, so the user gets an operational failure instead of the
  honest-unindexed signal that the daemon-attach fallback and agent
  heuristics key on.

The better fix is at the loader, not the CLI: a semantically-bad extension
should load as extension-absent. The main archive is intact, and the
change set already ships the exact recovery path —
`files_missing_resolver_metadata` flags the Go files, the migration
re-parses them, and the save rewrites the extension (prior F-12's
footerless-upgrade machinery, test-pinned by
`footerless_v13_go_cache_upgrades_during_scoped_resolution`). Whole-cache
rejection (the current `Err` on the MCP side) is a strict superset of
that work. Note the loader ALREADY does the right thing for the two
other extension failure modes — unsupported version and rkyv access
failure both return `Ok(None)` (`persist/mod.rs:501-525`); only the
decode-stage semantic errors take the `Err` route.

### F-3 — `set_resolver_metadata` accepts the wrong language (minor)

One-line gap: the guard is `self.files.contains_path(&path)` with no
`FileEntry.language` check. No production caller misuses it (all three
call sites pass Go metadata only for Go paths), so this is latent — but it
is the only route to F-2's classification violation that does not require
disk corruption, and it is a `pub` API on the graph type.

### F-4 — `max_distance` documentation omits the length ≤ 1 tier (minor)

New text in both the tool description and the `near` schema
("1 edit at length 2-11, 2 at 12-17, 3 at 18+") vs
`max_distance_for_query`'s `0..=1 => 0`. A 1-char `near` query is legal
(`is_plain_identifier` accepts single characters) and behaves as exact
matching, which the description never says. This is the first
documentation of these defaults (prior F-15 asked for the documentation),
so it lands as an incomplete fix of that finding rather than a
pre-existing gap.

## Lane results and disagreements
- **Quality** (2 findings, 5 negative checks): its major on the failed-
  manifest retry (→ F-1) independently found the non-Go-consumption
  consequence; its major on the CLI classification (→ F-2) was verified by
  primary trace end-to-end (error variant → `PersistError::CorruptedCache`
  → `CliError::Operational`). Cleared: footer bounds/alignment before
  slicing; interner ordering (metadata strings appended after main
  interning, so the main archive's structure is undisturbed); metadata
  lifecycle cleanup across merge/remove/subtree/clear; successful manifest
  publication atomicity; control-binding rewrite by inspection + unit
  coverage. No test execution available in its sandbox.
- **Blind-spots** (2 findings, 10 negative checks): its major (no-event
  staleness after a failed rescan) is the same root as F-1 with the softer
  consequence; its minor (→ F-3) verified. Cleared with reasons:
  concurrency (no lock-order cycle — module mutex never held while
  acquiring metadata/resolution locks; in-flight readers hold `Arc`
  clones; analyze/watch serialize on `index_lock`); mixed watch batches
  (created/removed Go files correctly fall through the `go_rescanned` skip
  because `has_file` is false for them); publication window (queries see
  old or complete state, never the remove/merge intermediate); retry
  synthetic-manifest path (universe-wide rescan makes the synthesized
  parent directory irrelevant for nested modules); framing corruption
  (all truncation/zero-fill/corruption inputs degrade to re-index, no
  slice panic — padding check bounds the slice; a footerless cache whose
  archive ends with the 8-byte magic is misclassified and needlessly
  re-indexed, 2⁻⁶⁴ class, conservative); metadata-removal invariants
  (every mutator keeps `metadata ⊆ indexed Go files` internally — F-3 is
  the public-API exception); Windows path forms (canonicalized at the
  watch boundary; pre-existing verbatim-UNC/casing limitations unchanged);
  restore ordering (before parsing in both analyze paths; non-Go plugins
  no-op); F-1 scope boundaries (RHS, update part, type expression, range
  RHS, nested control, labels — bound receivers degrade to unresolved
  markers, not `Resolved/1` false edges); long-daemon memory (prepare
  prunes to the active universe on every pass).
- **Spec-compliance** (1 finding, 13 verified-compliant): F-4. Verified:
  `subtree`/`near`/`count_only` description claims match
  `core/symbols.rs`; snapshot byte-consistent with the new description;
  Go metadata sorted+deduped before storage (`lang-go/lib.rs:269-280`);
  restore on both analyze paths; v13 changelog comment names both causes
  (scope fix + extension); no stale current-version `v12` references in
  governing docs, plugin commands, or smoke docs; F2 symbol citations real;
  F3 entry code-accurate including the `@bound-receiver::` terminal
  predicate (`resolve_call` returns `None` for that prefix before any other
  branch, `lang-go/lib.rs:894`); package-value suppression and test-
  visibility bullets match code; SMOKE_TEST mtime/sweep/legacy-JSON
  wording matches the fast path; the three `cg-index.md` files
  byte-identical; `.code-graph.toml.example` clean.
- **Plan-drift** (2 findings, full as-planned verification): F-5, F-6.
  Verified as-planned against the prior review's Resolution Log: F-1 (no
  residual `binding_scope_started`/`control_header_binds`; v13 bump in
  `ef52f26` with rationale naming the scope change; all claimed regression
  tests present — unit at `lang-go/lib.rs:2313-2459`, end-to-end
  `go_control_initializer_shadowing_never_resolves_the_outer_receiver`);
  F-2/D-0017 (Arc-identity pin at `lang-go/lib.rs:1828-1845`; manifest
  events processed before extension filtering, `watch.rs:795-825`;
  universe-wide rescan `watch.rs:560-575`; invalidate-before-IO; analyze
  epoch invalidation `core/analyze.rs:530-532`); D-0016/D-0017 (final code
  matches D-0017's whole-universe behavior; D-0016 was recorded and
  superseded in the SAME commit — accurate status, though it records a
  design that was never deployed); F-12 clause-by-clause (`PackedCacheV6`
  field list unchanged at `57d6ee5` vs `bfb0bc6`; non-Go no-extension
  bytes pinned by `packed_non_go_graph_has_no_sparse_metadata_table`;
  footerless fast-path bypass `core/analyze.rs:277-287`; migration via
  `migration_files`/`index_directory_with_extra` including ignored/
  out-of-scope entries; all four named compatibility test families found);
  F-3/F-4/F-5/F-6/F-9/F-10/F-14/F-15/F-16 all verified; F-7 README
  reworded, script's exclude claim verified true on this machine.

**Disagreements surfaced, not silently resolved:** F-1 severity (quality:
major, blind-spots: major, primary: **minor** — impact is stale namespace
metadata with self-healing and no false edges; the retry-contract break
remains and the fix is still recommended); F-2 severity (quality: major,
primary: **minor** — rare corruption shape, CLI-only, MCP self-heals, no
wrong data; still a documented FR-20 violation with a cheap loader-side
fix). The prior review's post-fix note that independent reviews "found no
remaining actionable findings" covered the compatibility/alignment fixes
in isolation; this full-range pass surfaces F-1/F-2/F-3 as new.

## Verified correct (checked, no finding)
- **F-1 scope fix (prior review's blocker):** all field names exist in the
  pinned grammar; the `AsAny` unit test proves the type-switch
  `initializer` field spans the full short-var-decl (so a call inside the
  type expression correctly sees the OUTER binding, matching Go's
  "scope begins at the end of the ShortVarDecl" rule); assignment-form
  range/receive (`for s = range …`, `case s = <-…`) correctly keep the
  existing receiver via the immediate-`:=` discriminator; string/comment-
  colon regressions present at both unit and end-to-end level; the v13
  bump co-locates with the behavior change so cached false edges cannot
  survive.
- **Cache extension format:** footer validation order prevents every
  slice-out-of-range (len ≥ 24 gate, `main_len`/`extension_start` bounds,
  alignment re-check, zero-padding check before the padding slice);
  extension reuses the root interner with decode-time indexed-path and
  language validation; non-Go caches pay zero framing bytes (test-pinned);
  pre-extension v13 readable (test-pinned); save is atomic (temp+rename),
  so a torn extension cannot appear from a crashed writer.
- **`PackedCacheV6` / `PackedFile` / `FileGraph` byte-layout** unchanged
  across the range (verified against `git show 57d6ee5:…/packed.rs`) — the
  extension is purely additive after the root archive, as the v13 comment
  and CLAUDE.md claim.
- **Concurrency:** no nested-lock cycle among the three Go cache domains;
  in-flight resolvers hold `Arc<GoResolutionState>` clones that survive
  cache replacement; poison recovery (`into_inner`) consistent across all
  new lock sites; the all-Go rescan holds `index_lock` across its
  `spawn_blocking` and publishes under one graph write (queries observe
  old-or-new only); the re-check of `ensure_daemon_root_current` before
  publication after the blocking phase is correct.
- **Metadata lifecycle:** `merge_file_graph` clears a path's prior
  metadata (fresh-merge-then-set ordering verified at all three call
  sites: analyze persistence loop, watch per-file, manifest batch);
  `remove_file`/`remove_files_under`/`clear`/sweep all remove metadata
  (graph unit test `sparse_resolver_metadata_follows_merge_remove_subtree_
  and_clear` pins all five).
- **Restore ordering:** both analyze paths restore BEFORE parsing, so fresh
  parses overwrite their own paths rather than being clobbered (the slow
  path's restore at `core/analyze.rs:497-505` sits above the
  `index_directory_with_extra` call at `:535+`); the fast path's restore
  is what makes watch edits after a fast-path analyze resolve correctly
  (`cache_fast_path_hydrates_go_metadata_before_watch_edit` pins it).
- **Footerless-v13 migration:** fast path gated on
  `files_missing_resolver_metadata` (unchanged caches still upgrade —
  `footerless_v13_go_cache_bypasses_unchanged_fast_path_for_upgrade`);
  migration re-parses ignored/out-of-scope Go files through
  `index_directory_with_extra` and re-saves the extension immediately;
  both stale-sibling divergence directions pinned
  (`go_scoped_analyze_keeps_cached_function_when_ignored_disk_file_becomes_
  value`, `go_scoped_analyze_uses_cached_sibling_package_bindings`).
- **Watch go.mod dispatch:** manifest events bypass source-extension
  filtering and are processed before per-file events in the batch; one
  rescan covers multiple manifests in a batch (reads all manifests from
  disk); go.mod removal routes to the no-module fallback; the external-
  importer enable/disable cycle is end-to-end pinned
  (`go_mod_create_modify_remove_rebuilds_go_universe`), including nested-
  module ownership transfer and the failed-transaction retry.
- **Docs:** CLAUDE.md "Go resolver metadata is cache-coherent" bullet
  verified clause-by-clause against code; "Go watch resolver caching"
  bullet matches D-0017's implemented behavior; the Go "Supported" bullets
  (package-level value suppression; asymmetric test visibility) match the
  resolver; MANIFEST's provisional-extraction label present; SMOKE_TEST
  crate-name and cache-path fixes accurate; README no longer promises a
  repo installer; the tools-list snapshot matches the new description.

## Recommended actions
1. **Fix F-1** — transaction-scoped dirty flag (set on
   `invalidate_resolution_for_path`, cleared only after a successful
   `try_reindex_go_manifest` publication); gate checks the flag; add the
   non-Go-edit-between-failure-and-next-Go-event regression. This is the
   only consistency item; the rest are cheap.
2. **Fix F-2 + F-3 together** — loader classifies the two metadata
   `DecodeError` variants as extension-absent (main archive intact; the
   existing migration path recovers), and `set_resolver_metadata` enforces
   the Go-language guard. Add corruption tests pinning the load
   classification for bad path/name IDs and language mismatch.
3. **Fold in F-4** while the description is fresh — one sentence in both
   the tool description and the `near` schema, snapshot updated in the
   same commit.
4. **Track F-5** — a one-line KNOWN_ISSUES entry (or plan task) for the
   reverse manifest-dependency metadata D-0017 defers to.
5. **Nits, optional:** F-7 (re-read or document the same-batch skip
   window), F-8 (per-directory `go.mod` memo if analyze-time discovery
   ever matters), F-6 (process note for future subjects).

## Resolution Log
Actionability was re-checked against `bfb0bc6` by tracing the current watch,
Go resolver, packed-cache, CLI bootstrap, and tool-description paths. The
review is directionally correct, with two material qualifications: F-2's
recommended recovery set is too narrow, and F-7's proposed race is not
possible under the fixed-batch event ordering.

| Finding | Disposition | Resolution / rationale |
|---|---|---|
| F-1 | **Actionable** | The retry-contract break is real. A failed manifest transaction advances the Go epoch, but a later non-Go reindex runs Go `post_index`/`prepare_resolution` over the full graph and can consume the only observable invalidation without publishing refreshed Go graphs. Add transaction-scoped manifest-dirty state that survives unrelated preparation and is cleared only after successful all-Go publication. Regress: failed manifest transaction -> non-Go reindex -> Go edit, then assert the whole Go universe uses the new module identity. |
| F-2 | **Actionable, broadened** | Extension-local semantic corruption must degrade to "metadata absent" while preserving the separately validated main archive, so standalone queries can use the intact graph and the existing analyze-time migration can repair missing metadata. The fix must cover not only `MetadataPathNotIndexed` and `MetadataLanguageMismatch`, but also extension-local `PathOutOfRange` and `NameOutOfRange` failures. Main-archive ID failures must remain hard `CorruptedCache` errors. Separate main and extension decode/error context, then test invalid extension path IDs, invalid package/binding name IDs, absent indexed paths, and non-Go paths. |
| F-3 | **Actionable with F-2** | `Graph::set_resolver_metadata` can currently create an unloadable cache through a legal public call. Preserve its existing no-op API shape, but accept `ResolverMetadata::Go` only when the indexed `FileEntry` is Go. Add a graph/save/load regression proving metadata for a non-Go path is ignored and the cache remains loadable. |
| F-4 | **Actionable** | The production description and `near` schema omit the implemented `0`-edit default for a one-character query; an empty near query is rejected. Document the valid full tiers in both strings and update the tools-list snapshot. |
| F-5 | **Actionable tracking** | D-0017 names reverse manifest-dependency metadata as the condition for replacing conservative all-Go rebuilds, but no issue or plan task tracks it. Add a `KNOWN_ISSUES.md` entry describing the missing metadata, current all-Go cost, and replacement criterion; retain the decision rationale. |
| F-6 | **No current code action** | The historical commit subject under-described its scope, but correcting it would require history rewriting with no product benefit. Keep this as a process note for future commit subjects. |
| F-7 | **Rejected** | `process_event_batch` operates on an already-materialized immutable event vector. An event already in that vector predates the manifest rescan and its write is included in the rescan's disk read; a write after that read necessarily arrives as later watcher input. The alleged "post-read write whose event is already in this batch" ordering cannot occur. Do not add a redundant second per-file parse. |
| F-8 | **Optional optimization; no action now** | The observation applies only when slow scoped analyze post-indexes a fresh Go subset and resolution later prepares a larger cached-plus-fresh universe. Full/same-universe analyzes reuse the module model, so "every analyze" is false. The second discovery is semantically correct and pre-existing; optimize only if profiling shows material analyze-time cost, preferably by memoizing manifest/ancestor discovery without weakening the universe key. |

Implementation priority:

1. F-1, because it is the only live consistency/retry-contract defect.
2. F-2 and F-3 together, because the API guard removes the non-corruption path and extension-local recovery restores the CLI contract.
3. F-4 and F-5 as small documentation/tracking follow-ups.

F-6 and F-8 do not warrant implementation work now. F-7 should not be
implemented.

## Implementation Evidence

F-1 through F-5 were implemented and verified in the working tree after the
disposition pass above. F-6 and F-8 remain no-action items, and F-7 remains
rejected for the fixed-batch ordering reason already recorded.

| Finding | Implemented resolution | Regression evidence |
|---|---|---|
| F-1 | Added transaction-scoped Go manifest pending state to `GoParser`, exposed an object-safe publication commit hook on `LanguagePlugin`, and clear the pending state only after `try_reindex_go_manifest` publishes and prunes the complete all-Go replacement graph. Explicit analyze invalidation advances derived-cache epochs without creating a watch transaction. Root-recheck, read, parse, and resolution failures leave the manifest transaction pending. | `manifest_invalidation_stays_pending_until_publication_commit`; `go_mod_create_modify_remove_rebuilds_go_universe` now exercises failed manifest rescan -> successful Rust reindex (including Go preparation hooks) -> ordinary Go event, and proves both Go files move atomically from module v4 to v5. |
| F-2 | Split main-archive semantic decoding from optional resolver-metadata decoding. Extension-local path/name reference failures, missing indexed paths, language mismatches, unsupported extension versions, and extension bytecheck failures now discard only metadata with an `eprintln!` breadcrumb; the independently valid main v13 graph still loads. Main-archive semantic ID failures remain `PersistError::CorruptedCache`. `CACHE_VERSION` remains 13 and the packed root/file layouts are unchanged. | `metadata_extension_out_of_range_path_id_is_discarded`; `metadata_extension_out_of_range_declared_package_id_is_discarded`; `metadata_extension_out_of_range_binding_name_id_is_discarded`; `metadata_extension_path_not_in_files_is_discarded`; `metadata_extension_non_go_file_path_is_discarded`; `metadata_extension_unsupported_version_is_discarded`; `metadata_extension_failed_bytecheck_is_discarded`; `main_archive_semantic_id_failure_remains_corrupted_cache_error`. |
| F-3 | `Graph::set_resolver_metadata` remains a no-op API for invalid input but now accepts Go metadata only for an indexed `Language::Go` file. Unknown and non-Go paths are ignored before persistence. | `resolver_metadata_accepts_only_indexed_go_files`; `non_go_resolver_metadata_is_ignored_and_cache_remains_loadable`; existing sparse metadata round-trip remains green. |
| F-4 | Updated both the `max_distance` schema and production `search_symbols` description to state the valid adaptive tiers: 0 edits at length 1, 1 at 2-11, 2 at 12-17, and 3 at 18+, while explicitly retaining the non-empty near-query requirement. Updated the tools-list snapshot. | `snapshot_tools_list__tools_list_search_symbols.snap`; all 33 tools-list snapshot/description tests pass. |
| F-5 | Added `.plans/KNOWN_ISSUES.md` F4, documenting the current all-Go watch cost, why directory/existing-edge narrowing is unsound, the reverse manifest-dependency metadata replacement criterion, and correctness/performance acceptance tests. | Documentation read-cold review found the entry self-contained and consistent with D-0017. |

Additional adversarial review after implementation found and closed two nearby
contract gaps: bytecheck-invalid, unsupported-version, or semantically
inconsistent optional metadata extensions now retain the valid main archive,
and the trait/tool prose now matches publication and empty-query behavior
exactly. Malformed extension framing still rejects the cache before the main
archive boundary can be trusted. The existing watch contention policy and failed-
transaction liveness model were not broadened: ordinary source events are
dropped during an in-flight analyze by design, and a failed manifest
transaction is retried by the next Go source event as specified by F-1.

Verification completed successfully:

- `cargo test -p code-graph-lang-go --lib` — 92 passed.
- `cargo test -p code-graph-tools --test watch_go_reindex` — 5 passed.
- `cargo test -p code-graph-graph` — 193 unit tests and 1 integration test passed.
- `cargo clippy -p code-graph-graph --all-targets -- -D warnings` — passed.
- `make test` — workspace passed.
- `make lint` — workspace passed with warnings denied.
- `make fmt-check` — passed.
- `make snapshot-clean` — no pending snapshots.
- `make plugin-sync-check` — generated plugin mirrors in sync.
- `git diff --check` — passed.
