---
title: "Phase 1 Debrief: Symbol Model & Crate Module Foundation"
type: debrief
plan: "RustSupportGaps"
phase: 1
phase_title: "Symbol Model & Crate Module Foundation"
status: complete
created: 2026-05-21
updated: 2026-05-21
tags: [rust, parser, edges, namespace, response-shape, dogfood]
---

# Phase 1 Debrief: Symbol Model & Crate Module Foundation

Twelve commits on `rust-main` (six implementer commits + six quality-scanner follow-ups, ~50% follow-up rate). All five gates (`lint`/`fmt-check`/`test`/`snapshot-clean`/`leak-scan`) green. 1136+ workspace tests, ripgrep dogfood baseline shifted 3104 → 3118 symbols.

## Decisions Made

- **Watch-path `new_fg` copy-back (1.1).** The design said `post_index` writes into the `graphs` slice in place. The implementer discovered the watch handler historically merged `new_fg` directly (not via the slice), so any in-place mutation in `post_index` would be lost. They added a single-line copy-back from `all_graphs.last()` to `new_fg` and proactively flagged it. The 1.1 follow-up added a load-bearing test (mutating recording plugin) that fails if the copy-back is removed.
- **`[[bin]]` target roots deferred (1.2).** RCMM v1 only recognises `src/lib.rs` and `src/main.rs` as root modules. Documented in the module doc as a follow-up.
- **`#![allow(dead_code, reason = …)]` between commits (1.2 → 1.3).** The pure-logic RCMM module had no production consumer until 1.3 wired it in. A module-wide allow shielded clippy `-D warnings`. 1.3 lifted the allow and replaced with two narrow per-field allows (`CrateInfo.root`, `with_path_overrides`).
- **`#[cfg(test)]` over `#[allow(dead_code)]` for test-only helpers (1.4).** 1.4's `find_enclosing_trait` is exclusively called from `#[cfg(test)]` code; the scanner argued for `cfg(test)` directly rather than the `allow` shield. Both `find_enclosing_trait` AND `find_enclosing_impl` (which became test-only after the `enclosing_function_id` switch) were converted. New convention established: prefer `cfg(test)` over `allow(dead_code)` for genuinely test-only code.
- **Surgical CLAUDE.md updates per task vs deferred sweep (1.4 follow-up + 1.6).** The plan deferred all CLAUDE.md edits to 1.6, but the "Documentation read cold" lens flagged two task-1.4-specific sentences as agent-misleading. We surgically updated them in 1.4-follow-up and kept the full sweep for 1.6. The lens forced this; it should be the default going forward.
- **`mtimes` deliberately absent from v4 round-trip equality (1.5).** `mtimes` lives in `GraphCache`, not `Graph`; `load` never assigns it to `self`. The new round-trip equality asserts every field that IS on `Graph`. The 1.5 follow-up added an explicit comment for future readers.
- **Combined dedup+diamond test by individually-broken-layer validation (1.5).** The dedup-and-diamond compositional test was proven load-bearing by temporarily breaking each layer and confirming distinct failure shapes — a methodology that should generalise to any future composition of orthogonal correctness layers.

## Requirements Assessment

| Criterion | Status | Notes |
|-----------|--------|-------|
| `post_index` hook object-safe, default no-op, invoked over full set in `index_directory`; non-Rust plugins unaffected | Met | `Box<dyn LanguagePlugin>` storage compiles; recording-plugin tests cover both analyze and watch paths over full graph sets. |
| RCMM derives correct crate-qualified module paths for all rule cases incl. fallbacks/errors | Met | 19 unit tests covering `lib.rs`/`main.rs`/`foo.rs`/`foo/mod.rs`/`a/b.rs`/`-`→`_`/no-Cargo.toml/malformed/multi-crate/depth-tiebreak. |
| Rust symbols carry crate-qualified namespaces; trait default + abstract methods are `Method`/parent=trait | Met | 8 new namespace composition tests + replaced trait-kind test + corpus shift 39 → 41 symbols. |
| Hierarchy shows no duplicate bases | Met | `dedup_inherits_targets` helper at both `adj`/`radj` collection sites; load-bearing combined dedup+diamond test. |
| `CACHE_VERSION` = 4; stale caches silently re-index with no `force=true` | Met | `persist.rs:59`; `stale_v3_cache_returns_ok_false_silent_reindex` covers the load path. |
| Full structural gate green; ripgrep baseline + in-phase CLAUDE.md updated | Met | `make lint/fmt-check/test --workspace/snapshot-clean/leak-scan` all green; baseline 3104 → 3118 in same commit. |

## Deviations

- **Design Interfaces section was factually wrong about `index_directory` (caught at plan review).** Pre-implementation plan review flagged that `index_directory` has no cache merge (a stale analyze re-parses every file), and the watch handler (`try_reindex_file`) is a *separate* re-index path never named in the design. Both were corrected: the plan added the watch call site as a 1.1 subtask + extended `watch_rust_reindex` to assert post_index ran; the design's Interfaces paragraph was back-patched in place with a "Corrected post-approval — plan-review Critical 1+2" marker. None of the 10 design Decisions changed.
- **`find_nearest_def_ancestor` composite helper (1.4) wasn't in the design.** The design's text described a `find_enclosing_trait` helper to mirror `find_enclosing_impl`. The 1.4 implementer added that plus a composite `find_nearest_def_ancestor` that returns `Impl(type) | Trait(name) | None` from a single ancestor walk — strictly cleaner than two independent walks. Adopted without back-patching the design (mechanism-level detail, not a design decision).
- **`enclosing_function_id` was the first thing the design didn't mention but should have.** The 1.4 implementer initially left `enclosing_function_id` (call-edge `from` field) using `find_enclosing_impl`, which orphaned call edges from trait default methods. The scanner caught this; the 1.4 follow-up switched it to use the same composite helper. The lesson: when the trait/impl classification rule changed, EVERY place that consumed "what's my parent symbol id?" needed to change. The design specified the change at the *definition* extractor but not the *call edge* builder.
- **Two pre-existing CppMacroStrip plan-pointers in CLAUDE.md (1.6 follow-up).** Widening `make leak-scan` to scan `CLAUDE.md` surfaced two lines in the Quality-Lenses section referencing "CppMacroStrip Phase 3/4". The 1.6-follow-up implementer surgically rewrote the lines (preserved the behavioural lesson, dropped the plan-pointer) rather than allowlisting — and flagged the unintended scope expansion for review.

## Risks & Issues Encountered

- **Major correctness bug nearly shipped (1.4 → 1.4 follow-up).** The scanner caught `enclosing_function_id`'s mismatch with the new trait-method symbol IDs. Pre-fix, every call inside a trait default method emitted a `Calls` edge whose `from = path:method` while the corresponding symbol ID was `path:Trait::method` — orphaned edges, broken `get_callers`/`get_callees` on the qualified ID. The implementer's tests verified definition emission but not call-edge consistency. Resolution in `36552d2`: switched to `find_nearest_def_ancestor`; added `call_inside_trait_default_method_has_trait_qualified_from` regression test that fails if the production helper ever drifts back.
- **Statelessness test was weak (1.3 → 1.3 follow-up).** Original test called `post_index` twice on the same fixture — proves idempotence, not statelessness. A future cache-on-`&self` bug would have passed. Replaced with disjoint-fixtures test (`crate_a` then `crate_b`) plus negative assertions (`assert!(!ns.starts_with("crate_a"))` on fixture B).
- **Plan-pointer leaks (1.3, 1.6, 1.6 follow-up).** Three rounds: 1.3 widened scope to include `Cargo.toml` and caught a leak in the same commit's `tempfile` comment; 1.6 followed up by widening to `CLAUDE.md` and caught two CppMacroStrip references. Each widening surfaced pre-existing rot. The pattern is real: agent-readable files accumulate plan-pointers until something scans them.
- **Dead-code allow shield chained across commits (1.2 → 1.3).** Acceptable workflow but added a "lift the allow" subtask to 1.3. Future foundation/wiring pairs should consider colocating in one commit to avoid the bridge state, OR use `#[cfg(test)]` as in 1.4's pattern.
- **Hierarchy dedup + diamond ref-stub composition lacked a combined test (1.5 → 1.5 follow-up).** Two independent test suites covered each mechanism in isolation; the combined case (X has duplicate edges to Y AND Y reachable from a sibling arm) was uncovered. Added a test that verifies both layers are load-bearing by individually disabling each and observing distinct failure shapes.

## Lessons Learned

- **Per-task quality-scanner pays for itself empirically.** 6 tasks, 6 follow-ups, ~25 individual fixes captured. One of them (1.4 Major #1) was a real correctness defect that would have silently shipped. The other ~24 were ranged from "incorrect comment that misleads future readers" to "test that doesn't actually verify what its name claims". The scanner's intent-blind framing caught issues the plan-aware implementer was structurally biased to miss.
- **Plan review (between /plan and /implement) catches design-level errors implementation can't recover from.** The reviewer's Critical 1+2 on the plan (the `index_directory` cache merge / watch handler omission) traced back to the approved design — the design had been reviewed once but only against its own internal consistency, not against the code's actual shape. Catching it at plan time cost one re-draft; catching it during implementation would have meant a watch-path bug silently regressing namespaces every file save.
- **Designs need post-approval correction conventions.** Plan-review surfaces issues that don't invalidate decisions but do invalidate mechanism descriptions. The "Corrected post-approval — plan-review Critical N+M" inline marker pattern worked: kept the design as the source of truth, flagged what had moved, preserved approval status. Worth standardising.
- **The "Documentation read cold" lens is more strict than `make leak-scan`.** Leak-scan catches plan-artifact pointers in scoped paths. The cold-read lens catches contradictions between sibling sections, false claims about feature support (the 1.6 `#[path]` finding), and stale enumerations (the 1.6 cache-version bullet). Different defects, complementary tools. Both are needed.
- **Progressive leak-scan scope widening surfaces real rot.** Defaults expanded from `crates/*/src+tests` → `+Cargo.toml` (1.3) → `+CLAUDE.md` (1.6) across the phase. Future plans should start with the widest reasonable scope: every agent-readable text file in the repo.
- **`#[cfg(test)]` > `#[allow(dead_code, reason = …)]` for genuinely-test-only helpers.** Honest about scope; excludes from release binary; lint stays meaningful on production code. 1.4's switch establishes the convention.
- **The user's "fix all" pattern is load-bearing in this codebase.** Across 6 finding batches the user picked the "fix all" option every time (sometimes the recommended option, sometimes adding extra fixes). The cumulative effect across small fixes was the difference between shipping a robust Phase 1 and shipping a Phase 1 with documentation contradictions and an orphaned-edge correctness bug. Future debriefs should not pre-decide what's worth fixing on time grounds.
- **Quality-scanner FOCUS_LIST authoring is where orchestrator judgment shows.** The scanner's findings depended heavily on what I told it to verify. The 1.4 scan caught the orphaned call edges because the FOCUS_LIST included "verify Symbol ID consistency between definitions and call-edge `from` fields." A more generic "review for correctness" prompt would have missed it. Worth investing in the per-task focus list each time.

## Impact on Subsequent Phases

- **Phase 2 (Edge Coverage — issues 1, 6) is now unblocked.** The `post_index` hook is wired in both re-index paths; Phase 2's `mod`-declaration `Includes` edge resolution can drop into the existing `RustParser::post_index` body and reuse the RCMM. The `CrateModuleModel::with_path_overrides` seam exists but has no production consumer — Phase 2 (or 2.2 specifically) will be its first.
- **Phase 3 (Query Ergonomics — issues 4, 7) is independent of Phase 1's changes** and can run concurrently with Phase 2. Phase 1 didn't touch `Graph::callers`/`callees` or the call-tool handlers.
- **Phase 4 (Documentation & Contract Sweep) is partially front-loaded.** Phase 1's surgical CLAUDE.md updates (trait method classification, abstract-signature exception, crate-qualified namespace, CACHE_VERSION-4 cache-invalidation, `#[path]` correction) have already landed. Remaining for Phase 4: the two `server.rs` tool-description strings (CallChain field semantics + `suggestions`), cross-language summary table for callers/callees behaviour, and removal of the implicit "Rust has no Includes" cross-cutting limitation (which Phase 2 enables).
- **`CACHE_VERSION 4` is live on `rust-main`.** Anyone with a v3 cache will see a one-time silent re-index on next `analyze_codebase` call. No documentation or release-note follow-up needed beyond the CLAUDE.md update already landed.
- **Ripgrep baseline is now 3118 symbols** (was 3104). Future per-phase baseline updates only need to track additive shifts.
- **`scripts/leak-scan.sh` default scope is now `crates/*/src/* + crates/*/tests/* + crates/*/Cargo.toml + Cargo.toml + CLAUDE.md`.** Any new agent-readable file in the repo should be added proactively.

## Skill Opportunities

### `make sync-corpus` — keep corpus.rs / MANIFEST.md / baseline files in lockstep

- **What you did repeatedly:** when Phase 1 changed symbol shapes (1.4 abstract trait signatures added 2 symbols to `testdata/rust/`, then ripgrep +14), three artifacts had to be re-measured and edited by hand: `crates/code-graph-lang-rust/tests/corpus.rs` constants (`TOTAL_SYMBOLS`, per-kind counts, per-file tables), `testdata/rust/MANIFEST.md` (line numbers and call-edge rows — the 1.4 scanner caught a ~6-line offset because the file-header docstring grew), and `testdata/rust/ripgrep-baseline.txt` (`symbols:`/`tag:`/`commit:`). Three updates, three opportunities for drift. The 1.4 scanner caught one off-by-N; future task scanners may catch more.
- **Where it belongs:** a Makefile target `make sync-corpus` (or `make corpus-update`) that runs the parse-test harness over `testdata/rust/`, prints the current counts and per-file tables, and either emits a unified diff against the three artifacts (so the user can review before applying) OR applies it with `--apply`. The ripgrep baseline update is the same shape but submodule-dependent — should be a separate sub-target `make sync-baseline-rust` that needs `make submodules` first.
- **Why a skill:** removes a class of "doc drift caught by scanner" findings, which were exactly the kind of issue we ran into repeatedly. Keeping the three artifacts in lockstep mechanically prevents the MANIFEST.md being wrong about line numbers, the corpus being wrong about symbol counts, or the baseline being stale relative to the SHA-pinned ripgrep version.
- **Rough shape:** `make sync-corpus` runs `cargo run -p code-graph-parse-test -- testdata/rust/` (or equivalent), captures the JSON output, runs a small script (probably in `scripts/sync-corpus.{sh,py}`) that:
  1. Updates `TOTAL_SYMBOLS`, `TOTAL_EDGES`, per-kind counts, per-file tables in `corpus.rs`.
  2. Rebuilds the symbol table sections of `MANIFEST.md` (preserving prose).
  3. With `--include-baseline`, re-runs the parse-test against `external/ripgrep` (if initialised) and updates `testdata/rust/ripgrep-baseline.txt`.
  
  Output by default is a diff for review; `--apply` writes in place. Should be invoked at the end of any task that changes Rust symbol classification (1.4-class tasks), at every plan phase boundary, and as part of the pre-commit hook chain (perhaps `make pre-commit` runs it in `--check` mode).

### Shared `mk_crate_layout` helper — tempfile-based Rust crate fixture builder

- **What you did repeatedly:** tasks 1.3, 1.4, and 1.5 each independently wrote tempfile-based test fixtures spelling out a Cargo.toml + `src/lib.rs` + `src/foo.rs` (or `src/foo/mod.rs`) + assertions on the resulting symbols/namespaces/edges. Roughly 50 lines of boilerplate per test (TempDir creation, write Cargo.toml, write each `.rs` file, construct file paths, build FileGraph set, call `post_index`, parse symbols out). The 1.3 implementer added a `with_mutator` constructor to the `RecordingPlugin` helper for the same reason; that pattern should generalise.
- **Where it belongs:** a new test-helper module in `code-graph-tools/src/` (analogous to the existing `test_recording_plugin` module) — `pub(crate) mod test_crate_layout` exposed under `#[cfg(test)]`. Plus a peer for `code-graph-lang-rust`'s own tests, OR a shared `code-graph-test-fixtures` crate in the workspace (depending on whether tests in different crates can share the helper).
- **Why a skill:** removes ~50 lines of boilerplate per test that wants a real Rust crate fixture. Standardises the shape (Cargo.toml format, src layout, FileGraph construction) so tests across the workspace produce consistent fixtures. Reduces the chance of fixture-construction bugs masking real bugs (the 1.3 scanner Question about `TMPDIR` inside a Cargo workspace is the kind of edge case that's easier to handle once, in the helper, than per-test).
- **Rough shape:**
  ```rust
  pub(crate) struct CrateLayout {
      pub root: TempDir,             // owns the on-disk fixture
      pub crate_name: String,        // pre-normalized; "-" already → "_"
      pub graphs: Vec<FileGraph>,    // post-parse_file, pre-post_index
      pub file_index: FileIndex,     // built over `graphs`
  }
  
  /// Builds a Cargo.toml + src/ tree under a fresh TempDir, parses every
  /// `.rs` file, returns the layout ready for post_index.
  pub(crate) fn mk_crate_layout(
      crate_name: &str,
      files: &[(&str, &str)],       // (src-relative path, file content)
  ) -> CrateLayout { ... }
  
  /// Variant that skips Cargo.toml emission — used by the no-Cargo.toml
  /// fallback tests (e.g. `post_index_fallback_when_no_cargo_toml_…`).
  pub(crate) fn mk_crateless_layout(
      files: &[(&str, &str)],       // path is relative to a fresh TempDir
  ) -> CrateLayout { ... }
  ```
  Invoked from any test that needs a real Rust crate on disk (the manifest-discovery walk requires real files; `PathBuf::from("/fake/path")` doesn't work). Phase 2 and Phase 3 will both need similar fixtures (mod-resolution tests need real `foo.rs`/`foo/mod.rs` files; resolved-only-filter tests benefit from real call-graph fixtures), so investment here amortises across the remaining phases.
