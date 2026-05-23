# CLAUDE.md — code-graph-mcp

Rust workspace, MCP server (rmcp, stdio). Builds in-memory semantic code graphs via tree-sitter; exposes graph-query tools. Languages: C++, Rust, Go, Python, C#, Java.

## Commands

| Action | Make | Cargo |
|---|---|---|
| Build release | `make build` | `cargo build --release -p code-graph-mcp` |
| Test all | `make test` | `cargo test --workspace` |
| Test one crate | — | `cargo test -p <crate>` |
| Lint (deny warnings) | `make lint` | `cargo clippy --workspace --all-targets -- -D warnings` |
| Format check | `make fmt-check` | `cargo fmt --all --check` |
| Snapshots clean | `make snapshot-clean` | fails if `*.snap.new` present |
| Snapshot audit | `make snapshot-audit ARGS="<fragments>"` | — |
| Parse-test harness | — | `cargo run -p code-graph-parse-test -- <dir>` |
| Install pre-commit hooks | `make install-hooks` | sets `core.hooksPath=scripts/hooks` |
| Init dogfood submodules | `make submodules` | `git submodule update --init external/<name>` |

No CGo/C toolchain — tree-sitter grammars build via pure-Rust `cc`. Build natively per target (no cross-compile pipeline). Pre-commit runs `make snapshot-clean`.

## Workspace map

| Crate | Path | Responsibility |
|---|---|---|
| `code-graph-mcp` | `crates/code-graph-mcp` | Binary; rmcp stdio server entry |
| `code-graph-core` | `crates/code-graph-core` | `Symbol`, `Edge`, `SymbolKind`, `EdgeKind`, `Confidence`, `RootConfig` (TOML) |
| `code-graph-lang` | `crates/code-graph-lang` | `LanguagePlugin` trait, `LanguageRegistry`, `SymbolIndex` |
| `code-graph-graph` | `crates/code-graph-graph` | In-memory `Graph` (forward+reverse adjacency, path-trie file/include indexes), rkyv binary cache (v8) at `<project_root>/.code-graph-cache.db`. The single `#![allow(unsafe_code)]` opt-in in the workspace lives here, scoped to the one mmap site in `persist/mmap.rs`. |
| `code-graph-path-trie` | `crates/code-graph-path-trie` | Segment-keyed Patricia trie (`PathTrie<V>`), `PathSet`, `PathInterner`. Backs `Graph.files`/`Graph.includes` and the cache encoder's path interning. `#![forbid(unsafe_code)]`. |
| `code-graph-tools` | `crates/code-graph-tools` | Tool handlers; parallel discovery+indexer; watcher (notify-debouncer-full) |
| `code-graph-parse-test` | `crates/code-graph-parse-test` | Dev binaries: `code-graph-parse-test` (parser-corpus harness) and `code-graph-bench` (repo-agnostic index/cache/verify). Not built into the server. |
| `code-graph-lang-cpp` | `crates/code-graph-lang-cpp` | tree-sitter-cpp v0.23.4 |
| `code-graph-lang-rust` | `crates/code-graph-lang-rust` | tree-sitter-rust v0.24.0 |
| `code-graph-lang-go` | `crates/code-graph-lang-go` | tree-sitter-go v0.25.0 |
| `code-graph-lang-python` | `crates/code-graph-lang-python` | tree-sitter-python v0.25.0 |
| `code-graph-lang-csharp` | `crates/code-graph-lang-csharp` | tree-sitter-c-sharp v0.23.5 |
| `code-graph-lang-java` | `crates/code-graph-lang-java` | tree-sitter-java v0.23.5 |

Cross-language name collisions (e.g. `init` in 5 languages) isolated via `(Language, name)`-keyed index at `crates/code-graph-lang/src/lib.rs:116`.

## Core invariants

- **Tool handler return type:** `Result<CallToolResult, McpError>`. User-visible errors travel as `CallToolResult` with error flag, NOT as `Err`.
- **State guard:** query handlers must call `ServerInner::require_indexed()` first.
- **Paths:** stored file paths are absolute and `\\?\`-prefix-stripped via `dunce` at index time (`code_graph_core::paths::canonicalize`). Incoming file-path args on `get_file_symbols`, `get_coupling`, `get_dependencies`, `generate_diagram(file=…)` are normalized through `code_graph_core::paths::normalize_user_path`. `dunce::simplified` strips `VerbatimDisk` only — `VerbatimUNC` (`\\?\UNC\server\share\…`) passes through unchanged by design; that form rides in symbol IDs for network-share-hosted code.
- **Symbol ID format:** `file:name` (free function) or `file:Parent::name` (method). Paginated tool records omit a separate `file` field — clients recover it by rsplit on the rightmost `:` not part of `::`.
- **Enums:** `SymbolKind`, `EdgeKind` derive Serde and serialize as readable JSON strings (`"function"`, `"calls"`).
- **`LanguagePlugin` trait:** `extensions()`, `parse_file(path, content)`, `preprocess(content, cfg)`, `resolve_edges(symbols, file_graph, registry)`. `preprocess` defaults to `Cow::Borrowed(content)`; override only for byte-level rewrites (e.g. C++ `[cpp].macro_strip`).
- **Logging:** workspace has NO `tracing` dep. `eprintln!` is the channel for out-of-handler warnings (canonical: `crates/code-graph-tools/src/handlers/watch.rs`). If a task says "use `tracing::warn!`", check `Cargo.toml` and flag the deviation — do NOT silently add the dep.
- **Snapshot tests:** `insta`. Snapshots at `crates/code-graph-tools/tests/snapshots/`.

## Known cross-cutting limitations

- **Watch-mode path canonicalization on Windows is dispatch-bounded.** `notify-debouncer-full` delivers `\\?\D:\…` extended-path event paths on Windows (passed through from `ReadDirectoryChangesW`). `process_event_batch` in `crates/code-graph-tools/src/handlers/watch.rs` normalizes every event path via `canonicalize_event_path` BEFORE the registry filter and any graph mutation — `paths::canonicalize` for create/modify (the file exists, returns the on-disk stripped form), `paths::simplify` for remove. Per-platform regression coverage: three Linux-runnable tests pin the dispatch-boundary contract; one `#[cfg(windows)]` test pins the verbatim-prefix strip and runs on manual Windows verification. Without the strip every watched edit would silently insert a duplicate file entry in the graph (the indexer-stored stripped form and the verbatim-form event path produce distinct `PathTrie` keys). `VerbatimUNC` paths still pass through unchanged by design (dunce limitation, see next item).
- **Verbatim UNC unchanged.** `dunce::simplified` strips `VerbatimDisk` only. Network-share-hosted code on Windows carries the extended-UNC form in IDs/paths. Not a regression.
- **Linux CI cannot exercise the `\\?\` strip.** Strip-correctness checks are `#[cfg(windows)]`-gated in `crates/code-graph-core/src/paths.rs`. The strongest cross-platform regression target is `crates/code-graph-tools/tests/path_normalization.rs::four_file_taking_tools_resolve_dot_segment_paths` — it fails on Linux if any handler's `normalize_user_path` wrap is removed.
- **`generate_diagram` rendered-label collapse is lossy in `symbol=` mode only.** BFS dedupes the edge set on the rendered `(from_label, to_label)` pair — NOT raw `SymbolId`, NOT per-edge `direction`. First BFS occurrence wins; repeat label pairs dropped. Consequences: (1) two distinct symbols whose display labels collide (template specializations, same-named methods in unrelated classes, same-named free functions) become ONE diagram edge; (2) under `direction="both"` at depth ≥ 2 a single underlying call `A→B` reachable from both arms is emitted ONCE tagged by whichever arm reached it first. A genuinely bidirectional pair (`A→B` and `B→A`) still survives as two edges. `file=`/`class=` modes dedupe on full path / class-name identity (exact). Clients needing per-arm or ID-level fidelity must call `get_callers`/`get_callees`.
- **`detect_cycles` is NOT byte-budgeted.** Every other paginated tool caps against `[response].max_bytes`; `detect_cycles` is by-COUNT pagination only (`limit`/`offset`). `max_cycle_size` (default 50, `0`→50, max 500) is the only size lever — it shrinks each oversized cycle in place (setting `Cycle.truncated` + `Cycle.original_len`), it does NOT drop cycles. A page of cycles with very large `files` lists can exceed `max_bytes` on the wire. Do NOT route through `byte_budget_take` — the asymmetry is intentional.
- **Claude Code `MCP_TOOL_TIMEOUT` can kill `analyze_codebase` on large codebases.** Hard wall-clock per call; progress notifications do NOT extend it (floor 1000ms). UE4/LLVM-scale trees (72k files, 770k symbols, ~130-200s wall time) can exceed the client default and surface to the agent as `"[Tool result missing due to internal error]"` while the server runs to completion, writes the cache, and returns to idle. **No server-side mitigation rescues this.** Set `MCP_TOOL_TIMEOUT=900000` (15 min) in the environment or `"timeout": 900000` in the per-server MCP config block. **Diagnostic discriminator:** if cache file mtime lands and RSS plateaus before the failure, the server completed and the client gave up — raise the timeout. If RSS is still climbing or no cache wrote, the operation is genuinely in flight or stuck — different problem.

## MCP tools (18)

Tool descriptions in `#[tool(description=…)]` strings (`server.rs`) are **production behavior**, not docs — agents pattern-match on them. Edits to these strings are evaluated under the "Agent-facing tool descriptions" lens (see Quality lenses).

| Group | Tools | Notes |
|---|---|---|
| Indexing | `analyze_codebase` | rkyv binary cache + mtime-based incremental re-index. `force=true` bypasses cache. |
| Symbol query | `get_file_symbols`, `search_symbols`, `get_symbol_detail`, `get_symbol_summary` | All paginated tools: `limit`/`offset` (default 100, max 1000). `get_file_symbols`: `top_level_only`/`brief`; `count_only=true` returns total without records (<1KB). `search_symbols`: `namespace` + `subtree` (PathTrie subtree iter, O(subtree) not O(graph)) + `brief` (default true); returns `SearchSymbolsResponse` (flattened `Page<SymbolResult>` + optional `suggestions`). `get_symbol_summary`: `Page<SummaryRow>`; empty namespace renders `<global>`. |
| Call graph | `get_callers`, `get_callees` | `limit`/`offset`; `min_confidence` (`"any"`/`"resolved"`) drops Heuristic edges at BFS time; response capped at `[response].max_bytes`. |
| Deps | `get_dependencies` | `Page<DependencyEntry>`; `kind` is always `"includes"`; unresolved targets dropped at index time. |
| Structural | `detect_cycles`, `get_orphans`, `get_class_hierarchy`, `get_coupling` | `get_orphans`: `kind` + `subtree` + `reliability` (`"all"`/`"high"` drops virtual + macro-synth false-positives); `limit`/`offset`/`brief` (default 20). `detect_cycles`: by-COUNT pagination only; `subtree` is post-detection filter (cycles crossing the prefix boundary silently dropped); `max_cycle_size` per-cycle cap; default `limit` 20. `get_class_hierarchy`: `depth` + `max_nodes` (default 250, max 1000); `HierarchyNode.ref` stubs in diamonds. `get_coupling`: `outgoing`/`incoming` → `Page<CouplingEntry>`; `both` → `CouplingBoth` (no top-level `results`). |
| Viz | `generate_diagram` | `symbol=` (call graph) / `file=` (file deps) / `class=` (inheritance); `symbol=` takes `direction` (`callees`/`callers`/`both`) + `min_confidence`; `format=edges` rows `{from, to, label, direction}` (`direction` serializes `"calls"`/`"called_by"`). |
| Watch | `watch_start`, `watch_stop` | auto-reindex via notify-debouncer-full. |

### Response shapes

- **`Page<T>` envelope** (`get_orphans`, `get_file_symbols`, `get_callers`, `get_callees`, `search_symbols`, `get_symbol_summary`, `get_dependencies`, `get_coupling` single-direction, `detect_cycles`):
  ```
  { results: T[], total: u32, offset: u32, limit: u32, truncated: bool, next_offset: u32 | null }
  ```
  - `total` = pre-pagination match count. `offset`/`limit` echo *resolved* values (so silent clamp-to-1000 is visible).
  - `limit = 0` → use default.
  - `truncated` / `next_offset` always present (no `skip_serializing_if`); non-truncated page emits `truncated: false`, `next_offset: null`.
  - **Paging-resume contract.** `truncated=true` means the page was cut short by the byte budget (`[response].max_bytes`, default 100KB) before reaching `limit`. Re-call with `offset = next_offset`. `next_offset` always points strictly past the current page's last emitted record. `truncated=false` + `next_offset=null` is natural end.
  - **`limit` is an upper bound, not exact.** Check `truncated` rather than `results.length == limit` — a full byte-capped page can still satisfy `results.length < limit`, and a natural last page satisfies the same inequality without truncation.
  - Sort: `symbol_id` asc (symbol lists); `(depth, symbol_id)` asc for callers/callees (closest hops on page 1).
  - **`count_only` exception** (used by `get_orphans`, `search_symbols`, `get_file_symbols`):
    ```
    { results: [], total, offset: 0, limit: 0, truncated: false, next_offset: null }
    ```
    `limit: 0` is deliberate — caller opted out of paging; envelope stays shape-compatible with `Page<T>` so a single client deserializer covers both modes.
- **`get_callers` / `get_callees`** → `Page<CallChain>`. `CallChain = { symbol_id, file, line, depth }`. **Field semantics:** `symbol_id` is the **definition site** (the callable being reported); `file`/`line` are the **call site** (the `Calls` edge that reached this hop). At depth ≥ 2 they routinely diverge across crates. Clients wanting "where defined?" split `symbol_id` on the rightmost `:` not part of `::`, NOT read `file`. `depth` = BFS distance from requested symbol (1 = direct). **Resolved-only filter:** hops whose target is not a resolved project symbol (same `is_resolved_node` predicate as `generate_diagram`) are dropped at BFS time, never enter `visited`, never appear. Bare-token unresolved callers/callees (`Ok`, `printf`, `unwrap`, `println!`, `fmt.Println`, language builtins/stdlib) are filtered uniformly across all six languages.
- **Non-callable soft-hint (success, not error):** calling `get_callers`/`get_callees` on a `Struct`/`Enum`/`Trait`/`Typedef`/`Interface` returns `CallToolResult` SUCCESS (`is_error: false`) with a **plain-text advisory body** naming the symbol + kind and pointing at `get_class_hierarchy`/`get_symbol_detail`. **The body is NOT the `Page<CallChain>` envelope** — JSON-decoding will fail. Clients pattern-matching the envelope must try plain-text first. Gated strictly on the non-callable kind set: a *callable* symbol (`Function`/`Method`/`Class`) with zero resolved hops still falls through to the empty `Page<CallChain>` envelope, preserving the trichotomy: symbol-not-found → tool error (with optional did-you-mean); non-callable kind → soft-hint success; callable with zero resolved hops → empty envelope.
- **`get_class_hierarchy`** (tree, NOT `Page<T>`):
  ```
  { hierarchy: HierarchyNode, truncated: bool, max_nodes: u32, total_nodes_seen: u32 }
  ```
  - `total_nodes_seen` = unique class names walked (diamond ancestor = 1 slot).
  - `truncated: true` → partial tree well-formed; retry with larger `max_nodes` (≤ 1000).
  - `HierarchyNode = { name, bases?: HierarchyNode[], derived?: HierarchyNode[], ref?: true }`. Walks both directions (no direction arg): `bases` = ancestors (forward `Inherits`), `derived` = descendants (reverse `Inherits`). Empty arms omitted; `ref` present only when `true`.
  - Diamond graphs: first DFS pre-order occurrence is canonical (full `bases`/`derived`); later occurrences are `{name, ref: true}` stubs. Reconstruct by keying a `name -> node` map on first non-ref entries.
  - Cycle-guard halts emit bare `{name}` (no `ref` field) — distinct from ref-stubs: a `{name}` without `ref` is a natural leaf OR cycle halt (both walk-terminal); only `ref: true` resolves back to the map.
  - `HierarchyNode.ref` is `Option<bool>` with `skip_serializing_if = "Option::is_none"`. Only ever `Some(true)`; `ref: false` is never on the wire.
- **`get_symbol_summary`** → `Page<SummaryRow>`. `SummaryRow = { namespace, kind, count }`. Empty namespace → literal `<global>` (rewritten pre-sort, so sorts where `<` lands in ASCII). `kind` byte-identical to other tools' kind spelling via `kind_str`. Sorted by `(namespace, kind)` asc. `count_only=true` total = `(namespace, kind)` pair count, NOT symbol sum.
- **`search_symbols`** → `SearchSymbolsResponse`. `Page<SymbolResult>` envelope `#[serde(flatten)]`-ed (top-level wire shape byte-identical to bare `Page<SymbolResult>`), PLUS optional `suggestions: string[]`. `suggestions` carries `skip_serializing_if = "Vec::is_empty"` — **absent from JSON entirely** when empty (no `"suggestions": []`). Populated ONLY when raw query is anchored (`^…$`, length ≥ 2, non-empty inner) AND `total == 0`; up to 5 candidate symbol-id strings from broad substring match on the anchors-stripped inner. `count_only=true` short-circuits before the suggestion block.
- **`get_dependencies`** → `Page<DependencyEntry>`. `DependencyEntry = { file, kind, line }`. `kind` is **always the string `"includes"`** for every language — Rust `mod`, Python/Go/Java `import`, C# `using`, C++ `#include` all map to `EdgeKind::Includes` (the enum has exactly three variants: `Calls`/`Includes`/`Inherits`; there is no `"imports"`). `line` is the source line. Only includes resolving to an indexed source file appear — system headers, external paths, `.ini`/`.cfg`/`.txt`, anything no plugin claims are filtered at index time. **Rust:** only intra-crate `mod foo;` survives; `use`/`extern crate` dotted tokens (`std::io`, `alloc`) drop at resolve via `RustParser::resolve_include` (intentional scope boundary).
- **`get_coupling`** depends on `direction`:
  - `outgoing` (default) / `incoming` → `Page<CouplingEntry>`. `CouplingEntry = { file, count }` (`count` = call+include edges between the two files in that direction). Sorted by `count` desc, then `file` asc.
  - `both` → `CouplingBoth = { incoming: Page<CouplingEntry>, outgoing: Page<CouplingEntry> }`. **No top-level `results`.** Pages byte-budgeted SEQUENTIALLY: incoming first against the full budget; outgoing gets what remains after incoming + fixed wrapper reserve. If incoming exhausts budget, outgoing returns empty with `truncated: true` and `next_offset: Some(0)` (start-fresh marker). Field-declaration order (`incoming`, `outgoing`) is the wire contract.
  - Other `direction` spelling → tool error. Absent/empty resolves to `outgoing`.
- **`detect_cycles`** → `Page<Cycle>`. `Cycle = { files, truncated, original_len? }`. `files` is one cycle's file paths in canonical sorted order. `Cycle.truncated` is **per-cycle** — `true` only when that cycle's `files` was capped by `max_cycle_size`; `original_len` present only when truncated. `Cycle.truncated` ALWAYS serializes (no `skip_serializing_if`). Two independent `truncated` notions: envelope = more cycles in further pages; per-cycle = one cycle's file list was capped. Neither implies the other. Envelope is by-COUNT only (see Known cross-cutting limitations).
- **`generate_diagram`** — `format=edges` (default) → JSON array of `DiagramEdge = { from, to, label, direction }`; `format=mermaid` → Mermaid flowchart text. `from`/`to` are already-rendered display labels. `symbol=` mode: `label` always `"calls"`. `direction` per-edge tag: `"calls"` (outgoing — `from` calls `to`, forward adjacency) or `"called_by"` (incoming — reverse adjacency); independent of the caller's `direction` request. Empty edge list → `[]`, never `null`. Endpoints not resolving to a real symbol dropped (no file-basename pseudo-node leak). Dedupe key: rendered `(from, to)` in `symbol=` mode (lossy — see Known limitations); full path / class-name identity in `file=`/`class=` modes (exact).

### Edge confidence (Calls / Inherits / Overrides only)

Every `EdgeEntry` carries a `Confidence` tag stamped by the resolver at index time. Two variants today (`Resolved`, `Heuristic`); enum is `#[non_exhaustive]` for future per-language type-inference variants without an MCP wire-format break.

- **`Resolved`** (default): unambiguous target. Either the callee/include name had exactly one indexed candidate, OR the edge is declarative (Inherits / Overrides / Rust `mod`-resolved Includes / suffix-disambiguated includes).
- **`Heuristic`**: ≥ 2 indexed candidates shared the callee/basename; resolver picked via scope rule (same file > same parent > same namespace > global) or first-of-N.

**`min_confidence` filter** on `get_callers`, `get_callees`, `generate_diagram(symbol=…)`: wire spelling `"any"` (default) / `"resolved"`. Applies at each BFS hop — a Heuristic intermediate prunes its entire downstream subtree (intermediate's depth-1 row never enters `visited`, depth-2 callees never surface). Parser: `parse_min_confidence` in `crates/code-graph-tools/src/handlers/mod.rs`; unknown spelling → tool error.

**`get_dependencies` does NOT take `min_confidence`.** `IncludeEntry` does not carry confidence (suffix-disambiguation collapses multi-candidate includes to Resolved 99% of cases).

**`get_orphans` reliability filter is signature-driven (virtual + macro-synth), NOT confidence-based.** Orphan = zero incoming Calls by definition; Heuristic-vs-Resolved applies only to edges that exist, so it has no leverage on the empty-inbound case.

## Configuration (`.code-graph.toml`)

Lives at the **project root** = nearest directory containing `.code-graph.toml`, discovered by upward walk from the `analyze_codebase` invocation path. Walk semantics match cargo / git / rustfmt / editorconfig / npm: first match wins, no merging, walk stops at FS root (returning built-in defaults). Read once per `analyze_codebase` and cached for watch events. Sample at `.code-graph.toml.example` (repo root). All keys optional.

**Discovery + cache + scope split.** Three orthogonal concepts:
- **Config** is project-wide: the discovered toml applies to every invocation under the project root regardless of subtree.
- **Cache** (`<project_root>/.code-graph-cache.db`) is project-wide: co-located with the config so scoped invocations from different subtrees share one cache and accumulate. Leftover `.code-graph-cache.json` from older builds is a separate inode — loader treats as "not present" without explicit format-sniffing; safe to delete.
- **Indexing scope** is invocation-local: `analyze_codebase(<subtree>)` parses only files under `<subtree>` even when the project root is upstream. Config still applies; file walk respects invocation path. Lazy/scoped contract — pay per scope.

**Nested `.code-graph.toml` shadows its ancestor.** Inner toml marks the inner subtree as its own project: discovery from inside the inner subtree stops at the inner toml; cache lands there. No merging.

**No config found anywhere** → built-in defaults + a warning surfaced through `AnalyzeResult.warnings` explicitly calling out that engine-style classes (`class CORE_API Foo`) will not extract.

```toml
[discovery]
extra_ignore = []         # gitignore-syntax globs added to defaults (.git, target, node_modules, …)
                          # SINGULAR. Field name is `extra_ignore` (config.rs); no
                          # `deny_unknown_fields`, so the plural spelling silently no-ops.
max_threads = 0           # 0 = num_cpus

[parsing]
max_threads = 0           # 0 = num_cpus; indexer caps discovery+parse sum at num_cpus

[response]
max_bytes = 102400        # byte cap on paginated MCP responses; mid-page truncation
                          # surfaces `truncated: true` + `next_offset` for paging resume.
                          # Default fits Claude Code's harness; raise for larger budgets.

[cpp]
macro_strip = []          # whole-word identifier tokens overwritten with same-length spaces
                          # before tree-sitter parses. Preserves byte offsets / line / column.
                          # Example: ["CORE_API", "ENGINE_API"] makes
                          # `class CORE_API AActor : public UObject {}` extract correctly.
macro_strip_with_args = [] # identifier-plus-(args) tokens stripped the same way; covers
                          # UCLASS(...), UFUNCTION(...), UPROPERTY(...), GENERATED_BODY(),
                          # DECLARE_*_DELEGATE(...). `\n` preserved during fill so multi-line
                          # arg lists keep line offsets aligned. Token may not appear in both
                          # lists (ConfigError::MacroStripConflict).

[extensions]
# Three semantics:
#   1. <lang> lists ADD extensions to that language.
#   2. User addition WINS over default-claim collision. Two `<lang>` lists
#      claiming the same ext is a load-time error (no tiebreak).
#   3. `disabled` SUPPRESSES, beating defaults AND additions.
# Entries must start with `.`; lowercased at load; empty strings dropped with eprintln!.
# Defaults: cpp=[.cpp .cc .cxx .c .h .hpp .hxx], rust=[.rs], go=[.go],
#           python=[.py .pyi], csharp=[.cs], java=[.java].
disabled = []
cpp = []
rust = []
go = []
python = []
csharp = []
java = []
```

`[response].max_bytes` is consulted from the cached `RootConfig` on each tool call (TOML NOT re-read per query).

### Cache invalidation

- **Format:** rkyv binary archive prefixed by an 8-byte header (`ENDIAN_PROBE: u32 native` = `0x01020304` + `CACHE_VERSION: u32 native`, currently `8`). Endian probe catches cross-endian mmap and routes to silent re-index. Single source of truth: `crates/code-graph-graph/src/persist/packed.rs::CACHE_VERSION`.
- **Version mismatch** on `Graph::load` → `Ok(false)` → caller **silently re-indexes**. No `force=true` required, no transparent migration.
- **mtime-based stale checking.** Changes to `[cpp].macro_strip`, `[cpp].macro_strip_with_args`, or `[extensions]` do NOT retroactively re-parse files with unchanged mtime. Apply with `force=true`.
- **Adding extensions:** new files brought in by `[extensions].<lang>` parse normally on next run (no `force=true`).
- **Watch path** consults cached `RootConfig.extensions` on every reindex — disabled extensions stop reindexing on subsequent edits, but pre-existing graph entries persist until `force=true`.
- **`[response].max_bytes`:** consulted from the cached `RootConfig` on each tool call. To apply a changed value, re-run `analyze_codebase` — **no `force=true` required** (no mtime-based entries affected; value only shapes response output).
- **Cache co-location at project root.** `cache_path = <project_root>/.code-graph-cache.db`. A pre-fix cache at an invocation subdir is reported as "orphan cache" in `AnalyzeResult.warnings`; the new run does NOT touch the orphan, and its own cache lands at the discovered project root.
- **Merge-not-clobber for scoped invocations.** `analyze_codebase(<subtree>)` (no `force`) loads the existing project cache, evicts in-scope files no longer on disk, parses in-scope files, and merges. Files outside scope preserved untouched. Subsequent scoped invocations at sibling subtrees accumulate into the same project graph.
- **Scoped `force=true`** is scope-limited invalidation: drops only entries inside the invoked subtree before re-indexing. Sibling-subtree entries survive. `force=true` at the project root clobbers and rebuilds the whole project.
- **Cross-scope edge resolution is asymmetric by design.** Fresh files' edges resolve against the union of cached + freshly-parsed symbols (fresh-to-cached works). Cached edges from prior invocations do NOT spontaneously re-resolve against newly-added symbols — `force=true` at the originating subtree to re-parse. Bounds resolve cost to the freshly-parsed set. Contract: `crates/code-graph-tools/src/indexer.rs::resolve_edges_with_indexes` doc-comment.
- **Out-of-scope hygiene sweep.** Each `analyze_codebase` checks `Graph::last_sweep_at`; if ≥ `SWEEP_INTERVAL_NANOS` (default 24h, `crates/code-graph-graph/src/persist/mod.rs`) elapsed, runs `Graph::sweep_missing_out_of_scope(invocation_path)` to stat every cached file OUTSIDE the invocation scope and drop the ones no longer on disk. Timestamp persisted in the cache (`last_sweep_at`) so cadence survives restarts.

## Per-language parser facts

Each section: supported patterns, **limitations** (decision-critical for agents recommending behavior).

### C++ — tree-sitter-cpp v0.23.4

Supported:
- Free functions, qualified methods (`Class::method`), inline methods.
- Classes, structs, enums (incl. `enum class`), typedefs, `using` aliases, function-pointer typedefs.
- Operator overloads (in-class and free); auto return types (trailing `-> T` and deduced); nested classes/structs (parent set).
- Lambda call edges.
- Call patterns: free, method, arrow, qualified, template.
- Macro-prefixed classes (`class CORE_API MyClass : public Base {}`) iff listed in `[cpp].macro_strip`. Default (no `[cpp]` section) leaves these broken.
- Parameterized API/reflection macros (`UCLASS(...)`, `UFUNCTION(...)`, `UPROPERTY(...)`, `GENERATED_BODY()`, `DECLARE_*_DELEGATE(...)`) iff listed in `[cpp].macro_strip_with_args`. Scanner uses `find_balanced_close` + `skip_lexical` (walks past strings/comments/raw-strings); `\n` preserved so multi-line arg lists keep line offsets aligned. Pair with `macro_strip` for `<MODULE>_API` bare-token coverage; same token may not appear in both lists.

Limitations:
1. **Macro-generated definitions invisible.** `DEFINE_HANDLER(name)` expansions aren't seen by tree-sitter. Macro invocations that look like calls ARE captured as call edges.
2. **Complex template metaprogramming** → ERROR nodes; parser skips gracefully.
3. **Call resolution heuristic** — same-file > same-class > same-namespace > global. Syntactic, not semantic. Overloads may misresolve.
4. **Cast expressions filtered:** `static_cast`/`dynamic_cast`/`const_cast`/`reinterpret_cast`.
5. **Forward declarations excluded.** Only `function_definition` (with body) emits symbols.
6. **`template_method` node not matched** in v0.23.4 — `obj.foo<T>()` falls through to `field_expression` when possible.
7. **`macro_strip` raw-string-delimiter collision.** Raw string with tag identical to a stripped macro (e.g. `R"CORE_API(…)CORE_API"`) → both delimiters overwritten → tree-sitter fails to close → rest of file becomes ERROR, zero symbols. Silent file-level failure. Workaround: drop the colliding macro from `macro_strip` / `macro_strip_with_args` or rename the raw-string tag.

### Rust — tree-sitter-rust v0.24.0

Supported:
- Free functions; methods in `impl` blocks (`Type::method`); default and abstract trait methods → `Method`/parent=trait.
- Structs, enums (all variant kinds), traits, type aliases.
- Generics (type-bound and where-clause); lifetime parameters.
- `async fn`, `const fn`, `unsafe fn` → `Function` (or `Method` in `impl`).
- **Crate-qualified namespaces.** `Symbol.namespace` rewritten at index time (`RustParser::post_index`) to canonical crate-qualified module path: `crate_name::a::b` for `src/a/b.rs` of a crate whose `[package].name` is `crate_name` (`-` → `_`); `crate_name` alone for `lib.rs`/`main.rs`; `crate_name::a` for `src/a/mod.rs`. `#[path = "x.rs"]` is NOT honored for namespace assignment (`post_index`'s namespace pass does not yet feed `CrateModuleModel::with_path_overrides` from source); `#[path]` IS honored by `mod`-edge resolution — the two are decoupled. Inline `mod a { mod b { … } }` composes onto the crate prefix. **No-Cargo.toml fallback:** inline-`mod`-only namespace; empty namespace still renders `<global>` via `get_symbol_summary`. `mod_item`s themselves do NOT produce Symbol records.
- **`mod`-declaration file edges.** External `mod foo;` declarations emit one file-level `Includes` edge per declaration (`from = declaring file`, `to = resolved child file`, `line = mod_item line`). Resolution in `RustParser::post_index`: `#[path = "x.rs"]` → sibling `dir/foo.rs` → `dir/foo/mod.rs` (first indexed candidate wins; `#[path]` is authoritative — no fall-through if override target unindexed). Inline `mod foo { … }` bodies emit no edge. Edges to unindexed candidates dropped here. This is what makes `get_dependencies`/`detect_cycles`/`generate_diagram(file=)` functional for a Rust crate.
- **Supertrait `Inherits` edges.** `trait Sub: Super { … }` emits one `Inherits` edge per nameable supertrait bound (`from = Sub`, `to = Super`). `trait S: A + B + 'a + ?Sized` → 2 edges (lifetime / `?Sized` / unhandled-bound kinds filtered).
- All `use`-tree forms (simple, scoped, grouped, nested grouped, wildcard, aliased, `self`-in-list, `extern crate`) are extracted into provisional `Includes` edges, but **drop at resolve** via `RustParser::resolve_include` (dotted tokens like `"std::io"` are not absolute paths → `None` → filtered). Intentional scope boundary — `mod`-decl is the sole intra-crate file-dep signal.
- Call patterns: direct, method via `field_expression`, scoped, turbofish, macro invocation, chained.
- Trait impls (`impl Trait for Type`) → `Inherits` edge Type → Trait (incl. generic and `where` clauses).
- **Trait-impl method parent rule:** in `impl Trait for Type { fn m() }`, parent is `Type`, never `Trait`. Trait identity lives ONLY on the `Inherits` edge.

Limitations:
1. **`macro_rules!` definitions NOT symbols.** Only macro *invocations* produce `Calls` edges.
2. **`#[derive(...)]` and proc-macro attributes NOT call edges.** They parse as `attribute_item`, not `macro_invocation`.
3. **Forward declarations excluded — Rust-trait-scoped exception.** A bare `function_signature_item` outside any trait (e.g. inside an `extern "C"` block) → no Symbol, preserving the cross-language invariant. INSIDE a `trait_item`, the abstract signature produces a `Method` with `parent = trait_name` (same classification as default methods in the trait — trait identity rides parent rather than the `Inherits` edge here, because there's no impl context).
4. **Call resolution heuristic** — same scope rule as C++.
5. **Generic parents recorded verbatim.** Methods in `impl<T> Trait for Vec<T>` carry parent `Vec<T>` (not bare `Vec`). `Inherits.from` follows the same rule. → **Hierarchy lookup gap** (`Graph::class_hierarchy` keys by `Symbol.name` bare, walks adjacency by `from` verbose → generic-class walks return leaf).
6. **Inline-nested `mod` declarations unresolved.** A `mod b;` declared inside an inline `mod a { … }` block emits a provisional edge with `to = "b"` (no inline-mod ancestor prefix). `post_index` drops any `mod` edge whose declaring `mod_item` has an inline ancestor, to avoid false edges. Top-level `mod` decls resolve correctly; inline-nested do not.

### Go — tree-sitter-go v0.25.0

Supported:
- Free functions; methods (receiver type as parent — pointer `(s *T)`, value `(s T)`, generic `(s *T[U])` → bare `T`).
- Structs, interfaces, type aliases (`type ID = string`), defined types (`type Count int`, `type Handler func(...)` → `Typedef`).
- Generic functions (Go 1.18+) — type-param list in captured signature; bare name as `Symbol.name`.
- `init()` and `main()` extracted as ordinary functions; no special-casing.
- `package_clause` → `Symbol.namespace`, **rewritten** to module-qualified path (`module_path::a::b`) by `GoParser::post_index` if `go.mod` is discoverable upward. Files outside any discoverable module fall back to bare package name.
- Call patterns: direct, selector (`obj.M()`), package-qualified (`fmt.Println()`), chained, `go fn()`, `defer fn()`, inside `func_literal`.
- Import forms via `import_spec`: single, grouped, aliased (alias dropped, path captured), dot, blank.
- Package-level closure fallback: call inside `var H = func() { foo() }` → `from` = file path (mirrors C++ lambda-at-global-scope).

Limitations:
1. **Structural interface implementation → zero edges.** No `Inherits` for Go. `get_class_hierarchy` on a Go interface returns leaf.
2. **Embedded struct fields → no `Inherits`.** `type T struct { Bar }` is composition.
3. **Call resolution heuristic.**
4. **`go.mod` consulted for namespace derivation only; vendor NOT consulted.** Import-path resolution to file edges is unchanged — import paths recorded verbatim in `Includes.to`; the default basename match against FileIndex is correctly a no-op for module paths. `get_dependencies` does NOT resolve cross-module imports to indexed Go files.
5. **Generic type parameters not in structured fields.** `(s *Server[T])` → parent `Server` (bare). `[T]`/`[T any]`/`[T comparable]` survive only in captured signature text.
6. **Backtick-string imports NOT matched.** Query only matches `interpreted_string_literal`.
7. **Forward declarations excluded.** `method_elem` (no body, interface method element) NOT matched; only `method_declaration`.

### Python — tree-sitter-python v0.25.0

Supported:
- Free functions; methods (parent = enclosing class); nested classes (inner's parent = *immediate* outer, NOT dotted path).
- `async def` → `Function`/`Method`. v0.25 wraps both as `function_definition`; single query path.
- `class` with single, multiple (`class D(A, B, C)` → 3 `Inherits` edges), qualified (`class D(module.Base)` → `to = "module.Base"`). Keyword args in superclasses (`metaclass=Meta`) filtered as non-bases.
- Decorators transparent for definition extraction. `@property`/`@staticmethod`/`@classmethod`/`@abstractmethod`/custom wrap `decorated_definition > function_definition`; queries match inner directly.
- Call patterns: direct, attribute, chained, constructor (`MyClass()` → call to `MyClass`), `super()`, in comprehensions, in `lambda`, in default arg expressions.
- Import forms: `import foo` → `"foo"`; `import foo.bar` → `"foo.bar"`; `import foo as f` → `"foo"`; `from foo import bar` → `"foo"` (**module is the dep, NOT the imported name**); `from foo.bar import baz` → `"foo.bar"`; `from . import utils` → `".utils"` (relative preserved verbatim); `from typing import List, Dict` → 1 edge `"typing"`; `from __future__ import annotations` → `"__future__"`.
- `.pyi` stubs indexed identically to `.py`.

Limitations:
1. **Call resolution especially noisy due to dynamic typing.** `PythonParser` does NOT override `resolve_call`; default heuristic stands (type inference out of scope).
2. **Decorators transparent.** No separate kind; decoration metadata not in symbol record.
3. **Type hints NOT extracted as edges.** Only call sites + explicit imports drive deps.
4. **Conditional imports NOT extracted.** `if TYPE_CHECKING: import x` and `try: import x except ImportError: ...` filtered by module-top-level guard.
5. **`from __future__`** handled via dedicated `future_import_statement` node kind, NOT `import_from_statement`.
6. **No forward declarations in Python.** `.pyi` indexed identically.

### C# — tree-sitter-c-sharp v0.23.5

Supported:
- **Partial classes:** one Class symbol per `partial class Foo` declaration. Cross-declaration merging deferred to hierarchy-walk via bare-name `from` rule on `Inherits` edges, NOT extraction time.
- **Default interface methods (C# 8+):** body-presence discriminator. Body can be `(block ...)` or `(arrow_expression_clause ...)`. Abstract interface methods (no body) → no Symbol.
- **Extension methods:** syntactic parent = static class. `static class Ext { static int Count(this string s) {...} }` → `Count` Method, parent `Ext`. `this string` does NOT remap to `string`.
- **Records → `Class`** (no `SymbolKind::Record`). Methods in `record Foo { void M() {...} }` parent to record name. `record Foo(string Name)` components parse as `formal_parameters`, correctly invisible.
- **Generics verbatim in `Inherits.from`.** `class Foo<T> : Base<T>` → `from = "Foo<T>"`, `to = "Base<T>"`.
- Call patterns: direct, member-access, chained, null-conditional (`obj?.M()`), generic (`Foo<T>()`), constructor (`new Foo()`).
- `using` forms: plain, dotted, `using static`, alias (`using F = System.IO.File;`), `global using`, `using` inside namespace blocks.

Limitations:
1. **`nameof(X)` filtered.** Parses as `invocation_expression` but compile-time operator.
2. **Generic-class hierarchy lookup gap.** `Symbol.name` is bare (`"Foo"`); `Inherits.from` has generics (`"Foo<T>"`). `Graph::class_hierarchy` keys by name but walks adjacency by `from` → halves miss → generic-class walks return leaf. Same shape as Rust/Java.
3. **Call resolution heuristic.** Method overloading + extension-method dispatch may misresolve.
4. **Partial-class search returns N results** (one per declaration file). Agents dedupe by name + group by file.
5. **Forward declarations excluded.** Abstract interface methods → no Symbol.
6. **Records: components invisible.** `record Foo(string Name)` → 1 Class symbol; positional components → `formal_parameters`, NOT Field.

### Java — tree-sitter-java v0.23.5

Supported:
- **Records → `Class`.** Components in `record Foo(String name)` parse as `formal_parameters`, invisible.
- **Anonymous classes invisible to symbol index.** `new Runnable() { void run() {...} }` → NO Class symbol. Methods inside anon classes inherit the *enclosing named entity's* parent (enclosing method, class, or file), NOT a synthetic `Anonymous$1`.
- **Default, static, private (Java 9+) interface methods:** all extract as `Function` via body-presence. Abstract → no Symbol.
- **Enum methods:** both enum-level (`enum E { ; void m() {} }`) AND per-constant bodies (`enum E { A { void m() {} } }`) attribute to enum type as parent. Per-constant boundaries transparent for parent walk.
- Call patterns: direct, member-access, chained, generic (`Foo.<T>bar()`), constructor, `this(...)`/`super(...)` chains, in lambdas/anon-class bodies/enum-constant bodies, identifier-form method refs (`String::length`, `obj::method`).
- Import forms: plain, dotted (`*` wildcard preserved verbatim in `Includes.to`), `import static`, `import static <pkg>.*`.
- Sealed types' `permits` clause ignored. Sealed interfaces/classes extract as ordinary `Interface`/`Class`.

Limitations:
1. **`Type::new` constructor references NOT extracted as `Calls`.** Grammar produces `new` keyword on RHS of `::`. Identifier-form method refs DO extract.
2. **Generic-class hierarchy lookup gap** (same shape as C#). Java's constraints (`<T extends Comparable<T>>`) ride inside `type_parameters` → `Inherits.from` verbose (`Foo<T extends Comparable<T>>`).
3. **Records cannot extend classes** (Java syntax error). `extract_inheritance` handles ERROR nodes via `has_error()`. Records CAN implement interfaces; that path extracts.
4. **Call resolution heuristic.**
5. **Anonymous-class method-name collisions.** Two anon classes in the same enclosing method both defining `run` → two Symbols with the SAME ID (anon invisible to ID-building walk). `Symbol.line` disambiguates at query time. Grouping by ID collapses; `search_symbols` returns both.
6. **Forward declarations excluded.** Abstract interface methods + enum-level abstract → no Symbol.

### Cross-language summary

| Capability | C++ | Rust | Go | Python | C# | Java |
|---|---|---|---|---|---|---|
| Inheritance edges | ✓ | ✓ trait impl + super | ✗ structural | ✓ multi-base | ✓ | ✓ |
| Generic verbatim in `Inherits.from` | n/a | ✓ → lookup gap | n/a | n/a | ✓ → lookup gap | ✓ → lookup gap |
| Forward decls excluded | ✓ | ✓ (trait sig excepted) | ✓ | n/a | ✓ | ✓ |
| Call resolution | heuristic | heuristic | heuristic | heuristic | heuristic | heuristic |

## Test conventions

- **Shared helpers in `super::test_helpers::*`** (canonical: `pub(super)` under `crates/code-graph-tools/src/handlers/mod.rs`). Use `body_text`, `page_parts` from there. Do NOT recreate locally.
- **Diagnostic sentinels before discriminator assertions** in timing/IO-dependent tests (watch-mode reindex is canonical). Assert low-stakes baseline first ("a no-macro class extracts") before the discriminator ("a macro-prefixed class extracts"). Sentinel failure message names the likely root cause (timing, IO, race). Example: `tests/watch_cpp_macro_strip.rs` (`UObject` sentinel before `AActor` check).
- **Gitignored test fixtures need `git add -f`.** `.code-graph.toml` is gitignored, but `RootConfig::load` requires that exact filename (e.g. `testdata/ue/.code-graph.toml`). Run `git check-ignore <path>` — a hit means `-f` is required. `cargo test` runs against local FS and passes locally even when the fixture is silently excluded; only fresh-checkout CI reveals it.

## Dogfood-baseline submodules (optional)

Per-language baseline tests parse a real upstream repo and assert symbol count within ±10% of recorded baseline. Submodules under `external/`, pinned by tag. Tests **auto-skip** with `eprintln!` setup hint if uninitialized — no panic, no `--ignored` opt-in.

| Lang | Submodule | Pin | Baseline |
|---|---|---|---|
| Rust | `external/ripgrep` (BurntSushi/ripgrep) | `15.1.0` | `testdata/rust/ripgrep-baseline.txt` |
| Go | `external/logrus` (sirupsen/logrus) | `v1.9.4` | `testdata/go/logrus-baseline.txt` |
| Python | `external/requests` (psf/requests) | `v2.33.1` | `testdata/python/requests-baseline.txt` |
| C++ | `external/fmt` (fmtlib/fmt) | `12.1.0` | `crates/code-graph-lang-cpp/tests/baselines/fmt.txt` |
| C++ | `external/curl` (curl/curl) | `curl-8_20_0` | `crates/code-graph-lang-cpp/tests/baselines/curl.txt` |
| C++ | `external/abseil-cpp` (abseil/abseil-cpp) | `20260107.1` | `crates/code-graph-lang-cpp/tests/baselines/abseil-cpp.txt` |
| C# | `external/efcore` (dotnet/efcore) | `v8.0.25` | `testdata/csharp/efcore-baseline.txt` |
| Java | `external/commons-lang` (apache/commons-lang) | `rel/commons-lang-3.20.0` | `testdata/java/commons-lang-baseline.txt` |

**SHA bump protocol:** symbol count almost always shifts. Re-measure with the new SHA and update `symbols: N` line + `tag:`/`commit:` headers in the same commit. ±10% tolerance may pass without update, but headers should still match the pinned commit. C++ baselines live next to tests (`crates/code-graph-lang-cpp/tests/baselines/`), NOT `testdata/cpp/`, because tied to submodule versions.

curl is primarily C; tree-sitter-cpp parses C as a (mostly compatible) superset; C++ plugin filters ERROR nodes so per-file parse always succeeds. Aggregate symbol count is the regression contract.

## Quality lenses (repo-local additions to planner:quality-scanner)

Standard 5 lenses (Correctness, Safety, Maintainability, Testing, Over-Engineering) plus:

### Agent-facing tool descriptions

Applies when diff touches `#[tool(description=…)]` strings in `crates/code-graph-tools/src/server.rs` (or analogous fields). These are **production behavior** — agents pattern-match on them. A misleading description (e.g. "raise `offset` for more results" when `offset` is a skip-count) is functionally a bug.

Checklist:
- Every named arg documented with default + ceiling.
- Verb in suggested action operationally produces the claimed result. ✓ "raise `limit` for more results"; ✗ "raise `offset` for more results".
- Response envelope shape named, not implied. Say `{results, total, offset, limit, truncated, next_offset}`; don't make agents guess.
- Hint when non-default values are appropriate ("default 100; raise for symbols with high fan-in" beats "default 100, max 1000" alone).
- Plurality + units match field type.

### Documentation read cold

Applies when diff touches `*.md`, `.code-graph.toml.example`, or other agent-readable docs. Read modified AND surrounding sections *cold* — without context from commit message / task — as a future agent would.

Checklist:
- **Framing contradictions across sibling sections.** A feature documented in two places (e.g. "Supported Patterns" + "Known Limitations") should convey consistent signals.
- **Stale references newly visible.** Adjacent edits double visibility of stale lines.
- **Load-bearing "must contain phrase X" strings.** E.g. `force=true` must appear in both CLAUDE.md and `.code-graph.toml.example`. When diff touches either, `grep -l <phrase>` to confirm survival.
- **Doc promises must match implementation.**

## Architecture diagram

```
AI Agent <-stdio/MCP-> [code-graph-mcp (rmcp server)]
                              |
                     +--------+--------+
                     |                 |
              [Tool Handlers]     [Graph + rkyv v8 cache]
              (code-graph-tools)  (code-graph-graph)
                     |                 |
              [LanguageRegistry]
              (code-graph-lang)
                     |
   [C++] [Rust] [Go] [Python] [C#] [Java]
                     |
   tree-sitter + tree-sitter-{cpp,rust,go,python,c-sharp,java}
```
