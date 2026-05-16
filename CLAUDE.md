# CLAUDE.md — code-graph-mcp

**Audience:** AI agents working in this repo. Optimized for fact lookup, not narrative.
**Project:** Rust workspace, MCP server (rmcp, stdio). Builds in-memory semantic code graphs via tree-sitter; exposes graph-query tools to AI agents. Languages live: C++, Rust, Go, Python, C#, Java.

## Commands

| Action | Make | Cargo |
|---|---|---|
| Build release | `make build` | `cargo build --release -p code-graph-mcp` |
| Test all | `make test` | `cargo test --workspace` |
| Test one crate | — | `cargo test -p <crate>` |
| Lint (deny warnings) | `make lint` | `cargo clippy --workspace --all-targets -- -D warnings` |
| Format check | `make fmt-check` | `cargo fmt --all --check` |
| Snapshots clean | `make snapshot-clean` | — (fails if `*.snap.new` present) |
| Snapshot audit | `make snapshot-audit ARGS="<fragments>"` | — |
| Parse-test harness | — | `cargo run -p code-graph-parse-test -- <dir>` |
| Install pre-commit hooks | `make install-hooks` | — (sets `core.hooksPath=scripts/hooks`) |
| Init dogfood submodules | `make submodules` | `git submodule update --init external/<name>` |

- No CGo/C toolchain needed; tree-sitter grammars build via pure-Rust `cc`. No cross-compile pipeline — build natively per target.
- Pre-commit hook runs `make snapshot-clean`. `--no-verify` only if you understand a stale `*.snap.new` will break CI.

## Workspace map

| Crate | Path | Responsibility |
|---|---|---|
| `code-graph-mcp` | `crates/code-graph-mcp` | Binary; rmcp stdio server entry |
| `code-graph-core` | `crates/code-graph-core` | `Symbol`, `Edge`, `SymbolKind`, `EdgeKind`, `RootConfig` (TOML) |
| `code-graph-lang` | `crates/code-graph-lang` | `LanguagePlugin` trait, `LanguageRegistry`, `SymbolIndex` |
| `code-graph-graph` | `crates/code-graph-graph` | In-memory `Graph` (forward+reverse adjacency, file index), JSON cache |
| `code-graph-tools` | `crates/code-graph-tools` | Tool handlers; parallel discovery+indexer; watcher (notify-debouncer-full) |
| `code-graph-lang-cpp` | `crates/code-graph-lang-cpp` | tree-sitter-cpp; scope-aware call resolution |
| `code-graph-lang-rust` | `crates/code-graph-lang-rust` | tree-sitter-rust; impl/trait, use-tree, macro invocations |
| `code-graph-lang-go` | `crates/code-graph-lang-go` | tree-sitter-go; receivers, import forms, selectors |
| `code-graph-lang-python` | `crates/code-graph-lang-python` | tree-sitter-python; classes/decorators, both import forms, `.py`+`.pyi` |
| `code-graph-lang-csharp` | `crates/code-graph-lang-csharp` | tree-sitter-c-sharp; partial classes, default interface methods, extension methods, records |
| `code-graph-lang-java` | `crates/code-graph-lang-java` | tree-sitter-java; records, anon classes invisible, default/static/private interface methods, enum methods |

Cross-language collisions (e.g. `init` in 5 languages) isolated via `(Language, name)`-keyed index at `crates/code-graph-lang/src/lib.rs:116`.

## Core invariants

- **Tool handler return type:** `Result<CallToolResult, McpError>`. User-visible errors travel as `CallToolResult` with error flag, NOT as `Err`.
- **State guard:** query handlers must call `ServerInner::require_indexed()` first.
- **Paths:** all stored file paths are absolute and `\\?\`-prefix-stripped via [`dunce`](https://crates.io/crates/dunce) at index time (`code_graph_core::paths::canonicalize`); incoming file-path args on `get_file_symbols`, `get_coupling`, `get_dependencies`, and `generate_diagram(file=…)` are normalized through `code_graph_core::paths::normalize_user_path` before lookup. `dunce::simplified` strips only `VerbatimDisk` prefixes — `VerbatimUNC` paths (`\\?\UNC\server\share\…`) pass through unchanged; this is dunce's documented behavior and is the form symbol IDs carry for network-share-hosted code.
- **Symbol ID format:** `file:name` (free function) or `file:Parent::name` (method). Records returned by paginated tools no longer include a separate `file` field; clients recover it via the documented id-to-file split (rsplit on the rightmost `:` not part of `::`).
- **Enums:** `SymbolKind`, `EdgeKind` derive Serde, serialize as readable JSON strings (`"function"`, `"calls"`).
- **`LanguagePlugin` trait:** `extensions()`, `parse_file(path, content)`, `preprocess(content, cfg)`, `resolve_edges(symbols, file_graph, registry)`. `preprocess` defaults to `Cow::Borrowed(content)`; override only for byte-level rewrites (e.g. C++ `[cpp].macro_strip`).
- **Logging:** workspace deliberately has NO `tracing` dep. `eprintln!` is the channel for out-of-handler warnings (canonical example: `crates/code-graph-tools/src/handlers/watch.rs`). If a task says "use `tracing::warn!`", check `Cargo.toml`; flag the deviation, do NOT silently add the dep.
- **Snapshot tests:** `insta`. Snapshots at `crates/code-graph-tools/tests/snapshots/`.

## Known cross-cutting limitations

Per-language parser limitations live in the per-language sections below. The items here cut across crates and warrant top-level visibility.

- **Watch-mode path re-contamination on Windows.** `notify-debouncer-full` may deliver `\\?\`-prefixed event paths during reindex (`crates/code-graph-tools/src/handlers/watch.rs`), re-contaminating a graph that was clean post-`PathNormalization`. Filed as a deferred follow-up plan; see `Designs/PathNormalization/README.md` Non-Goals. The fix would route watch event paths through `paths::simplify` (or, better, `paths::canonicalize`) before merging the reindexed `FileGraph`. Until that lands, a Windows user running watch mode against a UE-style codebase may see `\\?\` strings creep back into symbol IDs after the first watched file modification, defeating the index-time canonicalization done by `analyze_codebase`.
- **Verbatim UNC paths pass through unchanged.** `dunce::simplified` only strips `VerbatimDisk` prefixes; `VerbatimUNC` paths (`\\?\UNC\server\share\…`) survive verbatim by design. For UE-style codebases on local `D:` drives (the primary dogfood scenario) this is invisible. For network-share-hosted code on Windows, symbol IDs and stored paths will carry the extended-UNC form. No fix is in scope; documented so future readers don't mistake the behavior for a regression.
- **Linux CI cannot exercise the `\\?\` strip.** Path normalization is a Windows-only behavior; `dunce::simplified` is documented identity on non-Windows. The repo's load-bearing strip-correctness checks live in `#[cfg(windows)]`-gated unit tests in `crates/code-graph-core/src/paths.rs` and in `#[cfg(windows)]` ground-truth assertions inside `crates/code-graph-graph/src/persist.rs::tests`. The dotty-path test in `crates/code-graph-tools/tests/path_normalization.rs::four_file_taking_tools_resolve_dot_segment_paths` is the strongest cross-platform regression target — it would fail on Linux if any handler's `normalize_user_path` wrap were removed.
- **`generate_diagram` rendered-label collapse is lossy.** The diagram BFS dedupes its edge set on the rendered `(from_label, to_label)` pair only — NOT the raw `SymbolId`, and NOT the per-edge `direction`/orientation. First occurrence in BFS visitation order wins (no merge, no tiebreak); a repeat label pair is dropped outright, keeping the winning arm's `direction` tag. Consequences a recommending agent must know: (1) two distinct symbols whose display labels collide (template specializations, same-named methods in unrelated classes, same-named free functions in different files — anything reducing to the same `parent::name`) become ONE diagram edge — ID-level fidelity is sacrificed for visual coherence by design; (2) under `direction="both"` at depth ≥ 2 a single underlying call `A→B` is reachable from both arms (forward tags it `Calls`, reverse tags the same call `CalledBy`) — it is emitted ONCE tagged by whichever arm reached it first, NOT once per arm; a genuinely bidirectional pair (`A→B` and `B→A`) still survives as two edges because those are distinct label pairs. Clients needing per-arm or ID-level fidelity must call `get_callers`/`get_callees` instead of reading the diagram.
- **`detect_cycles` is NOT byte-budgeted.** Every other paginated tool (`get_orphans`, `get_file_symbols`, `get_callers`, `get_callees`, `search_symbols`, `get_coupling`, `get_dependencies`, `get_symbol_summary`) caps its page against `[response].max_bytes` and surfaces `truncated: true` + `next_offset` when the byte budget bites. `detect_cycles` deliberately does NOT: its envelope `truncated`/`next_offset` are derived purely from `offset`/emitted/`total` (by-COUNT pagination via `limit`/`offset`), never from serialized size — there is no byte budget threaded into the handler. A page of cycles with very large `files` lists can therefore exceed `[response].max_bytes` on the wire; the orthogonal `max_cycle_size` per-cycle file-list cap (default 50, `0`→50, clamped 500) is the only size lever, and it shrinks each oversized cycle in place (setting that `Cycle.truncated` + `Cycle.original_len`) rather than dropping cycles or shortening the page. Do NOT "fix" this by routing `detect_cycles` through `byte_budget_take` — the asymmetry is intentional and load-bearing.

## MCP tools (15)

Tool descriptions in `#[tool(description=…)]` strings (server.rs) are **production behavior**, not docs — agents pattern-match on them. Edits to these strings are evaluated under the "Agent-facing tool descriptions" lens (see Quality lenses below).

| Group | Tools | Notes |
|---|---|---|
| Indexing | `analyze_codebase` | JSON cache + mtime-based incremental re-index. Re-run with `force=true` to bypass cache (see Cache invalidation). |
| Symbol query | `get_file_symbols`, `search_symbols`, `get_symbol_detail`, `get_symbol_summary` | `get_file_symbols`: `top_level_only`/`brief` + `limit`/`offset` (default 100, max 1000); `count_only=true` returns total without records (< 1KB response); response capped at `[response].max_bytes` (default 100KB) — see Response shapes. `search_symbols`: `namespace` + `limit`/`offset` + `brief` (default true); `count_only=true` returns total without records (< 1KB response); returns `SearchSymbolsResponse` = flattened `Page<SymbolResult>` plus an optional `suggestions: string[]` (anchored-zero did-you-mean; absent from wire when empty) — see Response shapes. `get_symbol_summary`: `Page<SummaryRow>` (`{namespace, kind, count}`); empty namespace renders as `<global>`; `count_only=true` total = `(namespace,kind)` pair count — see Response shapes. |
| Call graph | `get_callers`, `get_callees` | `limit`/`offset` (default 100, max 1000); response capped at `[response].max_bytes` (default 100KB) — see Response shapes. |
| Deps | `get_dependencies` | `Page<DependencyEntry>` (`{file, kind, line}`); `kind` is always `"includes"`; only includes resolving to an indexed source file appear (unresolved dropped at index time) — see Response shapes. |
| Structural | `detect_cycles`, `get_orphans`, `get_class_hierarchy`, `get_coupling` | `get_orphans`: `kind` + `limit`/`offset`/`brief` (default 20, max 1000); `count_only=true` returns total without records (< 1KB response); response capped at `[response].max_bytes` (default 100KB) — see Response shapes. `detect_cycles`: `Page<Cycle>` (`{files, truncated, original_len?}`); paginated by COUNT not byte budget; `max_cycle_size` per-cycle file-list cap (default 50, max 500) — see Response shapes and Known limitations. `get_class_hierarchy`: `depth` + `max_nodes` (default 250, max 1000); `HierarchyNode.ref` ref-stub for diamond duplicates — see Response shapes. `get_coupling`: `outgoing`/`incoming` → `Page<CouplingEntry>` (`{file, count}`); `both` → `CouplingBoth` (`{incoming, outgoing}`, two independent pages, no top-level `results`) — see Response shapes. |
| Viz | `generate_diagram` | call graph (`symbol=`) / file deps (`file=`) / inheritance (`class=`); `symbol=` mode takes `direction` (`callees`/`callers`/`both`); `format=edges` rows are `{from, to, label, direction}` (`direction` serializes `"calls"`/`"called_by"`) — see Response shapes and Known limitations (lossy label collapse). |
| Watch | `watch_start`, `watch_stop` | auto-reindex via notify-debouncer-full. |

### Response shapes

- **`Page<T>` envelope** (`get_orphans`, `get_file_symbols`, `get_callers`, `get_callees`, `search_symbols`):
  ```
  { results: T[], total: u32, offset: u32, limit: u32, truncated: bool, next_offset: u32 | null }
  ```
  - `total` = pre-pagination match count.
  - `offset`/`limit` echo the *resolved* values (so silent clamp-to-1000 is visible).
  - `limit = 0` → use default.
  - `truncated` / `next_offset` are always present (no `skip_serializing_if`); a non-truncated page emits `truncated: false` and `next_offset: null` explicitly.
  - **Paging-resume contract.** When `truncated=true`, the page was cut short by the byte budget (`[response].max_bytes`, default 100KB) before reaching `limit`. Re-call with `offset = next_offset` to continue. `next_offset` always points strictly past the current page's last emitted record. `truncated=false` plus `next_offset=null` means the page is the natural end of the result set.
  - **`limit` is an upper bound, not an exact count.** The returned page may have fewer records than `limit` when the byte budget bites. Check `truncated` rather than `results.length == limit` to detect partial pages — a full byte-capped page can still satisfy `results.length < limit`, and a natural last page satisfies the same inequality without truncation.
  - Sort: `symbol_id` asc (symbol lists); `(depth, symbol_id)` asc for callers/callees (closest results page 1).
  - **`count_only` response shape** (subset of `Page<T>`, used by `get_orphans`, `search_symbols`, `get_file_symbols` when `count_only=true`):
    ```
    { results: [], total, offset: 0, limit: 0, truncated: false, next_offset: null }
    ```
    `limit: 0` here is a **deliberate exception** to the "envelope echoes resolved limit" contract above — `count_only` callers opted out of paging entirely, so echoing a would-have-been limit would mislead them into thinking there's a record page to fetch. `total` still reflects the true pre-pagination match count after all filters. The envelope stays shape-compatible with `Page<T>` so a single client deserializer covers both modes.
- **`get_class_hierarchy`** (tree):
  ```
  { hierarchy: HierarchyNode, truncated: bool, max_nodes: u32, total_nodes_seen: u32 }
  ```
  - `total_nodes_seen` = unique class names walked (diamond ancestor = 1 slot, not 1-per-arm).
  - `truncated: true` → partial tree is well-formed; retry with larger `max_nodes` (≤ 1000).
  - `HierarchyNode` = `{ name, bases?: HierarchyNode[], derived?: HierarchyNode[], ref?: true }`. Walks both directions (no direction arg): `bases` = ancestors (forward `Inherits`), `derived` = descendants (reverse `Inherits`). Empty `bases`/`derived` omitted; `ref` present only when `true` (never emitted as false).
  - `HierarchyNode.ref`: in diamond graphs, the FIRST DFS pre-order occurrence of a name is canonical (full `bases`/`derived`); every later occurrence is a `{name, ref: true}` stub with empty `bases`/`derived`. Clients reconstruct the full tree by keying a `name -> node` map on first non-ref occurrences and treating `ref:true` nodes as pointers to the canonical entry. Cycle-guard halts emit a bare `{name}` node with NO `ref` field — semantically distinct from a ref-stub: a `{name}` node without `ref` is a natural leaf OR a cycle halt (both walk-terminal), only `ref:true` resolves back to the map.
  - `HierarchyNode.ref` is a Rust `Option<bool>` with `#[serde(default, skip_serializing_if = "Option::is_none")]` (no `#[serde(rename = …)]` — the `r#ref` raw identifier serializes as the JSON key `ref`). `None` → field absent; the only value ever set is `Some(true)`, so `ref: false` is never on the wire.
- **`get_symbol_summary`** → `Page<SummaryRow>`. Each `SummaryRow` = `{ namespace: string, kind: string, count: u32 }`. `namespace` is the symbol's namespace path; the empty namespace is rendered as the literal display string `<global>` (the rename is applied at row-build time, before the sort, so `<global>` rows sort wherever `<` lands in ASCII). `kind` is the readable kind string (`"function"`, `"method"`, …, byte-identical to every other tool's kind spelling via `kind_str`). Rows sorted by `(namespace, kind)` ascending. Standard `Page<T>` envelope (byte-budgeted; `count_only=true` → sentinel page whose `total` is the number of distinct `(namespace, kind)` pairs, NOT the symbol sum).
- **`search_symbols`** → `SearchSymbolsResponse`. The `Page<SymbolResult>` envelope is `#[serde(flatten)]`-ed, so the top-level wire shape is exactly `{ results, total, offset, limit, truncated, next_offset }` (byte-identical to a bare `Page<SymbolResult>`), PLUS an optional `suggestions: string[]`. `suggestions` carries `skip_serializing_if = "Vec::is_empty"` — an empty list is **absent from the JSON entirely** (no `"suggestions": []` key). It is populated ONLY when the raw query is anchored (`^…$`, length ≥ 2, non-empty inner) AND `total == 0`; entries are up to 5 candidate symbol-id strings from a broad substring match on the anchors-stripped inner pattern. `count_only=true` returns the sentinel page and deliberately never emits `suggestions` (the count_only path short-circuits before the suggestion block).
- **`get_dependencies`** → `Page<DependencyEntry>`. Each `DependencyEntry` = `{ file: string, kind: string, line: u32 }`. `kind` is **always the string `"includes"`** for every language — Rust `use`, Python/Go/Java `import`, C# `using`, C++ `#include` all map to `EdgeKind::Includes` (the enum has exactly three variants: `Calls`, `Includes`, `Inherits`; there is no `"imports"` value). `line` is the source line the include/import directive was observed on. Only includes that resolve to an indexed source file appear: the indexer drops an `Includes` edge unless `resolve_include` returns a path AND the registry recognizes that file's language — ALL unresolved targets (system headers, external paths, `.ini`/`.cfg`/`.txt`, anything no plugin claims) are filtered at index time, never just `.ini`. Standard `Page<T>` envelope.
- **`get_coupling`** — shape depends on `direction`:
  - `direction = "outgoing"` (default) or `"incoming"` → `Page<CouplingEntry>`. Each `CouplingEntry` = `{ file: string, count: u32 }` (`count` = number of call+include edges between the two files in that direction). Rows sorted by `count` descending, then `file` ascending. Standard `Page<T>` envelope.
  - `direction = "both"` → `CouplingBoth` = `{ incoming: Page<CouplingEntry>, outgoing: Page<CouplingEntry> }`. **No top-level `results` array.** The two pages are byte-budgeted SEQUENTIALLY: incoming is sized first against the full budget; outgoing gets what remains after the incoming page plus a fixed wrapper reserve. If incoming exhausts the budget, outgoing comes back empty with `truncated: true` and `next_offset: Some(0)` (start-fresh marker). Field-declaration order (`incoming`, `outgoing`) is the wire contract.
  - Any other `direction` spelling is a tool error. Absent/empty resolves to `outgoing`.
- **`detect_cycles`** → `Page<Cycle>`. Each `Cycle` = `{ files: string[], truncated: bool, original_len?: u32 }`. `files` is one cycle's file paths in canonical sorted order. `Cycle.truncated` is **per-cycle** — it is `true` only when that one cycle's `files` list was capped by `max_cycle_size` (default 50, `0` → 50, clamped at 500); `original_len` is the pre-truncation file count, present ONLY when that cycle was truncated (absent otherwise). `Cycle.truncated` ALWAYS serializes (no `skip_serializing_if`), mirroring the always-present envelope `truncated`. There are TWO independent `truncated` notions: the **envelope's** `truncated` means more cycles in further pages; each **`Cycle.truncated`** means that one cycle's file list was capped — neither implies the other. The envelope's `truncated`/`next_offset` are honest and by-COUNT only — the `[response].max_bytes` byte budget does **not** apply to `detect_cycles` (see Known limitations). Default `limit` is 20 (`0` → 20), clamped at 1000; `offset` default 0.
- **`generate_diagram`** — `format = "edges"` (default) returns a JSON array of `DiagramEdge` = `{ from, to, label, direction }`; `format = "mermaid"` returns Mermaid flowchart text. `from`/`to` are already-rendered display labels. For `symbol=` (call-graph) mode `label` is always the string `"calls"`. `direction` is a per-edge orientation tag that serializes as `"calls"` (outgoing — `from` calls `to`, discovered via forward adjacency) or `"called_by"` (incoming — `from` is an inbound caller of `to`, discovered via reverse adjacency); it is independent of the caller's `direction` request (`"callees"`/`"callers"`/`"both"`, symbol mode only). Empty edge list serializes as `[]`, never `null`. Edges whose endpoints don't resolve to a real symbol are dropped (no file-basename pseudo-node leak). The diagram edge set is deduped on the rendered `(from_label, to_label)` pair — see Known limitations for the lossy collapse semantics.

## Configuration (`.code-graph.toml`)

Lives at `<root>/.code-graph.toml`. Read once per `analyze_codebase` and cached for watch events. Sample at `.code-graph.toml.example` (repo root). All keys optional.

```toml
[discovery]
extra_ignore = []         # gitignore-syntax globs added to defaults (.git, target, node_modules, …)
                          # SINGULAR. The serde field is `extra_ignore` (config.rs); there is no
                          # `deny_unknown_fields`, so the accidental plural spelling silently no-ops.
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
                          # parameterized UE reflection macros UCLASS(...), UFUNCTION(...),
                          # UPROPERTY(...), GENERATED_BODY(), DECLARE_*_DELEGATE(...).
                          # `\n` bytes are preserved during the fill so multi-line arg lists
                          # keep line offsets aligned. Token may not appear in both lists.
                          # See `.code-graph.toml.example` for the recommended UE preset.

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

- **`[response].max_bytes`**: byte cap on paginated tool responses. Default 102400. **Consulted from the cached `RootConfig` on each tool call** (the TOML file is NOT re-read per query).

### Cache invalidation

- mtime-based stale checking. Changes to `[cpp].macro_strip`, `[cpp].macro_strip_with_args`, or `[extensions]` do NOT retroactively re-parse files with unchanged mtime.
- To apply new `macro_strip`, new `macro_strip_with_args`, or to evict entries moved to `[extensions].disabled`: re-run `analyze_codebase` with `force=true`.
- Adding extensions: new files brought in by `[extensions].<lang>` parse normally on next run.
- Watch path consults cached `RootConfig.extensions` on every reindex — disabled extensions stop reindexing on subsequent edits, but pre-existing graph entries persist until `force=true`.
- `[response].max_bytes` is consulted from the cached `RootConfig` on each tool call (the TOML file is NOT re-read per query). To apply a changed value, re-run `analyze_codebase` — **no `force=true` required**, because no mtime-based cache entries are affected; the value only shapes response output.
- **Path-string migration on load.** JSON caches written before the `PathNormalization` plan (containing `\\?\D:\…` extended-path strings throughout `nodes`/`adj`/`radj`/`files`/`includes`/`mtimes`) auto-migrate during `Graph::load` via `paths::simplify` (see `crates/code-graph-graph/src/persist.rs::simplify_cache`). **No `force=true` required** — the migration runs unconditionally on every load and is a no-op on already-clean caches. `stale_paths` is deliberately exempt from the migration; see the inline comment at the `stale_paths` deserialization site for the rationale.
- **`CACHE_VERSION` bump (include entries now carry source lines).** `CACHE_VERSION` is `3` (`crates/code-graph-graph/src/persist.rs`). Include entries in the cache went from a bare path list to per-include `{path, line}` records so `get_dependencies` can report the source line of each include/import. The shape change is **not** backward-compatible: a v1 (Go-written) or v2 cache fails the version check on `Graph::load`, which surfaces as `Ok(false)` so the caller **silently re-indexes — no `force=true` required and no transparent migration is attempted** (distinct from the path-string migration above, which is in-place). The unresolved-include filter (drop any `Includes` edge that doesn't resolve to an indexed-language source file — see `resolve_all_edges` in `crates/code-graph-tools/src/indexer.rs`) lives at the indexer/edge-resolution layer, so every re-indexed graph is clean of `.ini`/`.txt`/system-header/unresolvable include edges going forward without `force=true`.

## Per-language parser facts

Each section: grammar pin, supported patterns, **known limitations** (decision-critical for agents recommending behavior).

### C++ — tree-sitter-cpp v0.23.4

Supported:
- Free functions, qualified methods (`Class::method`), inline methods.
- Classes, structs, enums (incl. `enum class`), typedefs, `using` aliases, function-pointer typedefs.
- Operator overloads (in-class and free); auto return types (trailing `-> T` and deduced); nested classes/structs (parent set).
- Lambda call edges.
- All call patterns: free, method, arrow, qualified, template.
- Macro-prefixed classes (`class CORE_API MyClass : public Base {}`) iff listed in `[cpp].macro_strip`. Default (no `[cpp]` section) leaves these broken — zero behavior change for non-UE users.
- Parameterized API/reflection macros (`UCLASS(...)`, `UFUNCTION(...)`, `UPROPERTY(...)`, `GENERATED_BODY()`, `DECLARE_*_DELEGATE(...)`, etc.) iff listed in `[cpp].macro_strip_with_args`. The scanner uses `find_balanced_close` + `skip_lexical` to walk parens past strings/comments/raw-strings; `\n` bytes inside the matched span are preserved so multi-line arg lists keep line offsets aligned. Default (no `[cpp].macro_strip_with_args`) leaves these broken — zero behavior change for non-UE users. Pair with `macro_strip` for `<MODULE>_API` bare-token coverage; same token may not appear in both lists (`ConfigError::MacroStripConflict`). See `Designs/UeMacroSupport` for the full UE preset.

Limitations:
1. **Macro-generated definitions invisible.** `DEFINE_HANDLER(name)` expansions aren't seen by tree-sitter. Macro invocations that look like calls ARE captured as call edges.
2. **Complex template metaprogramming** → ERROR nodes; parser skips gracefully.
3. **Call resolution heuristic** — same-file > same-class > same-namespace > global. Syntactic, not semantic. Overloads may misresolve.
4. **Cast expressions filtered:** `static_cast`/`dynamic_cast`/`const_cast`/`reinterpret_cast` (tree-sitter parses as calls).
5. **Forward declarations excluded.** Only `function_definition` (with body) emits symbols.
6. **`template_method` node not matched** in v0.23.4 — `obj.foo<T>()` falls through to `field_expression` when possible.
7. **`macro_strip` / `macro_strip_with_args` raw-string-delimiter collision.** Raw string with tag identical to a stripped macro (e.g. `R"CORE_API(…)CORE_API"` or `R"UCLASS(…)UCLASS"`) → both delimiters overwritten → tree-sitter fails to close → rest of file becomes ERROR, zero symbols. Silent file-level failure. The two-pass interaction makes this slightly worse: pass-1 (whole-word `strip_macros`) has no lexical awareness and rewrites raw-string tags it shouldn't; once the opening tag is blanked, pass-2's `skip_lexical` no longer recognizes the raw-string boundary and may scan into what was previously a raw-string body. Workaround: drop the colliding macro from `macro_strip` / `macro_strip_with_args` for the affected file, or rename the raw-string tag.

### Rust — tree-sitter-rust v0.24.0

Supported:
- Free functions; methods in `impl` blocks (`Type::method`); default methods in `trait` blocks (extracted as `Function`, NOT `Method` — only `impl` ancestry promotes to `Method`).
- Structs, enums (all variant kinds), traits, type aliases.
- Generics (type-bound and where-clause); lifetime parameters.
- `async fn`, `const fn`, `unsafe fn` → `Function` (or `Method` in `impl`).
- Nested modules → `Symbol.namespace = "a::b"`; `mod_item`s themselves do NOT produce Symbol records (namespace anchors only).
- All `use`-tree forms (simple, scoped, grouped, nested grouped, wildcard, aliased records the path, `self`-in-list, `extern crate`).
- All call patterns: direct, method via `field_expression`, scoped, turbofish, macro invocation, chained.
- Trait impls (`impl Trait for Type`) → `Inherits` edge Type → Trait (incl. generic impls and `where` clauses).
- **Trait-impl method parent rule:** in `impl Trait for Type { fn m() }`, parent is `Type`, never `Trait`. Trait identity lives ONLY on the `Inherits` edge.

Limitations:
1. **`macro_rules!` definitions NOT symbols.** Only macro *invocations* produce `Calls` edges. Anti-regression test: `macro_rules_definition_produces_zero_symbols` in `code-graph-lang-rust`.
2. **`#[derive(...)]` and proc-macro attributes NOT call edges.** They parse as `attribute_item`, not `macro_invocation`.
3. **Forward declarations excluded.** `function_signature_item` (no body) → no Symbol. Only `function_item` matches.
4. **Call resolution heuristic** — same scope rule as C++.
5. **Generic parents recorded verbatim.** Methods in `impl<T> Trait for Vec<T>` carry parent `Vec<T>` (not bare `Vec`). `Inherits.from` follows the same rule. → **Hierarchy lookup gap** (see C# limitation 2; same shape).

### Go — tree-sitter-go v0.25.0

Supported:
- Free functions; methods (receiver type as parent — pointer `(s *T)` and value `(s T)`, incl. generic receivers `(s *T[U])` → bare `T` recorded).
- Structs, interfaces, type aliases (`type ID = string`), defined types (`type Count int`, `type Handler func(...)` → `Typedef`).
- Generic functions (Go 1.18+) — type-param list in captured signature; bare name as `Symbol.name`.
- `init()` and `main()` extracted as ordinary functions; no special-casing.
- `package_clause` → `Symbol.namespace` (Go packages are flat; no nested module path).
- All call patterns: direct, selector (`obj.M()`), package-qualified (`fmt.Println()`), chained (one edge per chain link), `go fn()`, `defer fn()`, calls inside `func_literal`.
- All import forms via `import_spec`: single, grouped, aliased (alias dropped, path captured), dot, blank.
- Package-level closure fallback: call inside `var H = func() { foo() }` → `from` = file path (mirrors C++ lambda-at-global-scope).

Limitations:
1. **Structural interface implementation → zero edges.** No `Inherits` edges for Go. `get_class_hierarchy` on Go interface returns leaf. Anti-regression: `crates/code-graph-tools/tests/mixed_language.rs::get_class_hierarchy_for_go_interface`.
2. **Embedded struct fields → no `Inherits`.** `type T struct { Bar }` is composition, not inheritance. Anti-regression: `embedded_struct_field_produces_no_inherits_edge` in `code-graph-lang-go`.
3. **Call resolution heuristic.**
4. **`go.mod`/vendor NOT consulted.** Import paths recorded verbatim in `Includes.to`. Default basename match against FileIndex is correctly a no-op for module paths.
5. **Generic type parameters not in structured fields.** `(s *Server[T])` → parent `Server` (bare). `[T]`/`[T any]`/`[T comparable]` survive only in captured signature text.
6. **Backtick-string imports NOT matched.** Valid grammar but non-idiomatic; query only matches `interpreted_string_literal`. Anti-regression: `backtick_import_produces_no_includes_edge`.
7. **Forward declarations excluded.** `method_elem` (no body, interface method element) NOT matched; only `method_declaration`.

### Python — tree-sitter-python v0.25.0

Supported:
- Free functions; methods (parent = enclosing class); nested classes (inner class's parent = *immediate* enclosing outer, NOT dotted path).
- `async def` → `Function`/`Method`. v0.25 wraps both as `function_definition` (no separate `async_function_definition`); single query path.
- `class` with single, multiple (`class D(A, B, C)` → 3 `Inherits` edges), qualified (`class D(module.Base)` → `to = "module.Base"`) inheritance. Keyword args in superclasses (`metaclass=Meta`, `total=False`) filtered as non-bases.
- Decorators transparent for definition extraction. `@property`/`@staticmethod`/`@classmethod`/`@abstractmethod`/custom wrap `decorated_definition > function_definition`; queries match inner directly.
- All call patterns: direct, attribute, chained (one edge per chain link), constructor (`MyClass()` → call to `MyClass`), `super()`, calls in comprehensions, calls in `lambda` (transparent for enclosing-function walk), calls in default arg expressions.
- All import forms: `import foo` → `"foo"`; `import foo.bar` → `"foo.bar"`; `import foo as f` → `"foo"`; `from foo import bar` → `"foo"` (**module is the dep, NOT the imported name**); `from foo.bar import baz` → `"foo.bar"`; `from . import utils` → `".utils"` (relative preserved verbatim); `from typing import List, Dict` → 1 edge `"typing"`; `from __future__ import annotations` → `"__future__"`.
- `.pyi` stubs indexed identically to `.py`. `def f(x: int) -> str: ...` parses as `function_definition` → Function symbol.

Limitations:
1. **Call resolution especially noisy due to dynamic typing.** `PythonParser` does NOT override `resolve_call`; default heuristic stands as the documented contract (rationale: type inference is out of scope).
2. **Decorators transparent.** No separate kind for `@property`/`@staticmethod`/`@classmethod`/`@abstractmethod`; decoration metadata not in symbol record.
3. **Type hints NOT extracted as edges.** Only call sites + explicit imports drive deps.
4. **Conditional imports NOT extracted.** `if TYPE_CHECKING: import x` and `try: import x except ImportError: ...` filtered by module-top-level guard. Anti-regression tests cover both forms.
5. **`from __future__` handled via dedicated `future_import_statement` node kind**, NOT `import_from_statement`.
6. **No forward declarations in Python.** `.pyi` indexed identically.
7. **Method dispatch heuristic.**

### C# — tree-sitter-c-sharp v0.23.5

Supported:
- **Partial classes**: one Class symbol per `partial class Foo` declaration (Decision 3). Cross-declaration merging deferred to hierarchy-walk via bare-name `from` rule on `Inherits` edges, NOT extraction time.
- **Default interface methods (C# 8+)**: body-presence discriminator. Body can be `(block ...)` or `(arrow_expression_clause ...)`. Abstract interface methods (no body) → no Symbol.
- **Extension methods**: syntactic parent = static class. `static class Ext { static int Count(this string s) {...} }` → `Count` Method, parent `Ext`. `this string` does NOT remap to `string`.
- **Records → `Class`** (no `SymbolKind::Record`). Methods in `record Foo { void M() {...} }` parent to record name. `record Foo(string Name)` components parse as `formal_parameters`, correctly invisible.
- **Generics verbatim in `Inherits.from`.** `class Foo<T> : Base<T>` → `from = "Foo<T>"`, `to = "Base<T>"`.
- All call patterns: direct, member-access, chained, null-conditional (`obj?.M()`), generic (`Foo<T>()`), constructor (`new Foo()`).
- All `using` forms: plain, dotted, `using static`, alias (`using F = System.IO.File;`), `global using`, `using` inside namespace blocks.

Limitations:
1. **`nameof(X)` filtered.** Parses as `invocation_expression` but semantically a compile-time operator. Same precedent as C++ cast filtering.
2. **Generic-class hierarchy lookup gap.** `Symbol.name` is bare (`"Foo"`); `Inherits.from` has generics (`"Foo<T>"`). `Graph::class_hierarchy` keys by `Symbol.name` but walks adjacency by `from` string → halves miss → generic-class walks return leaf. Same accepted limitation as Rust/Java.
3. **Call resolution heuristic.** Method overloading + extension-method dispatch may misresolve.
4. **Partial-class search returns N results** (one per declaration file). File path is the disambiguator. Agents expecting one hit per type name must dedupe by name + group by file.
5. **Forward declarations excluded.** Abstract interface methods → no Symbol.
6. **Records: components invisible.** `record Foo(string Name)` → 1 Class symbol; positional components parse as `formal_parameters`, NOT promoted to Field.

### Java — tree-sitter-java v0.23.5

Supported:
- **Records → `Class`** (Decision 6). Components in `record Foo(String name)` parse as `formal_parameters`, invisible.
- **Anonymous classes invisible to symbol index** (Decision 4). `new Runnable() { void run() {...} }` → NO Class symbol. Methods inside anon classes inherit the *enclosing named entity's* parent (enclosing method, class, or file), NOT a synthetic `Anonymous$1`.
- **Default, static, private (Java 9+) interface methods**: all extract as `Function` via body-presence (Decision 11). Abstract interface methods → no Symbol.
- **Enum methods** (Decision 12): both enum-level (`enum E { ; void m() {} }`) AND per-constant bodies (`enum E { A { void m() {} } }`) attribute to enum type as parent. Per-constant boundaries transparent for parent walk.
- All call patterns: direct, member-access, chained, generic (`Foo.<T>bar()`), constructor (`new Foo()`), `this(...)`/`super(...)` chains, calls in lambdas/anon-class bodies/enum-constant bodies (all transparent for enclosing-function walk), identifier-form method refs (`String::length`, `obj::method`).
- All `import` forms: plain, dotted (`*` wildcard preserved verbatim in `Includes.to`), `import static`, `import static <pkg>.*`.
- Sealed types' `permits` clause ignored (Decision 6). Sealed interfaces/classes extract as ordinary `Interface`/`Class`.

Limitations:
1. **`Type::new` constructor references NOT extracted as `Calls`.** Grammar produces `new` keyword on RHS of `::`. Pinned by no-edge test. Identifier-form method refs DO extract.
2. **Generic-class hierarchy lookup gap** (same shape as C#). Java's constraints (`<T extends Comparable<T>>`) ride along inside `type_parameters`, so `Inherits.from` is verbatim verbose (`Foo<T extends Comparable<T>>`). Both honor Decision 9 ("preserved verbatim"); Java is just noisier.
3. **Records cannot extend classes** (Java syntax error). `extract_inheritance` handles ERROR nodes via `has_error()`. Records CAN implement interfaces; that path extracts.
4. **Call resolution heuristic.**
5. **Anonymous-class method-name collisions.** Two anon classes in the same enclosing method both defining `run` → two Symbols with the SAME Symbol ID (anon class invisible to ID-building walk). `Symbol.line` disambiguates at query time. Grouping by ID collapses; `search_symbols` returns both.
6. **Forward declarations excluded.** Abstract interface methods + enum-level abstract methods → no Symbol.

### Cross-language summary

| Capability | C++ | Rust | Go | Python | C# | Java |
|---|---|---|---|---|---|---|
| Inheritance edges | ✓ | trait impl | ✗ (structural) | ✓ multi-base | ✓ | ✓ |
| Generic verbatim in `Inherits.from` | n/a | ✓ → lookup gap | n/a | n/a | ✓ → lookup gap | ✓ → lookup gap |
| Forward decls excluded | ✓ | ✓ | ✓ | n/a | ✓ | ✓ |
| Call resolution | heuristic | heuristic | heuristic | heuristic | heuristic | heuristic |

## Test conventions

- **Shared helpers in `super::test_helpers::*`.** Use `body_text`, `page_parts` from there (canonical: `pub(super)` under `crates/code-graph-tools/src/handlers/mod.rs`). Do NOT recreate locally; codebase already cleaned this up.
- **Diagnostic sentinels before discriminator assertions** in timing/IO-dependent tests (watch-mode reindex is canonical). Assert low-stakes baseline first ("a no-macro class extracts") before the discriminator ("a macro-prefixed class extracts"). Sentinel failure message names the likely root cause (timing, IO, race). Example: `tests/watch_cpp_macro_strip.rs` (`UObject` sentinel before `AActor` check).
- **Gitignored test fixtures need `git add -f`.** `.code-graph.toml` is gitignored, but `RootConfig::load` requires that exact filename (e.g. `testdata/ue/.code-graph.toml`). Run `git check-ignore <path>` — a hit means `-f` is required. `cargo test` runs against local FS and passes locally even when the fixture is silently excluded; only a fresh-checkout CI reveals it.

## Dogfood-baseline submodules (optional)

Per-language baseline tests parse a real upstream repo and assert symbol count is within ±10% of recorded baseline. Submodules under `external/`, pinned by tag. **Tests auto-skip** with an `eprintln!` setup hint if uninitialized — no panic, no `--ignored` opt-in needed.

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

**SHA bump protocol:** symbol count almost always shifts. Re-measure with the new SHA and update `symbols: N` line + `tag:`/`commit:` headers in the same commit. ±10% tolerance may pass without an update, but headers should still match the pinned commit. fmt/curl/abseil baselines live next to their tests (`crates/code-graph-lang-cpp/tests/baselines/`), NOT under `testdata/cpp/`, because they're tied to submodule versions, not in-tree synthetic fixtures.

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

History: caught two real agent-misleading bugs in CppMacroStrip Phase 4 (`get_callers`/`get_callees` "raise via offset" and over-confident "typical depth=1/2 walks" on `get_class_hierarchy`).

### Documentation read cold

Applies when diff touches `*.md`, `.code-graph.toml.example`, or other agent-readable docs. Read modified AND surrounding sections *cold* — without context from commit message / task — as a future agent would.

Checklist:
- **Framing contradictions across sibling sections.** A feature documented in two places (e.g. "Supported Patterns" + "Known Limitations") should convey consistent signals. Caught in CppMacroStrip Phase 3.
- **Stale references newly visible.** "The sample `.code-graph.toml` ships at the repo root" is wrong — file is `.code-graph.toml.example`. New adjacent content doubles visibility of stale lines; fix them.
- **Load-bearing "must contain phrase X" strings.** Example: `force=true` must appear in both CLAUDE.md and `.code-graph.toml.example` for cache invalidation. When diff touches either, `grep -l <phrase> CLAUDE.md .code-graph.toml.example` to confirm survival.
- **Doc promises must match implementation.** "Default is 250" must match what code resolves; "supports X" must match what's wired through.

## Architecture diagram

```
AI Agent <-stdio/MCP-> [code-graph-mcp (rmcp server)]
                              |
                     +--------+--------+
                     |                 |
              [Tool Handlers]     [Graph]
              (code-graph-tools)  (code-graph-graph)
                     |                 |
              [LanguageRegistry]  [In-memory graph + JSON cache]
              (code-graph-lang)
                     |
   [C++] [Rust] [Go] [Python] [C#] [Java]
                     |
   tree-sitter + tree-sitter-{cpp,rust,go,python,c-sharp,java}
```
