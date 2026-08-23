---
title: "Resolution Verification: a4f736b (F-1..F-8 dispositions and fixes)"
type: review
tags: [review, go-resolver, watch, cache-v13, cli, resolution-verification]
related: [KNOWN_ISSUES.md, Decisions/decisions.md, Plans/RustSupportGaps/reviews/06-resolver-hardening-adversarial-review-bfb0bc6.md]
review_of: "Plans/RustSupportGaps"
rev: "bfb0bc6ae4b887177adec34260023cc391dd2ef7..a4f736bb3dc31ea735074e0398338dbee741ac1c"
reviewed_range: bfb0bc6..HEAD (a4f736b)
review_mode: primary verification of the 06-review Resolution Log plus new-findings pass
verdict: All eight findings correctly resolved; two new minor liveness gaps and three nits
---

# Resolution Verification: `a4f736b`

**Scope.** Commit `a4f736b` ("fix(go): harden resolver invalidation and cache
recovery") implements the Resolution Log appended to
`06-resolver-hardening-adversarial-review-bfb0bc6.md` (F-1..F-5 implemented,
F-6/F-8 no-action, F-7 rejected) and commits the updated review artifact.
This pass verifies each disposition against the code, re-runs the named
regressions, and records new findings (N-1..N-5) that the fix pass itself
surfaces.

**Verdict: all eight findings correctly resolved.** No finding was
closed without matching code, and no regression test is vacuous — the F-1
regression in particular is discriminating (its final assertion fails under
the pre-fix consumption semantics). The verification also **confirms the
F-7 rejection is correct and retracts the original finding** (the alleged
same-batch race cannot occur under the materialized-batch ordering), and
surfaces two new minor gaps in manifest-change liveness (N-1, N-2) plus
three nits (N-3..N-5).

## Disposition verification

| Finding | Claimed disposition | Verified | Evidence |
|---|---|---|---|
| F-1 | Actionable — transaction-scoped pending state | **Correct** | `manifest_reindex_pending: AtomicBool` (`lang-go/lib.rs:129-133`); set in `invalidate_resolution_for_path` for `go.mod` only (`:931-936`); observed via `resolution_cache_invalidated` (`:948-950`); cleared only by `commit_resolution_invalidation` (`:952-955`), called once, after the all-Go publication in `try_reindex_go_manifest` (`watch.rs:683`). Every post-invalidate early return (daemon-root recheck `:554`, read/parse `:590-616`, second recheck `:665`) leaves the flag set. `invalidate_resolution_cache` (analyze) advances the epoch WITHOUT setting the flag (`:939-942`) — analyze never creates a retry demand. The gate (`watch.rs:189-203`) diverts ANY Go event while pending to the full transaction. Regression `go_mod_create_modify_remove_rebuilds_go_universe` exercises failed v5 transaction → Rust reindex (which runs all-plugin `post_index`/`prepare_resolution`, `watch.rs:378-383` — the exact old consumption vector) → asserts v4 everywhere → Go event → asserts v5 on ALL Go files. The last assertion fails pre-fix (mixed identities), so the test is not vacuous |
| F-2 | Actionable, broadened — extension-local recovery | **Correct, as broadened** | `decode_archived` no longer touches the extension (`packed.rs:672-757`); new `decode_archived_resolver_metadata` with its own `MetadataDecodeError` type covering OOR path IDs, OOR name IDs (via `InvalidReference`), `PathNotIndexed`, `LanguageMismatch`, `UnsupportedVersion` (`packed.rs:879-895`). `load_and_stale` discards the extension on any `MetadataDecodeError` with an `eprintln!` breadcrumb and loads the intact main archive (`persist/mod.rs:394-410`). Unsupported version and extension bytecheck failure discard at the validation layer (`persist/mod.rs:528-542`). Main-archive semantic failures remain `Err(CorruptedCache)` — deliberately, per the log; see N-4 for the doc alignment this leaves. All 8 named tests present and green |
| F-3 | Actionable with F-2 — language guard | **Correct** | `set_resolver_metadata` requires both an indexed `Language::Go` file and `ResolverMetadata::Go` (`graph.rs:578-586`); API shape unchanged (no-op on invalid input). All three production call sites (analyze persistence `core/analyze.rs:607-609`, watch per-file `watch.rs:492,513`, manifest batch `watch.rs:660,676`) obtain metadata through the edited file's OWN plugin hook, which only Go overrides — no misuse possible. `resolver_metadata_accepts_only_indexed_go_files` + `non_go_resolver_metadata_is_ignored_and_cache_remains_loadable` pin both layers |
| F-4 | Actionable — document full tiers | **Correct (with residual N-3)** | Both the `max_distance` schema and the production description now state "0 edits at length 1, 1 at 2-11, 2 at 12-17, 3 at 18+" (`server.rs:921-923, 1497-1499`); snapshot updated in the same commit; all 33 `snapshot_tools_list` tests pass (re-run). The added "non-empty" wording is accurate: near mode rejects empty queries in code (`core/symbols.rs:282-286`) and is unit-pinned (`search_symbols_near_mode_rejects_empty_query`). Residual: the suggestions-block threshold table in the same description was not updated (N-3) |
| F-5 | Actionable tracking — KNOWN_ISSUES entry | **Correct** | `KNOWN_ISSUES.md` F4 documents the all-Go cost, why directory/edge narrowing is unsound, the reverse-manifest-metadata replacement criterion, and four acceptance criteria. Read cold: self-contained, consistent with D-0017's rationale clause, no stale cross-references |
| F-6 | No current code action | **Appropriate** | Rewriting `ef52f26`'s subject would require history rewrite for no product benefit; recorded as a process note |
| F-7 | Rejected — fixed-batch ordering | **Rejection confirmed; original finding retracted** | The batch is a materialized immutable vector BEFORE any manifest rescan runs (`watch.rs:795-825` materializes, `:811` rescans). A write's on-disk visibility precedes its watcher event (the FS notification is caused by the write), the event precedes batch materialization (debounce), and the rescan's disk read runs after materialization inside `spawn_blocking` (`watch.rs:583-601`). Therefore every event in the batch reflects a write already visible to the rescan's read, and any later write necessarily produces a LATER event — the "post-read write whose event is already in this batch" ordering the original finding posited cannot occur. The added comment at `watch.rs:831-835` is accurate. No redundant per-file re-read added, as directed |
| F-8 | Optional; no action now | **Appropriate** | The original "every analyze performs two discoveries" was overbroad — a full cold analyze's `post_index` and `prepare_resolution` both see the full universe (one discovery). Scoped-over-warm and partial-fresh analyzes still pay two; the pre-existing, semantically-correct, watch-free cost stands as an optional optimization |

## New findings

| # | Severity | Location | Issue |
|---|---|---|---|
| N-1 | **minor** | `core/analyze.rs:287-315` (fast-path gate) | The analyze fast path is blind to `go.mod` changes: staleness is source-file mtime, and no plugin claims `go.mod`, so a manifest-only change yields empty `in_scope_stale` and no uncached source files → fast path → every Go symbol keeps its pre-edit module namespace until a project-root forced analyze, a watch manifest event, or a `go.mod` re-edit. The graph stays CONSISTENT (stale-uniform, not mixed: the module-model cache key `(paths, epoch)` is unchanged, so the next per-file Go edit resolves under the same stale model) — stale metadata only, no false edges |
| N-2 | **minor** | `watch.rs:543-553` (invalidate after `try_lock`) | A `go.mod` event arriving while an `analyze_codebase` holds `index_lock` returns `LockContended` BEFORE `invalidate_resolution_for_path` runs, so no pending state is ever set — a DROPPED manifest transaction is not retried (F-1's guarantee covers only FAILED transactions). Combined with N-1 (the in-flight analyze's fast path will not repair it either, since a manifest-only change leaves no stale source files), a `go.mod` edit during an in-flight analyze can be silently lost until the next `go.mod` event or project-root forced analyze |
| N-3 | nit | `server.rs` tool description, suggestions block | The suggestions threshold table still reads "1 edit at length 2-11, 2 at 12-17, 3 at 18+", but `levenshtein_suggestions` (`core/symbols.rs:409`) applies `max_distance_for_query` to the INNER name, which is 0 for a length-1 inner (`^a$`). At that tier the distance-0 Levenshtein path is provably empty (an exact-name hit would make `total > 0`, suppressing suggestions), so the separately-documented broad-substring fallback always fires — behavior is covered by the fallback clause, but the threshold table omits the tier it documents elsewhere |
| N-4 | nit | CLAUDE.md CLI section (FR-20 sentence) vs `persist/mod.rs` + `main_archive_semantic_id_failure_remains_corrupted_cache_error` | "A readable-but-corrupt or version-mismatched cache is 'not present' → honest-unindexed → 1, NOT 2" no longer matches code for the three MAIN-ARCHIVE semantic `DecodeError` variants (`PathOutOfRange`, `NameOutOfRange`, `InconsistentSymbolId`), which remain `Err(CorruptedCache)` → CLI exit 2 — a behavior the F-2 fix deliberately retained and now test-pins. The F-2 fix shrank the mismatch from "any semantic decode failure" to "main-archive semantic failures"; the doc sentence should be amended to match (structurally-invalid/version-mismatch → 1; structurally-valid-but-semantically-inconsistent main archive → 2), or the classification changed |
| N-5 | nit | 06-review Implementation Evidence, F-1 row | "Root-recheck, read, parse, and **resolution** failures leave the manifest transaction pending" — `resolve_edges_with_indexes` returns `()` (`indexer.rs:476-484`); resolution has no non-fatal failure mode. The actual non-fatal failure paths are the two daemon-root rechecks, per-file read, per-file parse, and a blocking-task panic. Wording only; the code's guarantee is exactly as stated in the comment at `watch.rs:680-682` |

### N-1 + N-2 — the manifest-liveness seam (detail)

Both are pre-existing in mechanism (the mtime-staleness design and the
lock-then-invalidate ordering predate `a4f736b`), but they are the residual
edges of exactly the guarantee F-1 establishes, so they belong to this
record. Consequence class for both: stale-but-UNIFORM Go module identity
(namespace text in `search_symbols`/`get_symbol_summary`; no symbol-ID or
edge corruption, since symbol IDs are namespace-free and the module model is
rebuilt from disk only on epoch advance). Self-heals on the next `go.mod`
watch event (sets pending → full rescan) or project-root `force=true` analyze.

- **N-1 trigger:** `go.mod` edit + non-forced `analyze_codebase` with no
  intervening watch manifest event (watch disabled, or the daemon serving a
  query-only workload). The v4→v4-style "refresh analyze" step in
  `go_mod_create_modify_remove_rebuilds_go_universe` passes only because it
  uses `force=true` at the project root.
- **N-2 trigger:** `go.mod` write lands in a debounce batch while
  `analyze_codebase` holds `index_lock` (analyze duration × edit probability;
  seconds-to-minutes window on large repos).
- **Cheap combined fix (N-2):** move `invalidate_resolution_for_path` to the
  top of `try_reindex_go_manifest`, before `index_lock.try_lock()` (after the
  filename/parent/plugin validation, which needs no lock). A dropped
  transaction then leaves the pending flag set, so the next Go event retries
  the full rescan; if the in-flight analyze had already absorbed the change
  (slow path), the retry is a redundant but idempotent rescan. **N-1** then
  needs either (a) manifest mtime participation in the stale computation
  (stat the manifest set derived from indexed Go paths against mtimes
  recorded in the resolver-metadata extension or a cache footer field) or
  (b) a KNOWN_ISSUES entry documenting that manifest changes are picked up
  only by watch events or project-root forced analyzes.

## Verification evidence (this pass, 2026-08-22)

- `make verify` — **PASS end to end** on `a4f736b` (clippy `-D warnings`,
  fmt, full workspace test suite, snapshot-clean, plugin-sync-check).
- Re-ran the named regressions: `code-graph-graph` persist suite (all 8 new
  metadata-classification tests + `main_archive_semantic_id_failure_remains_
  corrupted_cache_error` + `non_go_resolver_metadata_is_ignored_and_cache_
  remains_loadable` green); `code-graph-tools --test watch_go_reindex` 5/5
  (incl. the strengthened `go_mod_create_modify_remove_rebuilds_go_universe`
  and `live_watch_dispatches_go_mod_events`); `code-graph-lang-go --lib`
  92/92 (incl. `manifest_invalidation_stays_pending_until_publication_
  commit`); `graph::tests::resolver_metadata_accepts_only_indexed_go_files`;
  `--test snapshot_tools_list` 33/33 (matches the log's claim).
- Code traces (primary): full `try_reindex_go_manifest` return-path audit
  (no post-invalidate return skips the commit except deliberately);
  all-plugin `prepare_resolution` on the ordinary watch path
  (`watch.rs:378-383`) confirming the old consumption vector and its
  neutralization; `with_validated_archive` framing-vs-bytecheck-vs-semantic
  error taxonomy; `set_resolver_metadata` call-site audit (3 sites, all
  through the file's own plugin hook); near-mode empty-query rejection
  (`core/symbols.rs:282-286`); suggestions threshold source
  (`core/symbols.rs:409`); fast-path gate staleness inputs
  (`core/analyze.rs:287-315` — source mtime + source discovery only).

## Recommended actions

1. **Fix N-2** (one-line move + one comment): invalidate before
   `try_lock` in `try_reindex_go_manifest`; add a regression — manifest
   event during a held `index_lock` → lock released → next Go event retries
   the full transaction.
2. **Decide N-1**: implement manifest-mtime staleness (preferred; makes
   non-watch analyzes correct) or file a KNOWN_ISSUES entry documenting the
   watch/force-only manifest liveness.
3. **Fold N-3** into the next `search_symbols` description touch: add "0
   edits at length 1" to the suggestions threshold table (or reword the
   table as applying to inner names of length ≥ 2, noting the substring
   fallback covers shorter).
4. **Fix N-4**: amend the CLAUDE.md FR-20 sentence to distinguish the two
   corruption classes now pinned by `main_archive_semantic_id_failure_
   remains_corrupted_cache_error`.
5. **N-5**: optional wording correction in the 06-review Implementation
   Evidence (drop "resolution" from the failure list).

N-1/N-2 do not block: stale-uniform metadata only, narrow triggers,
self-healing. They should be tracked so the F-1 guarantee is not quietly
read as covering dropped events or non-watch analyzes.

## Resolution Log

Actionability was re-checked against `a4f736b` and the current watch, analyze,
search-description, cache-loader, and CLI documentation paths. All five new
findings are actionable; N-1 takes the review's explicit documentation option
rather than introducing manifest identity into cache v13 in this small
follow-up.

| Finding | Disposition | Resolution / evidence |
|---|---|---|
| N-1 | **Tracked limitation** | Added `.plans/KNOWN_ISSUES.md` F5. It documents that non-forced analyze does not observe manifest-only edits, distinguishes stale-uniform metadata from graph corruption, names `analyze_codebase(<project_root>, force=true)` as the guaranteed refresh (scoped force is scope-limited), and records the manifest-identity persistence boundary for an automatic fix. It also documents that a lock-contended watch event needs a later Go event to retry. |
| N-2 | **Implemented** | Moved `invalidate_resolution_for_path(go.mod)` before `index_lock.try_lock()`. A contended event now advances the manifest epoch and leaves `manifest_reindex_pending` set. `go_mod_create_modify_remove_rebuilds_go_universe` now holds `index_lock`, verifies the immediate manifest transaction returns `LockContended` without partial publication, releases the lock, triggers an ordinary Go event, and proves every affected file moves atomically from module v5 to v6. The implementation intentionally follows the review's cheap retry-on-next-Go-event contract; it does not add a background retry queue. |
| N-3 | **Implemented** | Updated the production `search_symbols` suggestions table to include `0 edits at length 1`, retained the documented substring fallback, and refreshed the tools-list snapshot. |
| N-4 | **Implemented, clarified across copies** | Updated `CLAUDE.md`, the CLI bootstrap comment, the CLI design, and phase acceptance text. Structurally invalid/version-mismatched cache is honest-unindexed (`1`); a bytecheck-valid but semantically inconsistent main archive is operational (`2`); bytecheck/version/semantic failure inside an independently framed metadata payload is discarded while malformed extension framing remains structural invalidity. |
| N-5 | **Implemented** | Corrected review 06's evidence from nonexistent "resolution failures" to the actual blocking-task failure path. |

Post-implementation independent review found two documentation overclaims,
both corrected before closure: watch retry after lock contention is conditional
on a later Go event, and only failures inside a trusted metadata payload retain
the main graph; malformed extension framing rejects the whole cache. A final
cold read also qualified the guaranteed manual recovery as project-root
`force=true` (scoped force is scope-limited) and removed the remaining broad
"corrupt bytes → exit 1" wording from the CLI design.

Verification completed successfully:

- `cargo test -p code-graph-tools --test watch_go_reindex` — 5 passed.
- `INSTA_UPDATE=always cargo test -p code-graph-tools --test snapshot_tools_list` — 33 passed and the intended snapshot updated.
- `make verify` — full workspace tests, clippy with warnings denied, format check, snapshot cleanliness, and plugin mirror sync all passed.
- Four independent review lanes completed; plan-drift and quality wording findings were resolved above, while spec-compliance reported no findings and the blind-spot liveness observation is now explicitly documented as the retained retry-on-next-event boundary.
