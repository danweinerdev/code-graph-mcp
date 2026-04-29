//! Call-graph and dependency handlers: `get_callers`, `get_callees`,
//! `get_dependencies`.
//!
//! Mirrors `internal/tools/query.go` from the Go binary. Did-you-mean
//! suggestions on `get_callers` / `get_callees` only fire when the BFS
//! result is empty AND the symbol is unknown — the Go behavior is to
//! return `[]` for a known symbol that just has no callers/callees, and
//! to return a tool error only when the symbol itself isn't in the graph.

use std::path::Path;

use codegraph_graph::Graph;
use parking_lot::RwLock;
use rmcp::model::CallToolResult;

use super::{suggest_symbols, tool_error, tool_success_json};

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
pub fn callers_or_callees(
    graph: &RwLock<Graph>,
    symbol: &str,
    depth: Option<u32>,
    direction: Direction,
) -> CallToolResult {
    if symbol.is_empty() {
        return tool_error("'symbol' is required");
    }

    let depth = depth.filter(|&d| d > 0).unwrap_or(1);

    let g = graph.read();
    let chains = match direction {
        Direction::Callers => g.callers(symbol, depth),
        Direction::Callees => g.callees(symbol, depth),
    };

    if chains.is_empty() {
        // Symbol may not exist at all — surface a did-you-mean error.
        // If it exists but has no callers/callees, return `[]`.
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

    tool_success_json(&chains)
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
    use super::*;
    use codegraph_core::{Edge, EdgeKind, FileGraph, Language, Symbol, SymbolKind};

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

    fn body_text(r: &CallToolResult) -> String {
        r.content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default()
    }

    fn locked(g: Graph) -> RwLock<Graph> {
        RwLock::new(g)
    }

    // --- callers / callees ---

    #[test]
    fn callers_missing_symbol_param_errors() {
        let g = locked(Graph::new());
        let r = callers_or_callees(&g, "", None, Direction::Callers);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'symbol' is required");
    }

    #[test]
    fn callees_missing_symbol_param_errors() {
        let g = locked(Graph::new());
        let r = callers_or_callees(&g, "", None, Direction::Callees);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'symbol' is required");
    }

    #[test]
    fn callers_returns_chain_for_known_symbol() {
        let g = locked(graph_with_calls());
        let r = callers_or_callees(&g, "/x.cpp:c", Some(1), Direction::Callers);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["symbol_id"], serde_json::json!("/x.cpp:b"));
    }

    #[test]
    fn callers_depth_default_one() {
        let g = locked(graph_with_calls());
        let r = callers_or_callees(&g, "/x.cpp:c", None, Direction::Callers);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 1);
    }

    #[test]
    fn callees_returns_chain_for_known_symbol() {
        let g = locked(graph_with_calls());
        let r = callers_or_callees(&g, "/x.cpp:a", Some(2), Direction::Callees);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        let names: Vec<String> = arr
            .iter()
            .map(|h| h["symbol_id"].as_str().unwrap().to_string())
            .collect();
        assert!(names.contains(&"/x.cpp:b".to_string()));
        assert!(names.contains(&"/x.cpp:c".to_string()));
    }

    #[test]
    fn callers_known_symbol_with_no_callers_returns_empty_array() {
        // `/x.cpp:a` has no callers. Symbol exists in graph → return `[]`.
        let g = locked(graph_with_calls());
        let r = callers_or_callees(&g, "/x.cpp:a", Some(1), Direction::Callers);
        // Not an error; empty array.
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        assert_eq!(body_text(&r), "[]");
    }

    #[test]
    fn callees_known_symbol_with_no_callees_returns_empty_array() {
        // `/x.cpp:c` has no callees. Symbol exists → return `[]`.
        let g = locked(graph_with_calls());
        let r = callers_or_callees(&g, "/x.cpp:c", Some(1), Direction::Callees);
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        assert_eq!(body_text(&r), "[]");
    }

    #[test]
    fn callers_unknown_symbol_with_suggestions() {
        let g = locked(graph_with_calls());
        // "a" matches the substring of `/x.cpp:a`. The graph has `a`/`b`/`c`
        // — `a` should be suggested via search_symbols substring matching.
        let r = callers_or_callees(&g, "a", None, Direction::Callers);
        assert_eq!(r.is_error, Some(true));
        let text = body_text(&r);
        assert!(text.starts_with("symbol not found: \"a\""), "got: {text}");
        assert!(text.contains("Did you mean: "), "got: {text}");
    }

    #[test]
    fn callers_unknown_symbol_no_suggestions() {
        let g = locked(Graph::new());
        let r = callers_or_callees(&g, "nope", None, Direction::Callers);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "symbol not found: \"nope\"");
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
