//! Call-graph and dependency handlers: `get_callers`, `get_callees`,
//! `get_dependencies`.
//!
//! Mirrors `internal/tools/query.go` from the Go binary. Did-you-mean
//! suggestions on `get_callers` / `get_callees` only fire when the BFS
//! result is empty AND the symbol is unknown — the Go behavior is to
//! return `[]` for a known symbol that just has no callers/callees, and
//! to return a tool error only when the symbol itself isn't in the graph.

use code_graph_core::{paths, EdgeKind};
use code_graph_graph::{CallChain, Graph};
use parking_lot::RwLock;
use rmcp::model::CallToolResult;

use super::{
    byte_budget_take, edge_kind_str, suggest_symbols, tool_error, tool_success_json,
    DependencyEntry, Page,
};

/// Direction for [`callers_or_callees`]. Reverse → [`Graph::callers`];
/// Forward → [`Graph::callees`].
pub enum Direction {
    /// Walk reverse adjacency (`Graph::callers`).
    Callers,
    /// Walk forward adjacency (`Graph::callees`).
    Callees,
}

/// Shared body for `get_callers` and `get_callees` — same shape, only the
/// adjacency direction differs.
///
/// Output is the shared [`Page`]`<`[`CallChain`]`>` envelope. The BFS
/// returns rows in HashMap-iteration order (non-deterministic across
/// runs); the handler sorts by `(depth, symbol_id)` ascending so page 1
/// holds the closest callers/callees and same-depth rows tie-break by
/// `symbol_id` for stable pagination across calls. `total` reports the
/// pre-pagination match count.
///
/// Defaults: `limit = 100`, `offset = 0`. `limit = 0` means "use the
/// default" (mirrors `search_symbols` and `get_orphans`); `limit` is
/// silently clamped at 1000. The existing `depth` parameter still
/// constrains the BFS scope (default 1) and is unchanged.
///
/// The symbol-not-found error path is unchanged: an unknown symbol
/// surfaces a did-you-mean error before pagination is consulted. A known
/// symbol with no callers/callees returns the envelope with `results: []`
/// and `total: 0` — that distinction lets agents tell "wrong symbol"
/// apart from "no callers in scope".
pub fn callers_or_callees(
    graph: &RwLock<Graph>,
    symbol: &str,
    depth: Option<u32>,
    direction: Direction,
    limit: Option<u32>,
    offset: Option<u32>,
    max_bytes: usize,
) -> CallToolResult {
    if symbol.is_empty() {
        return tool_error("'symbol' is required");
    }

    let depth = depth.filter(|&d| d > 0).unwrap_or(1);

    let g = graph.read();
    let mut chains: Vec<CallChain> = match direction {
        Direction::Callers => g.callers(symbol, depth),
        Direction::Callees => g.callees(symbol, depth),
    };

    if chains.is_empty() {
        // Symbol may not exist at all — surface a did-you-mean error.
        // If it exists but has no callers/callees, return an empty
        // envelope (results=[], total=0) below. This preserves the
        // "wrong symbol" vs "no callers" distinction the Go binary had
        // (error vs. empty array) — pagination is additive on top.
        if g.symbol_detail(symbol).is_none() {
            let suggestions = suggest_symbols(&g, symbol, 5);
            drop(g);
            return if suggestions.is_empty() {
                tool_error(format!("symbol not found: {symbol:?}"))
            } else {
                tool_error(format!(
                    "symbol not found: {symbol:?}. Did you mean: {suggestions}?"
                ))
            };
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

    let response = Page::<CallChain> {
        results,
        total,
        offset: resolved_offset,
        limit: resolved_limit,
        truncated,
        next_offset,
    };
    tool_success_json(&response)
}

/// `get_dependencies` body. Returns the shared [`Page`]`<`[`DependencyEntry`]`>`
/// envelope: one row per included file carrying the included path, the
/// edge kind (`"includes"`), and the source line of the `#include`
/// directive. An unknown file is not an error — it yields an empty page
/// (`results: []`, `total: 0`), preserving the prior "never `null`"
/// contract in the reshaped envelope form.
///
/// Rows are sorted by `(file, line)` ascending so pagination partitions
/// the result deterministically across calls. `total` is the
/// pre-pagination match count; the page itself is byte-budgeted via
/// [`byte_budget_take`] so a file with thousands of includes cannot blow
/// the response-size cap.
///
/// Defaults: `limit = 100`, `offset = 0`. `limit = 0` means "use the
/// default" (mirrors the other paginated handlers); `limit` is silently
/// clamped at 1000.
pub fn get_dependencies(
    graph: &RwLock<Graph>,
    file: &str,
    limit: Option<u32>,
    offset: Option<u32>,
    max_bytes: usize,
) -> CallToolResult {
    if file.is_empty() {
        return tool_error("'file' is required");
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
    tool_success_json(&response)
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{body_text, page_parts};
    use super::super::NO_BYTE_BUDGET;
    use super::*;
    use code_graph_core::{Edge, EdgeKind, FileGraph, Language, Symbol, SymbolKind};

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
        }
    }

    fn include_edge(from: &str, to: &str) -> Edge {
        Edge {
            from: from.to_string(),
            to: to.to_string(),
            kind: EdgeKind::Includes,
            file: from.to_string(),
            line: 1,
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

    fn locked(g: Graph) -> RwLock<Graph> {
        RwLock::new(g)
    }

    // --- callers / callees ---

    #[test]
    fn callers_missing_symbol_param_errors() {
        let g = locked(Graph::new());
        let r = callers_or_callees(&g, "", None, Direction::Callers, None, None, NO_BYTE_BUDGET);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'symbol' is required");
    }

    #[test]
    fn callees_missing_symbol_param_errors() {
        let g = locked(Graph::new());
        let r = callers_or_callees(&g, "", None, Direction::Callees, None, None, NO_BYTE_BUDGET);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'symbol' is required");
    }

    #[test]
    fn callers_returns_chain_for_known_symbol() {
        let g = locked(graph_with_calls());
        let r = callers_or_callees(
            &g,
            "/x.cpp:c",
            Some(1),
            Direction::Callers,
            None,
            None,
            NO_BYTE_BUDGET,
        );
        let (arr, _, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["symbol_id"], serde_json::json!("/x.cpp:b"));
    }

    #[test]
    fn callers_depth_default_one() {
        let g = locked(graph_with_calls());
        let r = callers_or_callees(
            &g,
            "/x.cpp:c",
            None,
            Direction::Callers,
            None,
            None,
            NO_BYTE_BUDGET,
        );
        let (arr, _, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 1);
    }

    #[test]
    fn callees_returns_chain_for_known_symbol() {
        let g = locked(graph_with_calls());
        let r = callers_or_callees(
            &g,
            "/x.cpp:a",
            Some(2),
            Direction::Callees,
            None,
            None,
            NO_BYTE_BUDGET,
        );
        let (arr, _, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 2);
        let names: Vec<String> = arr
            .iter()
            .map(|h| h["symbol_id"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"/x.cpp:b".to_string()));
        assert!(names.contains(&"/x.cpp:c".to_string()));
    }

    #[test]
    fn callers_known_symbol_with_no_callers_returns_empty_envelope() {
        // `/x.cpp:a` has no callers. Symbol exists in graph → return an
        // envelope with results=[] and total=0 (NOT a tool error).
        let g = locked(graph_with_calls());
        let r = callers_or_callees(
            &g,
            "/x.cpp:a",
            Some(1),
            Direction::Callers,
            None,
            None,
            NO_BYTE_BUDGET,
        );
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let (arr, total, offset, limit) = page_parts(&r);
        assert!(arr.is_empty());
        assert_eq!(total, 0);
        assert_eq!(offset, 0);
        assert_eq!(limit, 100);
    }

    #[test]
    fn callees_known_symbol_with_no_callees_returns_empty_envelope() {
        // `/x.cpp:c` has no callees. Symbol exists → return empty envelope.
        let g = locked(graph_with_calls());
        let r = callers_or_callees(
            &g,
            "/x.cpp:c",
            Some(1),
            Direction::Callees,
            None,
            None,
            NO_BYTE_BUDGET,
        );
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let (arr, total, _, _) = page_parts(&r);
        assert!(arr.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn callers_unknown_symbol_with_suggestions() {
        let g = locked(graph_with_calls());
        // "a" matches the substring of `/x.cpp:a`. The graph has `a`/`b`/`c`
        // — `a` should be suggested via search_symbols substring matching.
        let r = callers_or_callees(
            &g,
            "a",
            None,
            Direction::Callers,
            None,
            None,
            NO_BYTE_BUDGET,
        );
        assert_eq!(r.is_error, Some(true));
        let text = body_text(&r);
        assert!(text.starts_with("symbol not found: \"a\""), "got: {text}");
        assert!(text.contains("Did you mean: "), "got: {text}");
    }

    #[test]
    fn callers_unknown_symbol_no_suggestions() {
        let g = locked(Graph::new());
        let r = callers_or_callees(
            &g,
            "nope",
            None,
            Direction::Callers,
            None,
            None,
            NO_BYTE_BUDGET,
        );
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "symbol not found: \"nope\"");
    }

    // --- pagination invariants --------------------------------------------

    /// Build a graph with a single hub symbol called by `n` distinct callers
    /// named `caller_000`, `caller_001`, ... — zero-padded to 3 digits so the
    /// natural sort by `symbol_id` ascending is predictable. All callers
    /// live in `/big.cpp`; the hub `target` lives in `/hub.cpp`.
    fn graph_with_n_callers(n: usize) -> Graph {
        let mut g = Graph::new();
        let mut symbols: Vec<Symbol> = Vec::with_capacity(n);
        let mut edges: Vec<Edge> = Vec::with_capacity(n);
        for i in 0..n {
            let name = format!("caller_{i:03}");
            symbols.push(sym(&name, "/big.cpp"));
            edges.push(call_edge(
                &format!("/big.cpp:{name}"),
                "/hub.cpp:target",
                "/big.cpp",
                (i + 1) as u32,
            ));
        }
        g.merge_file_graph(FileGraph {
            path: "/big.cpp".to_string(),
            language: Language::Cpp,
            symbols,
            edges,
        });
        g.merge_file_graph(FileGraph {
            path: "/hub.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("target", "/hub.cpp")],
            edges: vec![],
        });
        g
    }

    /// Mirror of `graph_with_n_callers` for the callees direction: a single
    /// hub symbol `entry` calls `n` distinct callees named `callee_NNN`.
    fn graph_with_n_callees(n: usize) -> Graph {
        let mut g = Graph::new();
        let mut callee_symbols: Vec<Symbol> = Vec::with_capacity(n);
        let mut edges: Vec<Edge> = Vec::with_capacity(n);
        for i in 0..n {
            let name = format!("callee_{i:03}");
            callee_symbols.push(sym(&name, "/big.cpp"));
            edges.push(call_edge(
                "/hub.cpp:entry",
                &format!("/big.cpp:{name}"),
                "/hub.cpp",
                (i + 1) as u32,
            ));
        }
        g.merge_file_graph(FileGraph {
            path: "/big.cpp".to_string(),
            language: Language::Cpp,
            symbols: callee_symbols,
            edges: vec![],
        });
        g.merge_file_graph(FileGraph {
            path: "/hub.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("entry", "/hub.cpp")],
            edges,
        });
        g
    }

    // --- get_callers pagination invariants --------------------------------

    #[test]
    fn callers_default_limit_is_100() {
        // 120 callers: default limit (100) returns the first 100; total = 120.
        let g = locked(graph_with_n_callers(120));
        let r = callers_or_callees(
            &g,
            "/hub.cpp:target",
            Some(1),
            Direction::Callers,
            None,
            None,
            NO_BYTE_BUDGET,
        );
        let (arr, total, offset, limit) = page_parts(&r);
        assert_eq!(arr.len(), 100);
        assert_eq!(total, 120);
        assert_eq!(offset, 0);
        assert_eq!(limit, 100);
    }

    #[test]
    fn callers_page_1_and_page_2_cover_full_set() {
        // 150 callers: page 1 (offset=0, limit=100) ∪ page 2 (offset=100,
        // limit=100) covers all 150 with no overlap.
        let g = locked(graph_with_n_callers(150));
        let p1 = callers_or_callees(
            &g,
            "/hub.cpp:target",
            Some(1),
            Direction::Callers,
            Some(100),
            Some(0),
            NO_BYTE_BUDGET,
        );
        let p2 = callers_or_callees(
            &g,
            "/hub.cpp:target",
            Some(1),
            Direction::Callers,
            Some(100),
            Some(100),
            NO_BYTE_BUDGET,
        );
        let (a1, t1, _, _) = page_parts(&p1);
        let (a2, t2, _, _) = page_parts(&p2);
        assert_eq!(a1.len(), 100);
        assert_eq!(a2.len(), 50);
        assert_eq!(t1, 150);
        assert_eq!(t2, 150);

        let mut ids: Vec<String> = a1
            .iter()
            .chain(a2.iter())
            .map(|e| e["symbol_id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids.dedup();
        assert_eq!(
            ids.len(),
            150,
            "page1 ∪ page2 must cover all 150 with no dup"
        );
    }

    #[test]
    fn callers_total_is_pre_pagination_count() {
        let g = locked(graph_with_n_callers(150));
        let r1 = callers_or_callees(
            &g,
            "/hub.cpp:target",
            Some(1),
            Direction::Callers,
            Some(50),
            Some(0),
            NO_BYTE_BUDGET,
        );
        let r2 = callers_or_callees(
            &g,
            "/hub.cpp:target",
            Some(1),
            Direction::Callers,
            Some(50),
            Some(50),
            NO_BYTE_BUDGET,
        );
        let r3 = callers_or_callees(
            &g,
            "/hub.cpp:target",
            Some(1),
            Direction::Callers,
            Some(10),
            Some(140),
            NO_BYTE_BUDGET,
        );
        let (_, t1, _, _) = page_parts(&r1);
        let (_, t2, _, _) = page_parts(&r2);
        let (_, t3, _, _) = page_parts(&r3);
        assert_eq!(t1, 150);
        assert_eq!(t2, 150);
        assert_eq!(t3, 150);
    }

    #[test]
    fn callers_limit_clamps_at_1000() {
        let g = locked(graph_with_n_callers(5));
        let r = callers_or_callees(
            &g,
            "/hub.cpp:target",
            Some(1),
            Direction::Callers,
            Some(999_999),
            None,
            NO_BYTE_BUDGET,
        );
        let (arr, _, _, limit) = page_parts(&r);
        assert_eq!(limit, 1000);
        assert_eq!(arr.len(), 5);
    }

    #[test]
    fn callers_zero_limit_uses_default() {
        let g = locked(graph_with_n_callers(5));
        let r = callers_or_callees(
            &g,
            "/hub.cpp:target",
            Some(1),
            Direction::Callers,
            Some(0),
            None,
            NO_BYTE_BUDGET,
        );
        let (_, _, _, limit) = page_parts(&r);
        assert_eq!(limit, 100);
    }

    #[test]
    fn callers_offset_beyond_total_returns_empty() {
        let g = locked(graph_with_n_callers(5));
        let r = callers_or_callees(
            &g,
            "/hub.cpp:target",
            Some(1),
            Direction::Callers,
            None,
            Some(999),
            NO_BYTE_BUDGET,
        );
        let (arr, total, offset, limit) = page_parts(&r);
        assert!(arr.is_empty());
        assert_eq!(total, 5);
        assert_eq!(offset, 999);
        assert_eq!(limit, 100);
    }

    #[test]
    fn callers_pagination_orders_by_depth_then_symbol_id() {
        // Two-depth fixture with same-depth ties. The chain is:
        //   d_far -> d_near_b -> target     (target is the query)
        //   c_near_a -> target              (depth 1 alongside d_near_b)
        //
        // Asking for callers of target with depth=2 should produce three
        // CallChains:
        //   depth=1 c_near_a (lex less than d_near_b)
        //   depth=1 d_near_b
        //   depth=2 d_far
        //
        // i.e. tuple sort (depth asc, symbol_id asc) — NOT BFS order.
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("target", "/x.cpp"),
                sym("d_near_b", "/x.cpp"),
                sym("c_near_a", "/x.cpp"),
                sym("d_far", "/x.cpp"),
            ],
            edges: vec![
                call_edge("/x.cpp:d_near_b", "/x.cpp:target", "/x.cpp", 1),
                call_edge("/x.cpp:c_near_a", "/x.cpp:target", "/x.cpp", 2),
                call_edge("/x.cpp:d_far", "/x.cpp:d_near_b", "/x.cpp", 3),
            ],
        });
        let g = locked(g);
        let r = callers_or_callees(
            &g,
            "/x.cpp:target",
            Some(2),
            Direction::Callers,
            None,
            None,
            NO_BYTE_BUDGET,
        );
        let (arr, _, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 3);
        // Expected order: (1, c_near_a), (1, d_near_b), (2, d_far).
        assert_eq!(arr[0]["depth"], serde_json::json!(1));
        assert_eq!(arr[0]["symbol_id"], serde_json::json!("/x.cpp:c_near_a"));
        assert_eq!(arr[1]["depth"], serde_json::json!(1));
        assert_eq!(arr[1]["symbol_id"], serde_json::json!("/x.cpp:d_near_b"));
        assert_eq!(arr[2]["depth"], serde_json::json!(2));
        assert_eq!(arr[2]["symbol_id"], serde_json::json!("/x.cpp:d_far"));
    }

    // --- byte-budget invariants (callers direction) ----------------------
    //
    // The companion callees-side test lives below (same handler, same
    // wiring — distinct fixture/assertions).

    /// Build a graph with `per_depth` distinct callers at each of three
    /// depths (1, 2, 3) feeding a single hub `target`. Names are
    /// zero-padded and per-depth-prefixed so the (depth, symbol_id) sort
    /// order is predictable: `d1_caller_NNN` < `d2_caller_NNN` < `d3_caller_NNN`
    /// within each depth bucket. At BFS depth=3, the handler returns
    /// `3 * per_depth` chains.
    ///
    /// Layout (per_depth=3 example):
    ///   d3_caller_002 -> d2_caller_002 -> d1_caller_002 -> target
    ///   d3_caller_001 -> d2_caller_001 -> d1_caller_001 -> target
    ///   d3_caller_000 -> d2_caller_000 -> d1_caller_000 -> target
    fn graph_with_layered_callers(per_depth: usize) -> Graph {
        let mut g = Graph::new();
        let mut symbols: Vec<Symbol> = Vec::with_capacity(per_depth * 3);
        let mut edges: Vec<Edge> = Vec::with_capacity(per_depth * 3);
        for i in 0..per_depth {
            let d1 = format!("d1_caller_{i:03}");
            let d2 = format!("d2_caller_{i:03}");
            let d3 = format!("d3_caller_{i:03}");
            symbols.push(sym(&d1, "/big.cpp"));
            symbols.push(sym(&d2, "/big.cpp"));
            symbols.push(sym(&d3, "/big.cpp"));
            // d1 -> target (depth=1)
            edges.push(call_edge(
                &format!("/big.cpp:{d1}"),
                "/hub.cpp:target",
                "/big.cpp",
                (i * 3 + 1) as u32,
            ));
            // d2 -> d1 (depth=2 in callers BFS from target)
            edges.push(call_edge(
                &format!("/big.cpp:{d2}"),
                &format!("/big.cpp:{d1}"),
                "/big.cpp",
                (i * 3 + 2) as u32,
            ));
            // d3 -> d2 (depth=3)
            edges.push(call_edge(
                &format!("/big.cpp:{d3}"),
                &format!("/big.cpp:{d2}"),
                "/big.cpp",
                (i * 3 + 3) as u32,
            ));
        }
        g.merge_file_graph(FileGraph {
            path: "/big.cpp".to_string(),
            language: Language::Cpp,
            symbols,
            edges,
        });
        g.merge_file_graph(FileGraph {
            path: "/hub.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("target", "/hub.cpp")],
            edges: vec![],
        });
        g
    }

    #[test]
    fn callers_byte_budget_preserves_depth_sort_order() {
        // Byte-budget truncation must not reorder the
        // (depth, symbol_id)-sorted chain
        // set. The helper preserves iteration order, so kept records are a
        // strict prefix of the sorted chain — i.e. `max(kept depth) <=
        // min(would-be-next-page depth)` and within-depth ties are
        // tiebroken by symbol_id ascending.
        //
        // Fixture: 30 callers spread across depths 1/2/3 (10 each). With a
        // tight `max_bytes` that fits only ~8 records, truncation must
        // happen within the depth-1 bucket (the sorted prefix). The first
        // dropped record is also at depth=1, so kept depths are
        // non-decreasing and bounded above by the dropped page's first
        // depth.
        use super::super::ENVELOPE_OVERHEAD_BYTES;
        let g = locked(graph_with_layered_callers(10));
        // Each CallChain serializes to roughly 90-110 bytes:
        //   {"symbol_id":"/big.cpp:d1_caller_NNN","file":"/big.cpp","line":N,"depth":1}
        // Budget = 800 bytes after envelope reservation → ~8 records before
        // the 9th's projected total exceeds the budget.
        let max_bytes = ENVELOPE_OVERHEAD_BYTES + 800;
        let r = callers_or_callees(
            &g,
            "/hub.cpp:target",
            Some(3), // walk all 3 BFS depths so 30 chains are produced
            Direction::Callers,
            Some(100),
            Some(0),
            max_bytes,
        );

        let (arr, total, offset, _limit) = page_parts(&r);
        let (truncated, next_offset) = super::super::test_helpers::page_extras(&r);

        assert!(truncated, "tight max_bytes must produce truncated=true");
        let n = next_offset.expect("truncated=true must set next_offset=Some(n)");
        assert!(
            n > offset,
            "next_offset must point past the current page: next_offset={n} <= offset={offset}",
        );
        assert_eq!(total, 30, "total is the pre-pagination match count");
        assert!(
            !arr.is_empty(),
            "budget should still admit at least one record",
        );
        assert!(
            (arr.len() as u32) < 100,
            "byte budget (not count cap) must trim the page: arr.len()={}",
            arr.len(),
        );

        // Sort-determinism core assertion: kept records' depths are
        // non-decreasing (the helper preserved the handler's (depth,
        // symbol_id) sort order).
        let depths: Vec<u64> = arr
            .iter()
            .map(|h| h["depth"].as_u64().expect("depth is u64"))
            .collect();
        for win in depths.windows(2) {
            assert!(
                win[0] <= win[1],
                "kept depths must be non-decreasing: {depths:?}",
            );
        }
        // And within-depth ties are tiebroken by symbol_id ascending. Run
        // the same monotonic check on the (depth, symbol_id) tuple to
        // confirm the handler's pre-truncation sort survived the helper.
        let keys: Vec<(u64, String)> = arr
            .iter()
            .map(|h| {
                (
                    h["depth"].as_u64().unwrap(),
                    h["symbol_id"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        for win in keys.windows(2) {
            assert!(
                win[0] <= win[1],
                "kept records must remain (depth, symbol_id)-sorted: {keys:?}",
            );
        }

        // max(kept depth) <= depth of the first dropped record. With the
        // fixture's 30 chains and an 8-ish-record budget, the prefix lives
        // entirely within depth=1, so the first dropped depth is also 1.
        let max_kept_depth = depths.iter().copied().max().unwrap();
        // Re-fetch the full sorted chain set deterministically and read the
        // dropped page's first depth from the position `next_offset`. The
        // handler's (depth, symbol_id) ordering is reproduced here by
        // calling with `offset = n, limit = 1` (NO_BYTE_BUDGET so nothing
        // truncates).
        let r_next = callers_or_callees(
            &g,
            "/hub.cpp:target",
            Some(3),
            Direction::Callers,
            Some(1),
            Some(n),
            NO_BYTE_BUDGET,
        );
        let (arr_next, _, _, _) = page_parts(&r_next);
        assert_eq!(arr_next.len(), 1, "fixture guarantees a next record exists");
        let first_dropped_depth = arr_next[0]["depth"].as_u64().unwrap();
        assert!(
            max_kept_depth <= first_dropped_depth,
            "max(kept depth)={max_kept_depth} must be <= first_dropped_depth={first_dropped_depth}",
        );
    }

    #[test]
    fn callers_byte_budget_no_truncation_with_no_budget() {
        // Anti-regression: with NO_BYTE_BUDGET (= usize::MAX), the
        // handler's existing behavior is preserved exactly — no truncation,
        // no next_offset. Locks the contract that the byte-budget wiring
        // does not affect callers that opt out.
        let g = locked(graph_with_n_callers(30));
        let r = callers_or_callees(
            &g,
            "/hub.cpp:target",
            Some(1),
            Direction::Callers,
            Some(100),
            Some(0),
            NO_BYTE_BUDGET,
        );
        let (arr, total, _, _) = page_parts(&r);
        let (truncated, next_offset) = super::super::test_helpers::page_extras(&r);
        assert_eq!(arr.len(), 30);
        assert_eq!(total, 30);
        assert!(!truncated);
        assert_eq!(next_offset, None);
    }

    // --- get_callees pagination invariants --------------------------------

    #[test]
    fn callees_default_limit_is_100() {
        let g = locked(graph_with_n_callees(120));
        let r = callers_or_callees(
            &g,
            "/hub.cpp:entry",
            Some(1),
            Direction::Callees,
            None,
            None,
            NO_BYTE_BUDGET,
        );
        let (arr, total, offset, limit) = page_parts(&r);
        assert_eq!(arr.len(), 100);
        assert_eq!(total, 120);
        assert_eq!(offset, 0);
        assert_eq!(limit, 100);
    }

    #[test]
    fn callees_page_1_and_page_2_cover_full_set() {
        let g = locked(graph_with_n_callees(150));
        let p1 = callers_or_callees(
            &g,
            "/hub.cpp:entry",
            Some(1),
            Direction::Callees,
            Some(100),
            Some(0),
            NO_BYTE_BUDGET,
        );
        let p2 = callers_or_callees(
            &g,
            "/hub.cpp:entry",
            Some(1),
            Direction::Callees,
            Some(100),
            Some(100),
            NO_BYTE_BUDGET,
        );
        let (a1, t1, _, _) = page_parts(&p1);
        let (a2, t2, _, _) = page_parts(&p2);
        assert_eq!(a1.len(), 100);
        assert_eq!(a2.len(), 50);
        assert_eq!(t1, 150);
        assert_eq!(t2, 150);

        let mut ids: Vec<String> = a1
            .iter()
            .chain(a2.iter())
            .map(|e| e["symbol_id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 150);
    }

    #[test]
    fn callees_total_is_pre_pagination_count() {
        let g = locked(graph_with_n_callees(150));
        let r1 = callers_or_callees(
            &g,
            "/hub.cpp:entry",
            Some(1),
            Direction::Callees,
            Some(50),
            Some(0),
            NO_BYTE_BUDGET,
        );
        let r2 = callers_or_callees(
            &g,
            "/hub.cpp:entry",
            Some(1),
            Direction::Callees,
            Some(50),
            Some(50),
            NO_BYTE_BUDGET,
        );
        let r3 = callers_or_callees(
            &g,
            "/hub.cpp:entry",
            Some(1),
            Direction::Callees,
            Some(10),
            Some(140),
            NO_BYTE_BUDGET,
        );
        let (_, t1, _, _) = page_parts(&r1);
        let (_, t2, _, _) = page_parts(&r2);
        let (_, t3, _, _) = page_parts(&r3);
        assert_eq!(t1, 150);
        assert_eq!(t2, 150);
        assert_eq!(t3, 150);
    }

    #[test]
    fn callees_limit_clamps_at_1000() {
        let g = locked(graph_with_n_callees(5));
        let r = callers_or_callees(
            &g,
            "/hub.cpp:entry",
            Some(1),
            Direction::Callees,
            Some(999_999),
            None,
            NO_BYTE_BUDGET,
        );
        let (arr, _, _, limit) = page_parts(&r);
        assert_eq!(limit, 1000);
        assert_eq!(arr.len(), 5);
    }

    #[test]
    fn callees_zero_limit_uses_default() {
        let g = locked(graph_with_n_callees(5));
        let r = callers_or_callees(
            &g,
            "/hub.cpp:entry",
            Some(1),
            Direction::Callees,
            Some(0),
            None,
            NO_BYTE_BUDGET,
        );
        let (_, _, _, limit) = page_parts(&r);
        assert_eq!(limit, 100);
    }

    #[test]
    fn callees_offset_beyond_total_returns_empty() {
        let g = locked(graph_with_n_callees(5));
        let r = callers_or_callees(
            &g,
            "/hub.cpp:entry",
            Some(1),
            Direction::Callees,
            None,
            Some(999),
            NO_BYTE_BUDGET,
        );
        let (arr, total, offset, limit) = page_parts(&r);
        assert!(arr.is_empty());
        assert_eq!(total, 5);
        assert_eq!(offset, 999);
        assert_eq!(limit, 100);
    }

    // --- byte-budget invariants (callees direction) ---------------------
    //
    // Mirrors the callers-side tests above. The wiring is identical — both
    // directions flow through the same `callers_or_callees` handler — so
    // these tests cover
    // the callee-side BFS edge construction and lock the documented
    // sort-determinism contract for `Direction::Callees`.

    /// Mirror of `graph_with_layered_callers` for the callees direction:
    /// a single hub `entry` calls `per_depth` distinct depth-1 callees,
    /// each of which calls a depth-2 callee, each of which calls a
    /// depth-3 callee. Names are zero-padded and per-depth-prefixed so
    /// the `(depth, symbol_id)` sort order is predictable:
    /// `d1_callee_NNN` < `d2_callee_NNN` < `d3_callee_NNN` within each
    /// depth bucket. At BFS depth=3, the handler returns
    /// `3 * per_depth` chains.
    ///
    /// Layout (per_depth=3 example):
    ///   entry -> d1_callee_000 -> d2_callee_000 -> d3_callee_000
    ///   entry -> d1_callee_001 -> d2_callee_001 -> d3_callee_001
    ///   entry -> d1_callee_002 -> d2_callee_002 -> d3_callee_002
    fn graph_with_layered_callees(per_depth: usize) -> Graph {
        let mut g = Graph::new();
        let mut callee_symbols: Vec<Symbol> = Vec::with_capacity(per_depth * 3);
        // entry's body holds the call sites for entry -> d1 (lives in /hub.cpp).
        let mut hub_edges: Vec<Edge> = Vec::with_capacity(per_depth);
        // d1's body holds d1 -> d2 call sites; d2's body holds d2 -> d3.
        // Both lots live in /big.cpp.
        let mut big_edges: Vec<Edge> = Vec::with_capacity(per_depth * 2);
        for i in 0..per_depth {
            let d1 = format!("d1_callee_{i:03}");
            let d2 = format!("d2_callee_{i:03}");
            let d3 = format!("d3_callee_{i:03}");
            callee_symbols.push(sym(&d1, "/big.cpp"));
            callee_symbols.push(sym(&d2, "/big.cpp"));
            callee_symbols.push(sym(&d3, "/big.cpp"));
            // entry -> d1 (depth=1 in callees BFS from entry).
            hub_edges.push(call_edge(
                "/hub.cpp:entry",
                &format!("/big.cpp:{d1}"),
                "/hub.cpp",
                (i + 1) as u32,
            ));
            // d1 -> d2 (depth=2). big.cpp interleaves d1->d2 (odd) and
            // d2->d3 (even), giving each edge a unique line within the file.
            big_edges.push(call_edge(
                &format!("/big.cpp:{d1}"),
                &format!("/big.cpp:{d2}"),
                "/big.cpp",
                (i * 2 + 1) as u32,
            ));
            // d2 -> d3 (depth=3).
            big_edges.push(call_edge(
                &format!("/big.cpp:{d2}"),
                &format!("/big.cpp:{d3}"),
                "/big.cpp",
                (i * 2 + 2) as u32,
            ));
        }
        g.merge_file_graph(FileGraph {
            path: "/big.cpp".to_string(),
            language: Language::Cpp,
            symbols: callee_symbols,
            edges: big_edges,
        });
        g.merge_file_graph(FileGraph {
            path: "/hub.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("entry", "/hub.cpp")],
            edges: hub_edges,
        });
        g
    }

    #[test]
    fn callees_byte_budget_preserves_depth_sort_order() {
        // Byte-budget truncation must not reorder the
        // (depth, symbol_id)-sorted chain
        // set in the callees direction. The helper preserves iteration
        // order, so kept records are a strict prefix of the sorted chain
        // — i.e. `max(kept depth) <= min(would-be-next-page depth)` and
        // within-depth ties are tiebroken by symbol_id ascending.
        //
        // Fixture: 30 callees spread across depths 1/2/3 (10 each). With a
        // tight `max_bytes` that fits only ~8 records, truncation must
        // happen within the depth-1 bucket (the sorted prefix). The first
        // dropped record is also at depth=1, so kept depths are
        // non-decreasing and bounded above by the dropped page's first
        // depth.
        use super::super::ENVELOPE_OVERHEAD_BYTES;
        let g = locked(graph_with_layered_callees(10));
        // Each CallChain serializes to roughly 90-110 bytes:
        //   {"symbol_id":"/big.cpp:d1_callee_NNN","file":"/big.cpp","line":N,"depth":1}
        // Budget = 800 bytes after envelope reservation → ~8 records before
        // the 9th's projected total exceeds the budget.
        let max_bytes = ENVELOPE_OVERHEAD_BYTES + 800;
        let r = callers_or_callees(
            &g,
            "/hub.cpp:entry",
            Some(3), // walk all 3 BFS depths so 30 chains are produced
            Direction::Callees,
            Some(100),
            Some(0),
            max_bytes,
        );

        let (arr, total, offset, _limit) = page_parts(&r);
        let (truncated, next_offset) = super::super::test_helpers::page_extras(&r);

        assert!(truncated, "tight max_bytes must produce truncated=true");
        let n = next_offset.expect("truncated=true must set next_offset=Some(n)");
        assert!(
            n > offset,
            "next_offset must point past the current page: next_offset={n} <= offset={offset}",
        );
        assert_eq!(total, 30, "total is the pre-pagination match count");
        assert!(
            !arr.is_empty(),
            "budget should still admit at least one record",
        );
        assert!(
            (arr.len() as u32) < 100,
            "byte budget (not count cap) must trim the page: arr.len()={}",
            arr.len(),
        );

        // Sort-determinism core assertion: kept records' depths are
        // non-decreasing (the helper preserved the handler's (depth,
        // symbol_id) sort order).
        let depths: Vec<u64> = arr
            .iter()
            .map(|h| h["depth"].as_u64().expect("depth is u64"))
            .collect();
        for win in depths.windows(2) {
            assert!(
                win[0] <= win[1],
                "kept depths must be non-decreasing: {depths:?}",
            );
        }
        // And within-depth ties are tiebroken by symbol_id ascending. Run
        // the same monotonic check on the (depth, symbol_id) tuple to
        // confirm the handler's pre-truncation sort survived the helper.
        let keys: Vec<(u64, String)> = arr
            .iter()
            .map(|h| {
                (
                    h["depth"].as_u64().unwrap(),
                    h["symbol_id"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        for win in keys.windows(2) {
            assert!(
                win[0] <= win[1],
                "kept records must remain (depth, symbol_id)-sorted: {keys:?}",
            );
        }

        // max(kept depth) <= depth of the first dropped record. With the
        // fixture's 30 chains and an 8-ish-record budget, the prefix lives
        // entirely within depth=1, so the first dropped depth is also 1.
        let max_kept_depth = depths.iter().copied().max().unwrap();
        // Re-fetch the full sorted chain set deterministically and read
        // the dropped page's first depth from the position `next_offset`.
        // The handler's (depth, symbol_id) ordering is reproduced here by
        // calling with `offset = n, limit = 1` (NO_BYTE_BUDGET so nothing
        // truncates).
        let r_next = callers_or_callees(
            &g,
            "/hub.cpp:entry",
            Some(3),
            Direction::Callees,
            Some(1),
            Some(n),
            NO_BYTE_BUDGET,
        );
        let (arr_next, _, _, _) = page_parts(&r_next);
        assert_eq!(arr_next.len(), 1, "fixture guarantees a next record exists");
        let first_dropped_depth = arr_next[0]["depth"].as_u64().unwrap();
        assert!(
            max_kept_depth <= first_dropped_depth,
            "max(kept depth)={max_kept_depth} must be <= first_dropped_depth={first_dropped_depth}",
        );
    }

    #[test]
    fn callees_byte_budget_no_truncation_with_no_budget() {
        // Anti-regression: with NO_BYTE_BUDGET (= usize::MAX), the
        // handler's existing behavior is preserved exactly — no
        // truncation, no next_offset. Locks the contract that the
        // byte-budget wiring does not affect callers that opt out, for
        // the callees direction.
        let g = locked(graph_with_n_callees(30));
        let r = callers_or_callees(
            &g,
            "/hub.cpp:entry",
            Some(1),
            Direction::Callees,
            Some(100),
            Some(0),
            NO_BYTE_BUDGET,
        );
        let (arr, total, _, _) = page_parts(&r);
        let (truncated, next_offset) = super::super::test_helpers::page_extras(&r);
        assert_eq!(arr.len(), 30);
        assert_eq!(total, 30);
        assert!(!truncated);
        assert_eq!(next_offset, None);
    }

    #[test]
    fn callees_pagination_orders_by_depth_then_symbol_id() {
        // Mirror of the callers test:
        //   entry -> b_near_one (depth 1)
        //   entry -> a_near_two (depth 1 — lex less than b_near_one)
        //   b_near_one -> z_far  (depth 2)
        // Expected order: (1, a_near_two), (1, b_near_one), (2, z_far).
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("entry", "/x.cpp"),
                sym("b_near_one", "/x.cpp"),
                sym("a_near_two", "/x.cpp"),
                sym("z_far", "/x.cpp"),
            ],
            edges: vec![
                call_edge("/x.cpp:entry", "/x.cpp:b_near_one", "/x.cpp", 1),
                call_edge("/x.cpp:entry", "/x.cpp:a_near_two", "/x.cpp", 2),
                call_edge("/x.cpp:b_near_one", "/x.cpp:z_far", "/x.cpp", 3),
            ],
        });
        let g = locked(g);
        let r = callers_or_callees(
            &g,
            "/x.cpp:entry",
            Some(2),
            Direction::Callees,
            None,
            None,
            NO_BYTE_BUDGET,
        );
        let (arr, _, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["depth"], serde_json::json!(1));
        assert_eq!(arr[0]["symbol_id"], serde_json::json!("/x.cpp:a_near_two"));
        assert_eq!(arr[1]["depth"], serde_json::json!(1));
        assert_eq!(arr[1]["symbol_id"], serde_json::json!("/x.cpp:b_near_one"));
        assert_eq!(arr[2]["depth"], serde_json::json!(2));
        assert_eq!(arr[2]["symbol_id"], serde_json::json!("/x.cpp:z_far"));
    }

    // --- get_dependencies ---

    /// Build an include edge carrying an explicit source line so tests can
    /// pin `DependencyEntry.line`. `include_edge` always uses line 1; this
    /// helper lets a fixture place each `#include` at a distinct line.
    fn include_edge_at(from: &str, to: &str, line: u32) -> Edge {
        Edge {
            from: from.to_string(),
            to: to.to_string(),
            kind: EdgeKind::Includes,
            file: from.to_string(),
            line,
        }
    }

    #[test]
    fn dependencies_missing_param_errors() {
        let g = locked(Graph::new());
        let r = get_dependencies(&g, "", None, None, NO_BYTE_BUDGET);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'file' is required");
    }

    #[test]
    fn dependencies_unknown_file_returns_empty_page() {
        let g = locked(Graph::new());
        let r = get_dependencies(&g, "/never-merged.cpp", None, None, NO_BYTE_BUDGET);
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        // Unknown file is not an error: it yields an empty Page envelope
        // (results=[], total=0), not the legacy `[]` bare array.
        let (arr, total, offset, limit) = page_parts(&r);
        assert!(arr.is_empty());
        assert_eq!(total, 0);
        assert_eq!(offset, 0);
        assert_eq!(limit, 100);
    }

    #[test]
    fn dependencies_returns_dependency_entry_page() {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/a.cpp".to_string(),
            language: Language::Cpp,
            symbols: Vec::new(),
            edges: vec![
                include_edge("/a.cpp", "/utils.h"),
                include_edge("/a.cpp", "/types.h"),
            ],
        });
        let g = locked(g);
        let r = get_dependencies(&g, "/a.cpp", None, None, NO_BYTE_BUDGET);
        let (arr, total, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 2);
        assert_eq!(total, 2);
        // The two dependencies asserted present in the old flat-array test
        // must still be present, now as DependencyEntry.file values.
        let files: Vec<&str> = arr.iter().map(|v| v["file"].as_str().unwrap()).collect();
        assert!(files.contains(&"/utils.h"));
        assert!(files.contains(&"/types.h"));
        // Every row's kind is the EdgeKind::Includes serde string.
        for row in &arr {
            assert_eq!(row["kind"], serde_json::json!("includes"));
        }
    }

    #[test]
    fn dependencies_rows_carry_include_line_and_sort_by_file_then_line() {
        // Three #include directives at distinct known lines, each resolving
        // to a real source-file-style path (no `.ini`-style entries that a
        // downstream filter would drop). Assert the Page<DependencyEntry>
        // is sorted (file, line) ascending and that each row carries its
        // include's source line verbatim.
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/a.cpp".to_string(),
            language: Language::Cpp,
            symbols: Vec::new(),
            edges: vec![
                // Inserted out of order on purpose so the handler's
                // (file, line) sort is actually exercised.
                include_edge_at("/a.cpp", "/zeta.h", 15),
                include_edge_at("/a.cpp", "/alpha.h", 5),
                include_edge_at("/a.cpp", "/mid.h", 10),
            ],
        });
        let g = locked(g);
        let r = get_dependencies(&g, "/a.cpp", None, None, NO_BYTE_BUDGET);
        let (arr, total, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 3);
        assert_eq!(total, 3);

        // Sorted by (file, line) ascending: alpha.h(5) < mid.h(10) < zeta.h(15).
        assert_eq!(arr[0]["file"], serde_json::json!("/alpha.h"));
        assert_eq!(arr[0]["line"], serde_json::json!(5));
        assert_eq!(arr[1]["file"], serde_json::json!("/mid.h"));
        assert_eq!(arr[1]["line"], serde_json::json!(10));
        assert_eq!(arr[2]["file"], serde_json::json!("/zeta.h"));
        // Pins `line` specifically: the last include's source line is 15.
        assert_eq!(arr[2]["line"], serde_json::json!(15));

        for row in &arr {
            assert_eq!(row["kind"], serde_json::json!("includes"));
        }
    }

    // --- user-path normalization ------------------------------------------

    #[test]
    fn dependencies_resolves_dot_segments_to_canonical_lookup() {
        // `get_dependencies` wraps the user-supplied `file` argument with
        // `paths::normalize_user_path` before the graph lookup. Mirrors
        // the sibling normalization test in `symbols.rs`.
        // Plant include edges keyed by a real canonical filesystem path,
        // then query the handler twice — once with the canonical form, once
        // with a `./sub/../` injected form — and assert both return the same
        // dependency list.
        //
        // The path must exist on disk so the canonicalize branch is exercised
        // (the lexical-fallback branch on a non-existent path would NOT
        // resolve dot segments, per `paths.rs` test `(d)`).
        let tmp = tempfile::TempDir::new().expect("create tempdir");
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).expect("create sub dir");
        let file_path = tmp.path().join("a.cpp");
        std::fs::write(&file_path, "// empty\n").expect("write file");

        // Capture the canonical form the graph will be keyed by. On Linux
        // this is identity for an already-canonical path; the explicit
        // canonicalize step keeps the test correct under symlinked tempdirs
        // (e.g. macOS `/var` -> `/private/var`).
        let canonical = paths::canonicalize(&file_path).expect("canonicalize file");
        let canonical_str = canonical
            .to_str()
            .expect("canonical path is valid UTF-8 on Linux");

        // Build a graph with two include edges from the canonical path.
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: canonical_str.to_string(),
            language: Language::Cpp,
            symbols: Vec::new(),
            edges: vec![
                include_edge(canonical_str, "/utils.h"),
                include_edge(canonical_str, "/types.h"),
            ],
        });
        let g = locked(g);

        // (1) Canonical form — baseline.
        let r_canonical = get_dependencies(&g, canonical_str, None, None, NO_BYTE_BUDGET);
        assert!(r_canonical.is_error.is_none() || r_canonical.is_error == Some(false));
        let (arr_canonical, _, _, _) = page_parts(&r_canonical);
        assert_eq!(arr_canonical.len(), 2);
        let strings_canonical: Vec<&str> = arr_canonical
            .iter()
            .map(|v| v["file"].as_str().unwrap())
            .collect();
        assert!(strings_canonical.contains(&"/utils.h"));
        assert!(strings_canonical.contains(&"/types.h"));

        // (2) `./sub/../a.cpp` form — load-bearing.
        let messy = tmp.path().join(".").join("sub").join("..").join("a.cpp");
        let messy_str = messy.to_str().expect("messy path is valid UTF-8 on Linux");
        assert_ne!(
            messy_str, canonical_str,
            "messy fixture must differ from canonical for the test to be meaningful"
        );

        let r_messy = get_dependencies(&g, messy_str, None, None, NO_BYTE_BUDGET);
        assert!(
            r_messy.is_error.is_none() || r_messy.is_error == Some(false),
            "messy form must succeed after normalize: body={}",
            body_text(&r_messy),
        );
        let (arr_messy, _, _, _) = page_parts(&r_messy);
        assert_eq!(
            arr_messy.len(),
            2,
            "messy form must return the same dep list as canonical",
        );
        let mut strings_messy: Vec<&str> = arr_messy
            .iter()
            .map(|v| v["file"].as_str().unwrap())
            .collect();
        let mut strings_canonical_sorted = strings_canonical.clone();
        strings_messy.sort();
        strings_canonical_sorted.sort();
        assert_eq!(strings_messy, strings_canonical_sorted);
    }
}
