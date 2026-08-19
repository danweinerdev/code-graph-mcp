//! Typed core for `get_callers`/`get_callees` (via `callers_or_callees`),
//! `find_overrides`, `get_dependencies`, and `find_path`.
//!
//! All four are GATED tools (confirmed against the `server.rs` call
//! sites): each function here calls the core-level [`require_indexed`] at
//! its own entry — a NEW call site per Design Decision 8, since
//! `handlers::query` never contained one.
//!
//! `handlers::query`'s four public functions keep their exact signatures
//! (no `ServerInner`/indexed-flag parameter — Decision 3), so they cannot
//! supply a real indexed flag to the core. Each adapter hardcodes
//! `indexed = true`: the MCP path already ran `ServerInner::require_indexed`
//! in `server.rs` before ever reaching the handler, so `true` is always
//! correct on that path. The `indexed` parameter exists so a future direct
//! caller (e.g. Track B's CLI) can pass its own real flag and get the
//! domain error `handlers::query`'s existing tests never had to exercise.
//!
//! Pure-logic helpers (`Direction`, `is_non_callable_kind`,
//! `symbol_id_basename`, `article`, `alternative_tool_hint`,
//! `is_virtual_signature`) stay in `handlers::query` rather than moving
//! here, mirroring Decision 4's precedent for `SearchSymbolsInput`/
//! `GenerateDiagramInput`: `handlers::query`'s own `#[cfg(test)]` module
//! asserts against them directly via `use super::*`, and moving them would
//! force edits to those (deliberately unmodified, Decision 6) tests.

use code_graph_core::{paths, EdgeKind};
use code_graph_graph::{CallChain, Graph};
use parking_lot::RwLock;

use crate::core::{require_indexed, ToolError, ToolOk, ToolResult};
use crate::handlers::query::{
    alternative_tool_hint, article, is_non_callable_kind, is_virtual_signature, symbol_id_basename,
    Direction,
};
use crate::handlers::{
    byte_budget_take, edge_kind_str, kind_str, parse_min_confidence, suggest_symbols,
    CallChainResponse, DependencyEntry, FindPathResponse, Page,
};

/// Re-exported so a second front-end (the CLI, Designs/CommandLineInterface
/// Decision 1) can select the walk arm without importing `handlers`.
pub use crate::handlers::query::Direction as CallDirection;

/// `callers_or_callees` body. Body moved verbatim from
/// `handlers::query::callers_or_callees`, plus the core `require_indexed`
/// call at entry (Decision 8).
///
/// **The trichotomy this function must preserve exactly (task 2.3's Trap):**
/// 1. symbol not found → `Err(ToolError)`, including the did-you-mean text.
/// 2. symbol found but a non-callable kind (`Struct`/`Enum`/`Trait`/
///    `Typedef`/`Interface`) → `Ok(ToolOk::Text(advisory))` — a SUCCESS,
///    not an error. Mapping this to `Err` would compile and pass most
///    tests while silently flipping `is_error` on the wire.
/// 3. callable kind with zero resolved hops → `Ok(ToolOk::Value(_))`
///    carrying an empty `Page<CallChain>`.
#[allow(clippy::too_many_arguments)]
pub fn callers_or_callees(
    graph: &RwLock<Graph>,
    indexed: bool,
    symbol: &str,
    depth: Option<u32>,
    direction: Direction,
    limit: Option<u32>,
    offset: Option<u32>,
    max_bytes: usize,
    min_confidence: Option<&str>,
) -> ToolResult<CallChainResponse> {
    require_indexed(indexed)?;

    if symbol.is_empty() {
        return Err(ToolError("'symbol' is required".to_string()));
    }

    let depth = depth.filter(|&d| d > 0).unwrap_or(1);

    let min_confidence_filter = match parse_min_confidence(min_confidence) {
        Ok(v) => v,
        Err(e) => return Err(ToolError(e)),
    };

    let g = graph.read();
    let mut chains: Vec<CallChain> = match direction {
        Direction::Callers => g.callers(symbol, depth, min_confidence_filter),
        Direction::Callees => g.callees(symbol, depth, min_confidence_filter),
    };

    // Accuracy-warning probe: when the target symbol carries `virtual`
    // in its signature, we know the resolver doesn't track
    // dynamic-dispatch call sites. The result page is correct for
    // static dispatch; surface the limitation as a warning so the
    // agent doesn't treat "0 callers" as authoritative on a virtual.
    //
    // Detection is a substring check on the signature text the parser
    // captured from source. The C++ parser preserves `virtual void
    // Foo()` verbatim in `Symbol.signature` — a substring search for
    // `"virtual "` (with the trailing space to avoid matching names
    // like `virtualize`) catches every virtual-method declaration.
    // The check fires for both `Direction::Callers` and
    // `Direction::Callees` because dynamic dispatch is the resolver
    // gap in BOTH directions.
    //
    // The lookup is `symbol_detail` (cheap HashMap hit). On a symbol
    // we can't find we silently skip — the no-result path below
    // surfaces the proper "symbol not found" error with its own
    // diagnostics.
    let mut response_warnings: Vec<String> = Vec::new();
    if let Some(s) = g.symbol_detail(symbol) {
        if is_virtual_signature(&s.signature) {
            response_warnings.push(
                "target is a virtual method; the resolver currently tracks \
                 STATIC-dispatch call sites only. Callers that dispatch \
                 through a base-class pointer or reference (e.g. \
                 `base_ptr->Foo()`) will not appear here even when they \
                 invoke this override at runtime. A `find_overrides` / \
                 `EdgeKind::Overrides` tool to bridge dynamic dispatch \
                 is on the roadmap."
                    .to_string(),
            );
        }
    }

    if chains.is_empty() {
        // Symbol may not exist at all — surface a did-you-mean error.
        // If it exists but is a non-callable kind (struct/enum/trait/
        // typedef/interface — structurally shaped, no call edges by
        // design), surface a soft-hint success that names the kind and
        // points the agent at the right alternative tool
        // (`get_class_hierarchy` / `get_symbol_detail`). This branch is
        // gated strictly on a kind set disjoint from the callable kinds
        // (`Function` / `Method`), so a callable symbol whose only
        // resolved callers/callees were filtered out by the resolved-only
        // BFS (e.g. a function whose only outgoing call edges target
        // unresolved tokens) still falls through to the empty envelope
        // below — that "wrong tool" vs "wrong symbol" vs "no callers in
        // scope" trichotomy is the agent-facing contract this function
        // implements.
        //
        // The advisory is `Ok(ToolOk::Text(_))` (not `Err`), matching the
        // workspace's core invariant that user-visible errors travel as
        // `CallToolResult { is_error: true }` — guidance is success, not
        // failure.
        match g.symbol_detail(symbol) {
            None => {
                let suggestions = suggest_symbols(&g, symbol, 5);
                drop(g);
                return if suggestions.is_empty() {
                    Err(ToolError(format!("symbol not found: {symbol:?}")))
                } else {
                    Err(ToolError(format!(
                        "symbol not found: {symbol:?}. Did you mean: {suggestions}?"
                    )))
                };
            }
            Some(s) if is_non_callable_kind(s.kind) => {
                let kind_name = kind_str(s.kind);
                let basename = symbol_id_basename(symbol);
                let kind_article = article(kind_name);
                let alt = alternative_tool_hint(s.kind);
                drop(g);
                let advisory = format!(
                    "{basename} is {kind_article} {kind_name}; {kind_name}s don't have call edges. {alt}"
                );
                return Ok(ToolOk::Text(advisory));
            }
            Some(_) => {
                // Callable kind (Function/Method/Class) with no resolved
                // callers/callees — fall through to the empty envelope.
            }
        }
    }
    drop(g);

    // Resolve defaults: zero-or-missing limit -> 100; clamp at 1000.
    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(100).min(1000);
    let resolved_offset = offset.unwrap_or(0);

    let total = chains.len() as u32;

    // Sort by (depth, symbol_id) ascending — depth first so page 1 holds
    // the closest hops, then symbol_id as a stable tiebreaker. The BFS in
    // `Graph::bfs` walks adjacency entries in HashMap iteration order
    // which is non-deterministic across runs; this canonicalizes the
    // sequence so offset/limit pagination partitions deterministically.
    chains.sort_by(|a, b| {
        a.depth
            .cmp(&b.depth)
            .then_with(|| a.symbol_id.cmp(&b.symbol_id))
    });

    // Route through byte_budget_take so the page honors the byte budget.
    // The helper internally applies offset+limit skip/take and stops early if
    // the running serialized byte count would exceed `max_bytes -
    // ENVELOPE_OVERHEAD_BYTES`. The helper preserves iteration order, so the
    // (depth, symbol_id) sort above is preserved across truncation: kept
    // records are a strict prefix of the sorted chain set. `total` (captured
    // above) remains the pre-pagination match count regardless of truncation.
    let (results, _total_kept, truncated, next_offset) =
        byte_budget_take(chains, resolved_offset, resolved_limit, max_bytes);

    let page = Page::<CallChain> {
        results,
        total,
        offset: resolved_offset,
        limit: resolved_limit,
        truncated,
        next_offset,
    };
    let response = CallChainResponse {
        page,
        warnings: response_warnings,
    };
    Ok(ToolOk::Value(response))
}

/// `find_overrides` body. Body moved verbatim from
/// `handlers::query::find_overrides`, plus the core `require_indexed`
/// call at entry (Decision 8).
pub fn find_overrides(
    graph: &RwLock<Graph>,
    indexed: bool,
    symbol: &str,
    limit: Option<u32>,
    offset: Option<u32>,
    max_bytes: usize,
) -> ToolResult<Page<CallChain>> {
    require_indexed(indexed)?;

    if symbol.is_empty() {
        return Err(ToolError("'symbol' is required".to_string()));
    }

    let g = graph.read();
    let mut overrides: Vec<CallChain> = g.find_overrides(symbol);

    if overrides.is_empty() && g.symbol_detail(symbol).is_none() {
        let suggestions = suggest_symbols(&g, symbol, 5);
        drop(g);
        return if suggestions.is_empty() {
            Err(ToolError(format!("symbol not found: {symbol:?}")))
        } else {
            Err(ToolError(format!(
                "symbol not found: {symbol:?}. Did you mean: {suggestions}?"
            )))
        };
    }
    drop(g);

    // Sort by symbol_id ascending for deterministic pagination.
    // Depth is always 1 for overrides; secondary sort by symbol_id
    // suffices.
    overrides.sort_by(|a, b| a.symbol_id.cmp(&b.symbol_id));

    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(100).min(1000);
    let resolved_offset = offset.unwrap_or(0);
    let total = overrides.len() as u32;

    let (results, _kept, truncated, next_offset) =
        byte_budget_take(overrides, resolved_offset, resolved_limit, max_bytes);

    let page = Page::<CallChain> {
        results,
        total,
        offset: resolved_offset,
        limit: resolved_limit,
        truncated,
        next_offset,
    };
    Ok(ToolOk::Value(page))
}

/// `get_dependencies` body. Body moved verbatim from
/// `handlers::query::get_dependencies`, plus the core `require_indexed`
/// call at entry (Decision 8).
pub fn get_dependencies(
    graph: &RwLock<Graph>,
    indexed: bool,
    file: &str,
    limit: Option<u32>,
    offset: Option<u32>,
    max_bytes: usize,
) -> ToolResult<Page<DependencyEntry>> {
    require_indexed(indexed)?;

    if file.is_empty() {
        return Err(ToolError("'file' is required".to_string()));
    }

    // Normalize the user-supplied `file` argument before graph lookup.
    // Mirrors `get_file_symbols`: canonical form when the path exists on
    // disk (resolving `.` / `..` and stripping the Windows `\\?\`
    // extended-path prefix), lexical fallback otherwise. On Linux with an
    // already-canonical path this is effectively identity, so existing
    // tests stay byte-identical.
    let path = paths::normalize_user_path(file);
    let deps = graph.read().file_dependencies(&path);

    // Resolve defaults: zero-or-missing limit -> 100; clamp at 1000.
    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(100).min(1000);
    let resolved_offset = offset.unwrap_or(0);

    // `file_dependencies` returns include entries each carrying the source
    // line of the `#include`. Map every entry to a `DependencyEntry`; the
    // kind is always `Includes` here (the include graph holds only
    // include edges), routed through `edge_kind_str` so the wire string
    // stays identical to `EdgeKind`'s serde output.
    let mut rows: Vec<DependencyEntry> = deps
        .into_iter()
        .map(|inc| DependencyEntry {
            file: inc.path.to_string_lossy().into_owned(),
            kind: edge_kind_str(EdgeKind::Includes),
            line: inc.line,
        })
        .collect();

    // Sort by (file, line) ascending so offset/limit pagination
    // partitions deterministically across calls. `file_dependencies`
    // clones the stored Vec in insertion order, which is not a stable
    // contract; this canonicalizes the sequence.
    rows.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.line.cmp(&b.line)));

    let total = rows.len() as u32;

    // Route through byte_budget_take: the helper applies offset+limit
    // skip/take and stops early if the running serialized byte count
    // would exceed `max_bytes - ENVELOPE_OVERHEAD_BYTES`. It preserves
    // iteration order, so the (file, line) sort above survives
    // truncation. `total` (captured above) stays the pre-pagination match
    // count regardless of truncation.
    let (results, _total_kept, truncated, next_offset) =
        byte_budget_take(rows, resolved_offset, resolved_limit, max_bytes);

    let response = Page::<DependencyEntry> {
        results,
        total,
        offset: resolved_offset,
        limit: resolved_limit,
        truncated,
        next_offset,
    };
    Ok(ToolOk::Value(response))
}

/// `find_path` body (phase 1). Body moved verbatim from
/// `handlers::query::find_path`, plus the core `require_indexed` call at
/// entry (Decision 8).
pub fn find_path(
    graph: &RwLock<Graph>,
    indexed: bool,
    from: &str,
    to: &str,
    node_cap: Option<u32>,
    min_confidence: Option<&str>,
) -> ToolResult<FindPathResponse> {
    require_indexed(indexed)?;

    if from.is_empty() {
        return Err(ToolError("'from' is required".to_string()));
    }
    if to.is_empty() {
        return Err(ToolError("'to' is required".to_string()));
    }

    let min_confidence_filter = match parse_min_confidence(min_confidence) {
        Ok(v) => v,
        Err(e) => return Err(ToolError(e)),
    };

    // Resolve node_cap: zero-or-missing -> default 100_000; clamp at the
    // 5_000_000 ceiling.
    let resolved_cap = node_cap
        .filter(|&n| n != 0)
        .unwrap_or(100_000)
        .min(5_000_000);

    let g = graph.read();

    if g.symbol_detail(from).is_none() {
        let suggestions = suggest_symbols(&g, from, 5);
        drop(g);
        return if suggestions.is_empty() {
            Err(ToolError(format!("'from' symbol not found: {from:?}")))
        } else {
            Err(ToolError(format!(
                "'from' symbol not found: {from:?}. Did you mean: {suggestions}?"
            )))
        };
    }
    if g.symbol_detail(to).is_none() {
        let suggestions = suggest_symbols(&g, to, 5);
        drop(g);
        return if suggestions.is_empty() {
            Err(ToolError(format!("'to' symbol not found: {to:?}")))
        } else {
            Err(ToolError(format!(
                "'to' symbol not found: {to:?}. Did you mean: {suggestions}?"
            )))
        };
    }

    let (result, nodes_examined, cap_reached) =
        g.shortest_path(from, to, resolved_cap, min_confidence_filter);
    drop(g);

    let response = match result {
        Some(r) => FindPathResponse {
            found: true,
            hop_count: r.hops.len().saturating_sub(1) as u32,
            hops: r.hops,
            heuristic_hops: r.heuristic_hops,
            nodes_examined,
            node_cap: resolved_cap,
            cap_reached,
        },
        None => FindPathResponse {
            found: false,
            hops: Vec::new(),
            hop_count: 0,
            heuristic_hops: 0,
            nodes_examined,
            node_cap: resolved_cap,
            cap_reached,
        },
    };
    Ok(ToolOk::Value(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph_core::{Confidence, Edge, FileGraph, Language, Symbol, SymbolKind};

    fn sym(name: &str, file: &str) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind: SymbolKind::Function,
            file: file.to_string(),
            line: 1,
            column: 0,
            end_line: 1,
            signature: format!("void {name}()"),
            namespace: String::new(),
            parent: String::new(),
            language: Language::Cpp,
        }
    }

    fn call_edge(from: &str, to: &str, file: &str, line: u32) -> Edge {
        Edge {
            from: from.to_string(),
            to: to.to_string(),
            kind: EdgeKind::Calls,
            file: file.to_string(),
            line,
            confidence: Confidence::Resolved,
            candidates: 1,
        }
    }

    fn graph_with_calls() -> Graph {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("a", "/x.cpp"), sym("b", "/x.cpp"), sym("c", "/x.cpp")],
            edges: vec![
                call_edge("/x.cpp:a", "/x.cpp:b", "/x.cpp", 1),
                call_edge("/x.cpp:b", "/x.cpp:c", "/x.cpp", 2),
            ],
        });
        g
    }

    fn graph_with_kind_only(name: &str, file: &str, kind: SymbolKind) -> Graph {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: file.to_string(),
            language: Language::Rust,
            symbols: vec![Symbol {
                name: name.to_string(),
                kind,
                file: file.to_string(),
                line: 1,
                column: 0,
                end_line: 1,
                signature: String::new(),
                namespace: String::new(),
                parent: String::new(),
                language: Language::Rust,
            }],
            edges: vec![],
        });
        g
    }

    fn locked(g: Graph) -> RwLock<Graph> {
        RwLock::new(g)
    }

    /// AC-28: an unindexed `callers_or_callees` returns `Err(ToolError)`,
    /// discriminable without ever going through serialization.
    #[test]
    fn callers_or_callees_unindexed_returns_typed_error() {
        let g = locked(Graph::new());
        let err = match callers_or_callees(
            &g,
            false,
            "/x.cpp:a",
            None,
            Direction::Callers,
            None,
            None,
            usize::MAX,
            None,
        ) {
            Err(e) => e,
            Ok(_) => panic!("unindexed callers_or_callees must error"),
        };
        assert_eq!(err.0, "no codebase indexed — call analyze_codebase first");
    }

    /// AC-28 counterpart: symbol-not-found is ALSO `Err(ToolError)` (once
    /// indexed), and the message still carries the did-you-mean text —
    /// both error paths must be discriminable from the two Ok variants
    /// without serialization.
    #[test]
    fn callers_or_callees_unknown_symbol_returns_typed_error_with_suggestion() {
        let g = locked(graph_with_calls());
        let err = match callers_or_callees(
            &g,
            true,
            "a",
            None,
            Direction::Callers,
            None,
            None,
            usize::MAX,
            None,
        ) {
            Err(e) => e,
            Ok(_) => panic!("unknown symbol must error"),
        };
        assert!(
            err.0.starts_with("symbol not found: \"a\""),
            "got: {}",
            err.0
        );
        assert!(err.0.contains("Did you mean: "), "got: {}", err.0);
    }

    /// AC-03 — the load-bearing trap this task exists to catch: a
    /// non-callable kind (`Struct` here) must yield `Ok(ToolOk::Text(_))`,
    /// distinguishable from BOTH `Ok(ToolOk::Value(_))` and `Err(_)`.
    #[test]
    fn callers_or_callees_non_callable_kind_returns_text_success() {
        let g = locked(graph_with_kind_only("Foo", "/lib.rs", SymbolKind::Struct));
        let result = callers_or_callees(
            &g,
            true,
            "/lib.rs:Foo",
            Some(1),
            Direction::Callers,
            None,
            None,
            usize::MAX,
            None,
        );
        match result {
            Ok(ToolOk::Text(s)) => {
                assert!(s.contains("Foo is a struct"), "got: {s}");
                assert!(
                    s.contains("get_class_hierarchy") || s.contains("get_symbol_detail"),
                    "got: {s}"
                );
            }
            Ok(ToolOk::Value(_)) => panic!("non-callable kind must not yield Value"),
            Err(e) => panic!("non-callable kind must not yield Err: {}", e.0),
        }
    }

    /// The third leg of the trichotomy: a callable kind (`Function`) with
    /// zero resolved hops returns `Ok(ToolOk::Value(_))` carrying an empty
    /// `Page<CallChain>` — proving the middle case (Text) does not bleed
    /// into this one.
    #[test]
    fn callers_or_callees_callable_zero_hops_returns_empty_value_page() {
        let g = locked(graph_with_calls());
        let result = callers_or_callees(
            &g,
            true,
            "/x.cpp:a",
            Some(1),
            Direction::Callers,
            None,
            None,
            usize::MAX,
            None,
        );
        match result {
            Ok(ToolOk::Value(response)) => {
                assert!(response.page.results.is_empty());
                assert_eq!(response.page.total, 0);
            }
            Ok(ToolOk::Text(s)) => panic!("callable kind must not yield the soft-hint: {s}"),
            Err(e) => panic!("callable symbol with zero hops must not error: {}", e.0),
        }
    }

    /// AC-28 for `find_overrides`.
    #[test]
    fn find_overrides_unindexed_returns_typed_error() {
        let g = locked(Graph::new());
        let err = match find_overrides(&g, false, "/x.cpp:a", None, None, usize::MAX) {
            Err(e) => e,
            Ok(_) => panic!("unindexed find_overrides must error"),
        };
        assert_eq!(err.0, "no codebase indexed — call analyze_codebase first");
    }

    /// AC-28 for `get_dependencies`.
    #[test]
    fn get_dependencies_unindexed_returns_typed_error() {
        let g = locked(Graph::new());
        let err = match get_dependencies(&g, false, "/a.cpp", None, None, usize::MAX) {
            Err(e) => e,
            Ok(_) => panic!("unindexed get_dependencies must error"),
        };
        assert_eq!(err.0, "no codebase indexed — call analyze_codebase first");
    }

    /// AC-28 for `find_path`.
    #[test]
    fn find_path_unindexed_returns_typed_error() {
        let g = locked(Graph::new());
        let err = match find_path(&g, false, "/x.cpp:a", "/x.cpp:c", None, None) {
            Err(e) => e,
            Ok(_) => panic!("unindexed find_path must error"),
        };
        assert_eq!(err.0, "no codebase indexed — call analyze_codebase first");
    }

    /// Sanity: once indexed, `find_path` still succeeds and returns a
    /// typed `Value`.
    #[test]
    fn find_path_indexed_returns_typed_value() {
        let g = locked(graph_with_calls());
        let result = find_path(&g, true, "/x.cpp:a", "/x.cpp:c", None, None);
        match result {
            Ok(ToolOk::Value(r)) => assert!(r.found),
            Ok(ToolOk::Text(_)) => panic!("expected Value(FindPathResponse)"),
            Err(e) => panic!("indexed find_path must succeed: {}", e.0),
        }
    }
}
