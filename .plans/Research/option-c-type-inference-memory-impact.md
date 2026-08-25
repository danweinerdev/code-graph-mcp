---
title: "Option C receiver-type-inference metadata: cache and RAM impact"
type: research
status: draft
created: 2026-08-25
updated: 2026-08-25
tags: [resolver, confidence, cache, memory, type-inference, option-c]
related: [KNOWN_ISSUES.md, Decisions/decisions.md]
---

# Option C receiver-type-inference metadata: cache and RAM impact

## Context
KNOWN_ISSUES F2 was closed at `68abe00` with Option B: receiver-typed
sole-candidate calls now resolve as `Heuristic/1` (honest, filterable) instead
of falsely `Resolved/1`. Option C — receiver type inference — remains the
long-term direction: infer the receiver's type from declarations, parameters,
return types, and fields, and resolve only on a type match, turning
`Heuristic/1` receiver picks into verified `Resolved` edges (or correctly into
no edge when the receiver is unindexed std/external).

Before any Option C design work, this research quantifies the data cost:
how much would tracking that extra metadata grow the on-disk cache
(`.code-graph-cache.db`, rkyv v13) and resident RAM? All measurements were
taken 2026-08-25 on this repository's release build (`code-graph-bench`) at
candidate `680b6be`, against the pinned dogfood submodules.

## Findings
Measured baseline (release `code-graph-bench`):

| Repo | Files | Symbols | Edges | Cache size | Whole-cache B/symbol |
|---|---|---|---|---|---|
| curl (C/C++) | 1,016 | 6,505 | 50,076 | 4,422,936 B | 680 |
| ripgrep (Rust) | 100 | 3,137 | 15,501 | 1,524,976 B | 486 |

Solving the two datapoints as `cache ~= a*symbols + b*edges + c*files`
(with c ~= 150 B/file for the interned path trie) gives the per-component
serialized costs:

- **~168 B per symbol node**, **~64 B per edge entry**, **~150 B per file**

Type-annotation facts sampled from real source (the strings Option C would
have to store):

- Rust return-type annotations (ripgrep): **2,210 sites, avg 18 B**, only
  **464 distinct** — 79% redundant; the top 6 values cover 42% of all sites.
- C return-type prefixes (curl lib/): avg ~27 B; redundancy visibly higher
  (`CURLcode`/`int`/`void`/`bool` dominate).
- Field declarations: type portion ~15-25 B; ~5-8 fields per type-like symbol.

Load/save latency scales linearly with cache size today: curl saves in
183 ms and loads in 73 ms; ripgrep 54 ms / 15 ms.

### Key Insights
1. **Only interface facts need persistence.** The cross-scope resolution
   contract (fresh files resolve against CACHED symbols; cached edges never
   re-resolve) means the cache needs exactly: return type per callable,
   field name->type per class/struct, and typedef targets. Parameter types
   and local `let`/declaration types are consulted only inside the declaring
   file's own parse — parse-time transient, never persisted.
2. **Type names are massively redundant** (79% in Rust, higher in C), so a
   per-graph type-name interner (u32 ids into one string table) collapses
   the dominant cost: all 464 distinct ripgrep return types total ~12 KB.
3. **Blended per-symbol cache delta** (symbol mix ~70% callables / ~12%
   type-like, measured annotation lengths):
   - Naive strings: **~50-80 B/symbol → +8-12% cache** (C++ template-heavy
     trees up to +15-20%).
   - Interned type names: **~15-20 B/symbol → +3-4% cache**.
4. **Absolute deltas are small.** ripgrep: +~180 KB naive / +~55 KB interned.
   curl: +~370 KB / +~110 KB. UE-scale (72k files, 770k symbols; cache
   extrapolated at ~390-535 MB assuming 5-8 edges/symbol): **+44-62 MB naive,
   +12-15 MB interned**.
5. **RAM follows the cache delta at 2-3x** (String/HashMap heap overhead on
   materialization): UE-scale +90-190 MB naive, **+25-45 MB interned**;
   single-digit MB on ordinary repos. The resolve-time `(Type, method)` and
   field lookup indexes add ~50-100 B per method entry but are transient
   (built beside today's `SymbolIndex`, dropped after resolve; UE-scale peak
   ~30-60 MB). Parse-time scope tables are KBs per file per thread —
   negligible.

### Sources
- `code-graph-bench` runs against `external/curl` and `external/ripgrep`
  (pinned submodules, release build, candidate `680b6be`): graph counts,
  cache sizes, save/load/stale timings.
- Two-datapoint linear solve for per-node/per-edge/per-file serialized costs.
- `rg` sampling over `external/ripgrep/crates` (return-type annotations,
  field declarations, distinct-type counts) and `external/curl/lib`
  (C return-type prefixes).
- CLAUDE.md UE-scale reference workload (72k files, 770k symbols) for
  extrapolation; UE cache size itself is extrapolated, not measured.
- Storage-shape precedent: the Go resolver-metadata sparse framed extension
  in cache v13 (`crates/code-graph-graph/src/persist/`).

## Analysis
Memory is not the blocker for Option C. Even the naive-strings design costs
~+10% cache and low-hundreds-of-MB RAM at UE scale; the interned design —
justified by the measured redundancy, not speculation — lands at ~+3-4% cache
and tens of MB RAM at UE scale, and single-digit MB everywhere else. Load and
save latency grow by the same proportion (linear in cache bytes), so a
+3-12% cache delta is imperceptible next to the parse phase that dominates
analyze wall time.

The dominant real cost of Option C is unchanged from the F2 assessment:
implementation complexity — per-language scope/type tracking across five
parsers (C++, Rust, Python, C#, Java) — not data volume.

### Implications
- **Timing matters for the cache format.** v13 is unreleased (decision
  2026-08-24, no-bump policy). Landing Option C's persisted interface facts
  while v13 is still unreleased is format-free; landing it after release
  forces exactly the `CACHE_VERSION` bump F2/F3 just avoided.
- The Go resolver-metadata extension is the storage precedent to generalize:
  a sparse framed sidecar keyed by path, absent for languages that carry no
  facts, footerless-cache backward compatible.
- `Heuristic/1` (Option B's wire state) is the migration target: Option C
  refines those edges to `Resolved` on type match or drops them on type
  mismatch; no additional wire-format change is required beyond the
  `#[non_exhaustive]` `Confidence` enum's existing headroom.
- The 2-3x RAM multiplier on materialization applies to the EXISTING graph
  too; if resident RAM ever becomes a concern at UE scale, interning helps
  the current `Symbol` strings (signature, namespace, parent) independently
  of Option C.

### Recommendations
1. Design Option C's persisted metadata with a **per-graph type-name
   interner from day one** (u32 ids; measured 79-90% redundancy makes this
   a 3-5x cost reduction for one indirection).
2. Persist **only interface facts** (return types, field tables, typedef
   targets); keep parameter/local inference parse-time transient. This is
   what keeps the delta at +3-4% instead of an unbounded per-call-site cost.
3. Reuse the **framed-extension mechanism** rather than growing `PackedFile`,
   preserving footerless-cache compatibility and per-language sparseness.
4. If Option C is plausibly near-term, prefer landing its cache shape
   **before v13 ships** to users; otherwise budget a `CACHE_VERSION` bump
   (14) into the work.
5. Treat memory as a non-blocker in the Option C design review; gate the
   work on resolver-correctness and per-language complexity instead.

## Open Questions
- What is the REAL UE-scale cache size and edges-per-symbol ratio? The
  390-535 MB baseline here is extrapolated from curl/ripgrep component
  costs; a single `code-graph-bench` run on a UE checkout would replace the
  assumption.
- How long do C++ template type names run in practice on engine code
  (abseil/UE), and does truncation/normalization (e.g. stripping template
  arguments to the primary template name) suffice for resolution while
  bounding string cost?
- Do C# generics and Java erasure need type-argument fidelity in the field
  tables, or is the erased/primary type enough for receiver matching?
- Should the interner be per-graph or per-language? Cross-language type-name
  collisions are harmless for storage but matter if the interner is reused
  as a resolution key.
- Field tables for partial classes (C#): one table per declaration or merged
  at index time?
