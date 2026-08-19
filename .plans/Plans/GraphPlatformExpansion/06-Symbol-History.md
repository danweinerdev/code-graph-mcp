---
title: "Symbol History"
type: phase
plan: GraphPlatformExpansion
phase: 6
status: in-progress
created: 2026-08-08
updated: 2026-08-18
deliverable: "A symbol_history tool reporting the revisions at which a symbol's content actually changed, with a fingerprint hook on LanguagePlugin and a content-addressed fingerprint cache."
tasks:
  - id: "6.1"
    title: "LanguagePlugin::fingerprint_symbol hook with a std-only text default"
    status: complete
    justifies: "FR-34 (Normalized half), NFR-02, AC-38. parse_file returns a FileGraph, not a syntax tree, so there is no way to fingerprint an AST from outside a plugin; the hook is what makes AST overrides possible in phase 8 without leaking tree-sitter across a crate boundary."
    verification: "cargo test -p code-graph-lang fingerprint:: — the default implementation returns an unchanged fingerprint for a symbol whose only change is whitespace or comments (AC-38); LiteralInsensitive returns None from the default rather than silently falling back to Normalized; all six existing plugins compile with no change, proving the hook is additive; cargo tree confirms code-graph-lang gained no third-party dependency (NFR-02)."
  - id: "6.2"
    title: "Fingerprint cache under .code-graph/fingerprints"
    status: in-progress
    justifies: "FR-37, AC-46. A history walk re-reads and re-parses every revision in the window and the parse dominates; without the cache a repeated query pays the whole cost again, and the tombstone case makes pre-existence revisions free."
    verification: "cargo test -p code-graph-tools fingerprint_cache:: — a second identical query is served from cache; deleting the cache directory recomputes the same answer; a corrupted shard recomputes rather than erroring (AC-46); a revision predating the symbol is cached as a tombstone and not re-parsed on the second walk; the cache is separate from .code-graph-cache.db and CACHE_VERSION is unchanged."
    depends_on: ["6.1"]
  - id: "6.3"
    title: "symbol_history tool: transition walk and exact symbol matching"
    status: planned
    justifies: "FR-33, FR-35, NFR-10, NFR-11, AC-19, AC-20, AC-37, AC-45. Distinguishing a logic change from a reformat is the whole value of the feature; without transition comparison the tool degenerates into git log for a file, which the agent could already get."
    verification: "cargo test -p code-graph-tools symbol_history:: against the phase 5 fixture — a reformat-only commit is not reported while the logic commit is (AC-19); a commit moving the function without changing it is not reported (AC-20); historical bytes are parsed in memory with no temporary file written, asserted by watching the temp directory (AC-37); a case-only rename reports Removed then Introduced, matching the exact-match rule; a large-window call does not delay a concurrent non-history query (NFR-10); the tool description meets the agent-facing lens (AC-45)."
    depends_on: ["6.2"]
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
- [ ] Key on `(provider id, RevId, repo-relative path, symbol name, kind, mode, binary identity)`
- [ ] Store the `u64` fingerprint or a tombstone recording absence at that revision
- [ ] Shard on the key's leading bits, one file per shard
- [ ] Treat every read, decode, or key-mismatch failure as a miss; delete and recompute a corrupt shard
- [ ] Confirm no interaction with `.code-graph-cache.db` or `CACHE_VERSION`
- [ ] Tests for hit, cold miss, deleted directory, corrupt shard, and tombstone reuse

### Notes
Revision boundary: fingerprints are cached and the cache is provably safe to lose.

Keying on `RevId` is what makes the correctness argument trivial: revisions identify immutable content in every VCS worth supporting, so a key can never name two different contents and a stale entry cannot be served. That satisfies FR-37 structurally rather than by an invalidation rule — the only kind of cache-correctness argument worth making.

Tombstones matter more than they look. "Not present at this revision" is a real, reusable answer that drives `Introduced` and `Removed`; without caching it, every walk re-parses all the revisions predating the symbol.

### Completion Evidence

Pending — not complete.

## 6.3: symbol_history tool: transition walk and exact symbol matching

### Subtasks
- [ ] Fetch revisions touching the file, bounded by a documented window
- [ ] Give `revisions_touching` a truncation signal (gate artifact 15 follow-up: the revwalk cap currently truncates silently; this tool must not consume it blind) and surface it plus the window-filled state on the wire
- [ ] Walk oldest to newest, computing the fingerprint at each revision
- [ ] Emit only transitions: `Introduced`, `Modified`, `Removed`
- [ ] Match the symbol at each revision by exact, case-sensitive `(name, kind)`
- [ ] Run the whole per-revision loop inside `spawn_blocking`
- [ ] Report unsupported modes and partial windows explicitly; skip and flag revisions that fail to parse
- [ ] Register the tool with a description covering the rename behaviour and the window bound
- [ ] Tests per the verification field

### Notes
Revision boundary: the second history feature is live end to end.

Oldest-to-newest is required: each revision is compared against a known predecessor, and newest-first would need lookahead and would get the `Introduced` boundary wrong at the end of the window. The oldest entry in a bounded window cannot be distinguished from a genuine introduction — say which it is rather than mislabelling it.

The per-revision loop is CPU-bound — `read_at`, then a tree-sitter parse, then a fingerprint that may parse again — so it must run inside `spawn_blocking`. Phase 5's `spawn_blocking` covers the *provider*; this loop is the heavier half and would starve the runtime on its own.

Historical code may not parse with today's grammar. That is expected, not exceptional: skip the revision and flag it.

### Completion Evidence

Pending — not complete.

### Trap
Reporting every revision returned by `revisions_touching`. That is `git log -- <file>`, which the agent can already get and which answers a different question. The tool's entire value is the comparison step that drops revisions where the symbol did not change.

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
