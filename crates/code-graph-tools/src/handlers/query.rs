//! Call-graph and dependency handlers: `get_callers`, `get_callees`,
//! `get_dependencies`.
//!
//! Mirrors `internal/tools/query.go` from the Go binary. Did-you-mean
//! suggestions on `get_callers` / `get_callees` only fire when the BFS
//! result is empty AND the symbol is unknown — the Go behavior is to
//! return `[]` for a known symbol that just has no callers/callees, and
//! to return a tool error only when the symbol itself isn't in the graph.

use std::path::Path;

use code_graph_graph::{CallChain, Graph};
use parking_lot::RwLock;
use rmcp::model::CallToolResult;

use super::{suggest_symbols, tool_error, tool_success_json, Page};

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

    // Bounds-safe slice via skip+take.
    let page: Vec<CallChain> = chains
        .into_iter()
        .skip(resolved_offset as usize)
        .take(resolved_limit as usize)
        .collect();

    let response = Page::<CallChain> {
        results: page,
        total,
        offset: resolved_offset,
        limit: resolved_limit,
    };
    tool_success_json(&response)
}

/// `get_dependencies` body. Returns the dependency list as a JSON array
/// of strings — never `null`, even for an unknown file.
pub fn get_dependencies(graph: &RwLock<Graph>, file: &str) -> CallToolResult {
    if file.is_empty() {
        return tool_error("'file' is required");
    }

    let deps = graph.read().file_dependencies(Path::new(file));
    let strings: Vec<String> = deps
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    tool_success_json(&strings)
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{body_text, page_parts};
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
        let r = callers_or_callees(&g, "", None, Direction::Callers, None, None);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'symbol' is required");
    }

    #[test]
    fn callees_missing_symbol_param_errors() {
        let g = locked(Graph::new());
        let r = callers_or_callees(&g, "", None, Direction::Callees, None, None);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'symbol' is required");
    }

    #[test]
    fn callers_returns_chain_for_known_symbol() {
        let g = locked(graph_with_calls());
        let r = callers_or_callees(&g, "/x.cpp:c", Some(1), Direction::Callers, None, None);
        let (arr, _, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["symbol_id"], serde_json::json!("/x.cpp:b"));
    }

    #[test]
    fn callers_depth_default_one() {
        let g = locked(graph_with_calls());
        let r = callers_or_callees(&g, "/x.cpp:c", None, Direction::Callers, None, None);
        let (arr, _, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 1);
    }

    #[test]
    fn callees_returns_chain_for_known_symbol() {
        let g = locked(graph_with_calls());
        let r = callers_or_callees(&g, "/x.cpp:a", Some(2), Direction::Callees, None, None);
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
        let r = callers_or_callees(&g, "/x.cpp:a", Some(1), Direction::Callers, None, None);
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
        let r = callers_or_callees(&g, "/x.cpp:c", Some(1), Direction::Callees, None, None);
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
        let r = callers_or_callees(&g, "a", None, Direction::Callers, None, None);
        assert_eq!(r.is_error, Some(true));
        let text = body_text(&r);
        assert!(text.starts_with("symbol not found: \"a\""), "got: {text}");
        assert!(text.contains("Did you mean: "), "got: {text}");
    }

    #[test]
    fn callers_unknown_symbol_no_suggestions() {
        let g = locked(Graph::new());
        let r = callers_or_callees(&g, "nope", None, Direction::Callers, None, None);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "symbol not found: \"nope\"");
    }

    // --- Phase 3 pagination invariants ------------------------------------

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
        );
        let p2 = callers_or_callees(
            &g,
            "/hub.cpp:target",
            Some(1),
            Direction::Callers,
            Some(100),
            Some(100),
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
        );
        let r2 = callers_or_callees(
            &g,
            "/hub.cpp:target",
            Some(1),
            Direction::Callers,
            Some(50),
            Some(50),
        );
        let r3 = callers_or_callees(
            &g,
            "/hub.cpp:target",
            Some(1),
            Direction::Callers,
            Some(10),
            Some(140),
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
        let r = callers_or_callees(&g, "/x.cpp:target", Some(2), Direction::Callers, None, None);
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
        );
        let p2 = callers_or_callees(
            &g,
            "/hub.cpp:entry",
            Some(1),
            Direction::Callees,
            Some(100),
            Some(100),
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
        );
        let r2 = callers_or_callees(
            &g,
            "/hub.cpp:entry",
            Some(1),
            Direction::Callees,
            Some(50),
            Some(50),
        );
        let r3 = callers_or_callees(
            &g,
            "/hub.cpp:entry",
            Some(1),
            Direction::Callees,
            Some(10),
            Some(140),
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
        );
        let (arr, total, offset, limit) = page_parts(&r);
        assert!(arr.is_empty());
        assert_eq!(total, 5);
        assert_eq!(offset, 999);
        assert_eq!(limit, 100);
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
        let r = callers_or_callees(&g, "/x.cpp:entry", Some(2), Direction::Callees, None, None);
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

    #[test]
    fn dependencies_missing_param_errors() {
        let g = locked(Graph::new());
        let r = get_dependencies(&g, "");
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'file' is required");
    }

    #[test]
    fn dependencies_unknown_file_returns_empty_array() {
        let g = locked(Graph::new());
        let r = get_dependencies(&g, "/never-merged.cpp");
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        assert_eq!(body_text(&r), "[]");
    }

    #[test]
    fn dependencies_returns_string_array() {
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
        let r = get_dependencies(&g, "/a.cpp");
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let strings: Vec<&str> = arr.iter().map(|v| v.as_str().unwrap()).collect();
        assert!(strings.contains(&"/utils.h"));
        assert!(strings.contains(&"/types.h"));
    }
}
