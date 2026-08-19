---
title: "Symbol History"
type: phase
plan: GraphPlatformExpansion
phase: 6
status: in-progress
created: 2026-08-08
updated: 2026-08-19
deliverable: "A symbol_history tool reporting the revisions at which a symbol's content actually changed, with a fingerprint hook on LanguagePlugin and a content-addressed fingerprint cache."
tasks:
  - id: "6.1"
    title: "LanguagePlugin::fingerprint_symbol hook with a std-only text default"
    status: complete
    justifies: "FR-34 (Normalized half), NFR-02, AC-38. parse_file returns a FileGraph, not a syntax tree, so there is no way to fingerprint an AST from outside a plugin; the hook is what makes AST overrides possible in phase 8 without leaking tree-sitter across a crate boundary."
    verification: "cargo test -p code-graph-lang fingerprint:: — the default implementation returns an unchanged fingerprint for a symbol whose only change is whitespace or comments (AC-38); LiteralInsensitive returns None from the default rather than silently falling back to Normalized; all six existing plugins compile with no change, proving the hook is additive; cargo tree confirms code-graph-lang gained no third-party dependency (NFR-02)."
  - id: "6.2"
    title: "Fingerprint cache under .code-graph/fingerprints"
    status: complete
    justifies: "FR-37, AC-46. A history walk re-reads and re-parses every revision in the window and the parse dominates; without the cache a repeated query pays the whole cost again, and the tombstone case makes pre-existence revisions free."
    verification: "cargo test -p code-graph-tools fingerprint_cache:: — a second identical query is served from cache; deleting the cache directory recomputes the same answer; a corrupted shard recomputes rather than erroring (AC-46); a revision predating the symbol is cached as a tombstone and not re-parsed on the second walk; the cache is separate from .code-graph-cache.db and CACHE_VERSION is unchanged."
    depends_on: ["6.1"]
  - id: "6.3"
    title: "symbol_history tool: transition walk and exact symbol matching"
    status: complete
    justifies: "FR-33, FR-35, NFR-10, NFR-11, AC-19, AC-20, AC-37, AC-45. Distinguishing a logic change from a reformat is the whole value of the feature; without transition comparison the tool degenerates into git log for a file, which the agent could already get."
    verification: "cargo test -p code-graph-tools --test symbol_history against a hermetic git fixture (phase 5's fixture pattern) — a reformat-only commit is not reported while the logic commit is (AC-19); a commit moving the function without changing it is not reported (AC-20); historical bytes are parsed in memory with no temporary file written, asserted by watching the temp directory (AC-37); a case-only rename reports Removed then Introduced, matching the exact-match rule; a large-window call does not delay a concurrent non-history query (NFR-10); the tool description meets the agent-facing lens (AC-45)."
    depends_on: ["6.2"]
  - id: "6.4"
    title: "Resolve the phase-gate review findings (gate artifact 18, cycle 1)"
    status: complete
    justifies: "Gate artifact 18 cycle 1: one material finding (historical parses skipped the config pipeline, so [cpp].macro_*-dependent symbols silently reported empty histories — the flagship UE configuration) plus confirmed minors (deletion commits downgraded to skips, boundary flag blind to skipped-oldest revisions, data-dependent literal_insensitive rejection, same-process temp-path collision, undocumented rename/duplicate-name/truncation caveats)."
    verification: "cargo test -p code-graph-tools --test symbol_history — a [cpp].macro_strip-dependent class walks a real introduced/modified history; git rm reports removed at the deletion commit with no skips; a mock provider with an unreadable oldest blob labels the introduction at_window_boundary; literal_insensitive errors before any provider call; cargo test -p code-graph-tools --lib fingerprint_cache — a different config identity is a cache miss; make verify green."
    depends_on: ["6.3"]
  - id: "6.5"
    title: "Resolve the cycle-2 material finding: unreadable blob is Operation, not NotFound"
    status: complete
    justifies: "Gate artifact 18 cycle 2: past a successful tree-entry lookup, a find_blob failure (partial clone without the blob, corrupt object store, gitlink) mapped to NotFound — which task 6.4 made the deterministic cacheable absence signal — would manufacture false removed/introduced transitions and cache a permanent false tombstone keyed by the immutable revision."
    verification: "cargo test -p code-graph-vcs-git — a committed gitlink entry whose commit object is absent reports Operation while a genuinely absent path keeps NotFound; cargo test -p code-graph-tools --test symbol_history — a mock provider drives history_truncated:true through the tool (cycle-2 undispositioned test gap); make verify green."
    depends_on: ["6.4"]
---

# Phase 6: Symbol History

## Overview

The second history feature, and the one with no equivalent anywhere else in the tool surface: when did this symbol's *logic* actually change, as opposed to when was its file touched. Built on a new `LanguagePlugin` hook, a content-addressed fingerprint cache, and a transition walk over revisions.

Depends on phase 5. Gates phase 8.

## 6.1: LanguagePlugin::fingerprint_symbol hook with a std-only text default

### Subtasks
- [x] Add `FingerprintMode { Normalized, LiteralInsensitive }` to `code-graph-lang`
- [x] Add `fingerprint_symbol(&self, content, symbol, mode) -> Option<u64>` with a default implementation
- [x] Implement the default: hash the symbol's line span with comments stripped and inter-token whitespace normalized
- [x] Return `None` for `LiteralInsensitive` in the default — never fall back to `Normalized`
- [x] Use `std::collections::hash_map::DefaultHasher` and nothing else
- [x] Confirm all six plugins compile unchanged
- [x] Tests for the reformat-invariance property and the unsupported-mode path

### Notes
Revision boundary: every language can be fingerprinted in `Normalized` mode; nothing consumes it yet.

**Whitespace rule refined at implementation.** The design sketch said
"whitespace runs collapsed", but pure collapse-to-one-space preserves the
presence-vs-absence distinction, so the canonical reformat — a line break
after `(`, exactly AC-38's fixture shape — would change the fingerprint.
The shipped rule is token-aware: a separator survives only where two word
tokens would otherwise merge (`fn add` stays two tokens; `add( left` and
`add(left` normalize identically). Known cost, documented in the module:
spacing-sensitive punctuation pairs (`a - -b` vs `a--b`) hash equal — a
sensitivity loss, never an instability.

The hook follows the trait's existing shape — `preprocess`, `synthesize_symbols`, `resolve_call`, and `post_index` are all default-provided hooks plugins may override — so this adds no new pattern and no plugin needs touching.

The **std-only** constraint is not stylistic. `code-graph-lang` is one of the four crates NFR-02 protects; reaching for `blake3`, `xxhash`, or `ahash` here violates it the moment it lands, and nothing about the code would look wrong. A faster hash is permitted in the vcs crates only.

`DefaultHasher` is unstable across Rust releases, which is fine for in-process equality but means the fingerprint cache must key on binary identity or be treated as invalid across upgrades. Handle that in 6.2.

### Completion Evidence

- Verified: 2026-08-18
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `0baf6c65bf4e1dbe41c9ee16313f401bbcea95e3`
- Identity recheck: `git rev-parse HEAD` at 2026-08-18 20:05 matched `0baf6c65bf4e1dbe41c9ee16313f401bbcea95e3`
- Focused review: `git show 0baf6c65bf4e1dbe41c9ee16313f401bbcea95e3`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `0baf6c65bf4e1dbe41c9ee16313f401bbcea95e3`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-lang && cargo clippy -p code-graph-lang --all-targets -- -D warnings && cargo fmt --all --check` | `.` | PASS (`exit 0`) | `65 tests passed including the twelve new fingerprint tests (reformat/comment invariance incl. AC-38's canonical line-break-after-paren shape, literal/code sensitivity, string verbatim-ness, Rust lifetimes and nested block comments, Python/Go string forms, span edges, determinism, LiteralInsensitive -> None); clippy denied no warnings.` |
| `cargo tree -p code-graph-lang -e normal --depth 1 && cargo check -p code-graph-lang-{cpp,rust,go,python,csharp,java}` | `.` | PASS (`exit 0`) | `Dependency list unchanged (code-graph-core, thiserror, tree-sitter) — no third-party hash entered the NFR-02-protected crate; all six plugins compile unchanged, proving the hook is additive.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `focused diff review` | `git show 0baf6c6` | PASS | `Two files: the new fingerprint module and the trait/enum addition; no plugin, indexer, or wire surface touched.` |

### Trap
Making `LiteralInsensitive` fall back to `Normalized` when a plugin has no override. It feels helpful and it makes "the logic didn't change" mean two different things depending on the language, with no way for the caller to tell which they got. Return `None` and let the handler report the mode as unsupported.

## 6.2: Fingerprint cache under .code-graph/fingerprints

### Subtasks
- [x] Key on `(provider id, RevId, repo-relative path, symbol name, kind, mode, binary identity)`
- [x] Store the `u64` fingerprint or a tombstone recording absence at that revision
- [x] Shard on the key's leading bits, one file per shard
- [x] Treat every read, decode, or key-mismatch failure as a miss; delete and recompute a corrupt shard (the full key string is stored, so key aliasing is structurally impossible rather than detected)
- [x] Confirm no interaction with `.code-graph-cache.db` or `CACHE_VERSION`
- [x] Tests for hit, cold miss, deleted directory, corrupt shard, and tombstone reuse

### Notes
Revision boundary: fingerprints are cached and the cache is provably safe to lose.

Keying on `RevId` is what makes the correctness argument trivial: revisions identify immutable content in every VCS worth supporting, so a key can never name two different contents and a stale entry cannot be served. That satisfies FR-37 structurally rather than by an invalidation rule — the only kind of cache-correctness argument worth making.

Tombstones matter more than they look. "Not present at this revision" is a real, reusable answer that drives `Introduced` and `Removed`; without caching it, every walk re-parses all the revisions predating the symbol.

### Completion Evidence

- Verified: 2026-08-18
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `ea6df2df6f05b8788d570748d80392fd13a1f313`
- Identity recheck: `git rev-parse HEAD` at 2026-08-18 20:25 matched `ea6df2df6f05b8788d570748d80392fd13a1f313`
- Focused review: `git show ea6df2df6f05b8788d570748d80392fd13a1f313`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `ea6df2df6f05b8788d570748d80392fd13a1f313`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools --lib fingerprint_cache && cargo clippy -p code-graph-tools --all-targets -- -D warnings && cargo fmt --all --check` | `.` | PASS (`exit 0`) | `Six cache tests passed: hit, tombstone round-trip, key isolation (rev/symbol/mode), deleted-directory recompute, corrupt-shard delete-and-recompute, and graph-cache independence (AC-46); clippy denied no warnings.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `focused diff review` | `git show ea6df2d` | PASS | `One new module plus its core/mod.rs registration; no interaction with persist/, CACHE_VERSION, or any wire surface; writes are temp+rename and every failure path degrades to a miss.` |

## 6.3: symbol_history tool: transition walk and exact symbol matching

### Subtasks
- [x] Fetch revisions touching the file, bounded by a documented window
- [x] Give `revisions_touching` a truncation signal (gate artifact 15 follow-up: the revwalk cap currently truncates silently; this tool must not consume it blind) and surface it plus the window-filled state on the wire
- [x] Walk oldest to newest, computing the fingerprint at each revision
- [x] Emit only transitions: `Introduced`, `Modified`, `Removed`
- [x] Match the symbol at each revision by exact, case-sensitive `(name, kind)`
- [x] Run the whole per-revision loop inside `spawn_blocking`
- [x] Report unsupported modes and partial windows explicitly; skip and flag revisions that fail to parse
- [x] Register the tool with a description covering the rename behaviour and the window bound
- [x] Tests per the verification field

### Notes
Revision boundary: the second history feature is live end to end.

Oldest-to-newest is required: each revision is compared against a known predecessor, and newest-first would need lookahead and would get the `Introduced` boundary wrong at the end of the window. The oldest entry in a bounded window cannot be distinguished from a genuine introduction — say which it is rather than mislabelling it.

The per-revision loop is CPU-bound — `read_at`, then a tree-sitter parse, then a fingerprint that may parse again — so it must run inside `spawn_blocking`. Phase 5's `spawn_blocking` covers the *provider*; this loop is the heavier half and would starve the runtime on its own.

Historical code may not parse with today's grammar. That is expected, not exceptional: skip the revision and flag it.

### Completion Evidence

- Verified: 2026-08-19
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `07b10d3e238821dc78c249359cf2c80657040f56`
- Identity recheck: `git rev-parse HEAD` at 2026-08-19 12:35 matched `07b10d3e238821dc78c249359cf2c80657040f56`
- Focused review: `git show 07b10d3e238821dc78c249359cf2c80657040f56`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `07b10d3e238821dc78c249359cf2c80657040f56`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools --test symbol_history` | `.` | PASS (`exit 0`) | `9 tests passed: transitions-only over the reformat/move fixture (AC-19, AC-20); no-temp-file watch during a walk (AC-37); removed-then-reintroduced double transition; case-only rename reports removed + introduced under exact (name, kind) matching (D-0005); window boundary labelled at_window_boundary and a 9999 request clamps to 500; unavailability (no VCS, untracked file) is a SUCCESS shape with available: false + reason (FR-36); unknown and unsupported modes are tool errors naming "normalized"; a second identical walk is served from the fingerprint sidecar and matches byte-for-byte; a gated slow provider delays only history tools while a concurrent search_symbols completes first (NFR-10).` |
| `make verify` | `.` | PASS (`exit 0`) | `clippy -D warnings clean (suite mutex switched to tokio::sync::Mutex to satisfy await_holding_lock); fmt clean; full workspace tests green including the 24->25 tool-count assertions in smoke/daemon_serve/daemon_proxy/server unit tests and the new tools-list snapshot; no pending snapshots; plugin mirrors in sync.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `focused diff review` | `git show 07b10d3` | PASS | `13 files: RevisionWindow trait change (vcs + vcs-git + all test doubles, truncation flagged at MAX_REVWALK_COMMITS with a comment distinguishing provider truncation from a filled caller limit); core walk in core/history.rs (async cache-consult + read_at prefetch phase, then one spawn_blocking for the CPU-bound parse+fingerprint loop); handler adapter takes Arc<ServerInner> (registry needed inside the 'static closure — core::analyze precedent); #[tool] description covers rename behaviour, window bound, and every response field (AC-45 lens); transition trichotomy guards correct (before is None on the first examined revision, so the !first_examined guards are belt-and-braces, not load-bearing); tombstones and fingerprints both written back to the cache; LiteralInsensitive never silently falls back to Normalized.` |

### Trap
Reporting every revision returned by `revisions_touching`. That is `git log -- <file>`, which the agent can already get and which answers a different question. The tool's entire value is the comparison step that drops revisions where the symbol did not change.

## 6.4: Resolve the phase-gate review findings (gate artifact 18, cycle 1)

### Subtasks
- [x] M1: run historical bytes through the indexer's config pipeline (`preprocess` byte-rewrites feed the parse; `synthesize_symbols` sees the original bytes) so `[cpp].macro_*`-dependent symbols have real histories
- [x] Add the effective config's identity to the fingerprint cache key (`FingerprintKey.config` via `config_identity`) — a config change reads as misses, exactly like a different binary
- [x] Map `read_at` `NotFound` to an examined absence (`RevisionInput::Absent`, cached as a tombstone) so a file-deletion commit reports `removed` instead of a skip
- [x] Extend `at_window_boundary` to the skipped-oldest case (skips preceding the first examined revision carry the same boundary uncertainty as a filled window)
- [x] Reject `literal_insensitive` up front so the outcome is data-independent
- [x] Per-process write sequence in shard temp names (same-process walks cannot share a temp path)
- [x] Document in the tool description + CLAUDE.md: earliest-occurrence rule for duplicate `(name, kind)`, file renames not followed, deletions report `removed`, raising `window` cannot extend past `history_truncated`
- [x] Tests per the verification field (3 new integration tests, 1 new cache unit test)

### Notes
Cycle-1 findings NOT fixed here, accepted as recorded follow-ups: unbounded prefetch memory on a cold cache (window × file size; cap or chunk later), NFR-10's test gates the provider await rather than the blocking pool (structural guarantee verified by inspection), `binary_identity` constant-fallback aliasing (requires two unreadable executables), Rust lifetime-list mis-lex in the default fingerprint (`<'a,'b>` vs `<'a, 'b>` hashes differ — phase 8 per-language override territory), shard-growth across rebuilds, and the theoretical `\u{1f}`-in-filename key injection.

### Completion Evidence

- Verified: 2026-08-19
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `d6b3d9c9813e67a41d4bf3b5208abfb9c10d3bbd`
- Identity recheck: `git rev-parse HEAD` at 2026-08-19 13:02 matched `d6b3d9c9813e67a41d4bf3b5208abfb9c10d3bbd`
- Focused review: `git show d6b3d9c9813e67a41d4bf3b5208abfb9c10d3bbd`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `d6b3d9c9813e67a41d4bf3b5208abfb9c10d3bbd`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-tools --test symbol_history && cargo test -p code-graph-tools --lib fingerprint_cache` | `.` | PASS (`exit 0`) | `12 integration tests passed including the three new pins: a class extractable only under [cpp].macro_strip walks a real introduced/modified history with zero skips (M1); git rm reports removed at the deletion commit with no skips; a mock provider with an unreadable oldest blob labels the introduction at_window_boundary despite an unfilled, untruncated window. 7 cache unit tests passed including config-identity key isolation (same key under a different config is a miss).` |
| `make verify` | `.` | PASS (`exit 0`) | `clippy -D warnings clean; fmt clean; full workspace tests green; tools-list snapshot regenerated for the description caveats and accepted; no pending snapshots; plugin mirrors in sync.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `focused diff review` | `git show d6b3d9c` | PASS | `6 files, all within the gate-finding scope: the walk now mirrors indexer.rs's preprocess/parse/synthesize order exactly (synthesis over original bytes); the config rides into the blocking closure by value and its serialization-hash identity joins the key in BOTH construction sites (prefetch + walk); Absent caches a tombstone so the second walk stays answer-identical; the boundary condition reads skipped.is_empty() at a point where the list can only hold pre-first skips; the upfront mode rejection leaves the in-walk LiteralInsensitive arm as unreachable defense for phase 8.` |

### Trap
Fixing M1 by only changing the parse call. Without adding the config identity to the cache key, the fix itself would poison every cache written before it (pre-fix tombstones for macro symbols) and every future config edit would serve stale fingerprints — the invalidation story is half the fix.

## 6.5: Resolve the cycle-2 material finding: unreadable blob is Operation, not NotFound

### Subtasks
- [x] Map `read_at`'s `find_blob` failure arm to `VcsError::Operation`, reserving `NotFound` for the `lookup_entry`-returned-`None` arm (genuine path absence)
- [x] Regression test: a committed gitlink entry whose commit object is absent reports `Operation`; a genuinely absent path keeps the `NotFound` contract
- [x] Close the cycle-2 undispositioned test gap: drive `history_truncated: true` through the tool via a mock provider and pin that provider truncation alone makes an oldest introduction boundary-ambiguous
- [x] Repair the `window_filled` field doc overclaim (a history exactly `window` revisions long also sets it — the flag is conservative)

### Notes
The severity came from composition: task 6.4 made `NotFound` the deterministic, *cacheable* absence signal (`RevisionInput::Absent` → permanent tombstone keyed by immutable rev). Any read failure mapped to `NotFound` after that turns transient object-store trouble into false `removed`/`introduced` transitions that never heal within a stable binary+config. The fix restores the invariant the deletion feature depends on: `NotFound` means "the tree at this revision provably has no entry at this path", everything else is `Operation` → skip.

Cycle-2 findings NOT fixed here, accepted as recorded follow-ups (joining the 6.4 list): skip-adjacent transitions attribute to the first examined revision after the skip with no per-entry uncertainty marker (the global `skipped` list permits client-side reconstruction); synchronous shard reads in the async prefetch loop (latency, not correctness); config-identity over-invalidation from extraction-irrelevant knobs; the dead in-walk `literal_insensitive` arm's divergent wording; a shared key-builder to make the two `FingerprintKey` construction sites structurally identical.

### Completion Evidence

- Verified: 2026-08-19
- Repository: `.`
- VCS: `git`
- Revision / checkpoint: `e747b14ead60c663d1aba69d26b20b857873e337`
- Identity recheck: `git rev-parse HEAD` at 2026-08-19 13:18 matched `e747b14ead60c663d1aba69d26b20b857873e337`
- Focused review: `git show e747b14ead60c663d1aba69d26b20b857873e337`; complete task diff reviewed for correctness, scope, tests, maintainability, and task boundary
- Reviewed candidate / final: `e747b14ead60c663d1aba69d26b20b857873e337`
- Review result: PASS/Aligned

| Command | Working directory | Result | Observable evidence |
|---|---|---|---|
| `cargo test -p code-graph-vcs-git && cargo test -p code-graph-tools --test symbol_history` | `.` | PASS (`exit 0`) | `14 vcs-git tests passed including the new pin: a gitlink entry whose commit object is absent reports Operation while src/never_existed.rs keeps NotFound. 13 integration tests passed including the new provider-truncation pin: history_truncated:true reaches the wire and alone (window unfilled, nothing skipped) labels the oldest introduction at_window_boundary.` |
| `make verify` | `.` | PASS (`exit 0`) | `clippy -D warnings clean; fmt clean; full workspace tests green; no pending snapshots; plugin mirrors in sync.` |

| Tool / inspection | Context | Result | Observable evidence |
|---|---|---|---|
| `focused diff review` | `git show e747b14` | PASS | `3 files, all within the cycle-2 finding scope: the find_blob arm now formats an Operation error naming the path and revision; the comment explains WHY the mapping is load-bearing (cacheable-absence contract); the new vcs-git test uses relative paths matching the oracle test's convention (the fixture's 8.3 short-form temp path defeats the ownership check otherwise); the truncation test's mock returns truncated:true with one readable commit so only the plumbing under test can produce the asserted flags.` |

### Trap
Mapping ALL of `read_at`'s early failure arms (`rev_parse_single`, `object()`, `peel_to_commit`) to `Operation` too. The fix deliberately touched only the blob arm, where the tree entry's existence makes the semantics unambiguous. Be precise about what the retained early-arm `NotFound` mapping does in the history prefetch: it produces `RevisionInput::Absent` → tombstone (NOT a skip — only `Operation` skips). That composition is acceptable because the poisoning is provably inert, not because it is harmless in kind: an early-arm failure requires a rev emitted by `revisions_touching` to become unresolvable in the sub-second window before prefetch (concurrent `gc --prune=now` + HEAD rewrite + expired reflog), and a pruned rev is unreachable from HEAD, so the HEAD-rooted walk can never re-emit it and the false tombstone is never read again. `blame_symbol` is unaffected either way — its unavailability routing keys on `provider.blame(...)`'s `NotFound`, and its `read_at` use (staleness probe) maps every error to `stale_reason`, never a tombstone.

## Acceptance Criteria

- [ ] **AC-19**: A reformat-only commit is not reported; the logic commit is (FR-33, FR-34).
- [ ] **AC-20**: A commit moving the symbol without changing it is not reported (FR-33).
- [ ] **AC-37**: Historical bytes are parsed in memory with no temporary file written (FR-35).
- [ ] **AC-38**: A reformatted symbol yields an unchanged fingerprint under the formatting-insensitive mode (FR-34).
- [ ] **AC-46**: The cache serves the second query, and recomputes rather than errors when absent or corrupt (FR-37).
- [ ] **AC-45**: The `symbol_history` description meets the agent-facing-description lens, including the rename behaviour (NFR-11).
- [ ] A `symbol_history` call over a large window does not delay a concurrent non-history query (NFR-10).
- [ ] `code-graph-lang` gains no third-party dependency (NFR-02).
- [ ] **AC-27**: `make verify` passes (NFR-04).
- [ ] FR-33, FR-35, FR-37 realized; FR-34 realized for `Normalized` across all languages, with `LiteralInsensitive` completing in phase 8.

## Phase Completion Evidence

Pending — not complete.
