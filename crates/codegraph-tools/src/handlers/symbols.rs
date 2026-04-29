//! Symbol-query handlers: `get_file_symbols`, `search_symbols`,
//! `get_symbol_detail`, `get_symbol_summary`.
//!
//! Each function takes the shared graph (read-locked by the caller) and
//! returns a `CallToolResult` ready to hand back to rmcp. Error wording
//! matches the Go reference at `internal/tools/symbols.go` byte-for-byte.

use std::collections::HashMap;
use std::path::Path;

use codegraph_graph::{Graph, SearchParams};
use parking_lot::RwLock;
use rmcp::model::CallToolResult;
use serde::Serialize;

use super::{
    kind_str, parse_kind, parse_language, suggest_symbols, symbol_to_result, tool_error,
    tool_success_json, SymbolResult,
};

/// `get_file_symbols` body. Returns a tool error when `file` is empty,
/// when the file is unknown to the graph, or — implicitly — when the file
/// has no symbols at all (the Go binary's wording is "no symbols found in
/// file: <file>").
///
/// Filtering by `top_level_only` happens **after** the empty-file check so
/// the error wording is always about the file itself, not the filter
/// result.
pub fn get_file_symbols(
    graph: &RwLock<Graph>,
    file: &str,
    top_level_only: bool,
    brief: bool,
) -> CallToolResult {
    if file.is_empty() {
        return tool_error("'file' is required");
    }

    let symbols = graph.read().file_symbols(Path::new(file));
    if symbols.is_empty() {
        return tool_error(format!("no symbols found in file: {file}"));
    }

    // `Vec::with_capacity(symbols.len())` so an empty filter result still
    // serializes as `[]` (never null) — matches the Go behavior.
    let mut results: Vec<SymbolResult> = Vec::with_capacity(symbols.len());
    for s in &symbols {
        if top_level_only && !s.parent.is_empty() {
            continue;
        }
        results.push(symbol_to_result(s, brief));
    }

    tool_success_json(&results)
}

/// `search_symbols` response envelope. Field order mirrors Go's anonymous
/// struct in `handleSearchSymbols`.
#[derive(Debug, Serialize)]
struct SearchResponse {
    results: Vec<SymbolResult>,
    total: u32,
    offset: u32,
    limit: u32,
}

/// Inputs to [`search_symbols`]. Bundled into a struct so the handler
/// signature stays under clippy's `too_many_arguments` threshold without
/// reaching for an `allow` attribute.
#[derive(Debug, Default)]
pub struct SearchSymbolsInput<'a> {
    pub query: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub namespace: Option<&'a str>,
    pub language: Option<&'a str>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub brief: bool,
}

/// `search_symbols` body. Validates that at least one filter was supplied,
/// parses string-typed filters into their typed forms, then delegates to
/// `Graph::search`. The pagination envelope is always present — `total`
/// is reported pre-pagination so callers can render "page X of Y" UIs.
pub fn search_symbols(graph: &RwLock<Graph>, input: SearchSymbolsInput<'_>) -> CallToolResult {
    let query_str = input.query.unwrap_or("");
    let kind_str_ref = input.kind.unwrap_or("");
    let namespace_str = input.namespace.unwrap_or("");
    let language_str = input.language.unwrap_or("");

    if query_str.is_empty()
        && kind_str_ref.is_empty()
        && namespace_str.is_empty()
        && language_str.is_empty()
    {
        return tool_error("'query', 'kind', 'namespace', or 'language' is required");
    }

    let parsed_kind = if kind_str_ref.is_empty() {
        None
    } else {
        match parse_kind(kind_str_ref) {
            Some(k) => Some(k),
            None => return tool_error(format!("invalid kind: {kind_str_ref}")),
        }
    };

    let parsed_language = if language_str.is_empty() {
        None
    } else {
        match parse_language(language_str) {
            Some(l) => Some(l),
            None => return tool_error(format!("invalid language: {language_str}")),
        }
    };

    let resolved_limit = input.limit.filter(|&l| l > 0).unwrap_or(20);
    let resolved_offset = input.offset.unwrap_or(0);

    let sr = graph.read().search(SearchParams {
        pattern: query_str.to_string(),
        kind: parsed_kind,
        namespace: namespace_str.to_string(),
        language: parsed_language,
        limit: resolved_limit,
        offset: resolved_offset,
    });

    let results: Vec<SymbolResult> = sr
        .symbols
        .iter()
        .map(|s| symbol_to_result(s, input.brief))
        .collect();

    let response = SearchResponse {
        results,
        total: sr.total,
        offset: resolved_offset,
        limit: resolved_limit,
    };
    tool_success_json(&response)
}

/// `get_symbol_detail` body. Returns full detail (brief=false) on hit; on
/// miss, attaches a did-you-mean suggestion when any candidate symbols
/// match the substring.
pub fn get_symbol_detail(graph: &RwLock<Graph>, symbol: &str) -> CallToolResult {
    if symbol.is_empty() {
        return tool_error("'symbol' is required");
    }

    let g = graph.read();
    if let Some(s) = g.symbol_detail(symbol) {
        let result = symbol_to_result(&s, false);
        return tool_success_json(&result);
    }

    let suggestions = suggest_symbols(&g, symbol, 5);
    drop(g);
    if suggestions.is_empty() {
        tool_error(format!("symbol not found: {symbol:?}"))
    } else {
        tool_error(format!(
            "symbol not found: {symbol:?}. Did you mean: {suggestions}?"
        ))
    }
}

/// `get_symbol_summary` body. Returns the namespace → kind-string → count
/// map as JSON. Unlike Go (which serializes `map[string]map[parser.SymbolKind]int`
/// directly), we re-key the inner map by the lowercase kind string so the
/// JSON output uses the same kind names as every other surface.
pub fn get_symbol_summary(graph: &RwLock<Graph>, file: Option<&str>) -> CallToolResult {
    let path: Option<&Path> = file.filter(|s| !s.is_empty()).map(Path::new);
    let summary = graph.read().symbol_summary(path);

    // Re-key SymbolKind -> &str for stable JSON output.
    let mut response: HashMap<String, HashMap<&'static str, u32>> =
        HashMap::with_capacity(summary.len());
    for (ns, kinds) in summary {
        let mut inner = HashMap::with_capacity(kinds.len());
        for (k, count) in kinds {
            inner.insert(kind_str(k), count);
        }
        response.insert(ns, inner);
    }
    tool_success_json(&response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::{FileGraph, Language, Symbol, SymbolKind};

    fn sym(name: &str, kind: SymbolKind, file: &str, parent: &str) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind,
            file: file.to_string(),
            line: 1,
            column: 0,
            end_line: 1,
            signature: format!("sig {name}"),
            namespace: String::new(),
            parent: parent.to_string(),
            language: Language::Cpp,
        }
    }

    fn small_graph() -> Graph {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/a.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("foo", SymbolKind::Function, "/a.cpp", ""),
                sym("Bar", SymbolKind::Class, "/a.cpp", ""),
                sym("do_thing", SymbolKind::Method, "/a.cpp", "Bar"),
            ],
            edges: Vec::new(),
        });
        g
    }

    fn locked(g: Graph) -> RwLock<Graph> {
        RwLock::new(g)
    }

    fn body_text(r: &CallToolResult) -> String {
        r.content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.to_string())
            .unwrap_or_default()
    }

    // --- get_file_symbols ---

    #[test]
    fn file_symbols_missing_file_param_errors() {
        let g = locked(Graph::new());
        let r = get_file_symbols(&g, "", false, true);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'file' is required");
    }

    #[test]
    fn file_symbols_unknown_file_errors() {
        let g = locked(Graph::new());
        let r = get_file_symbols(&g, "/missing.cpp", false, true);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "no symbols found in file: /missing.cpp");
    }

    #[test]
    fn file_symbols_returns_full_list_in_brief_mode() {
        let g = locked(small_graph());
        let r = get_file_symbols(&g, "/a.cpp", false, true);
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        // All three include id/name/kind/file/line/namespace? namespace is "" so omitted.
        for entry in arr {
            assert!(entry.get("id").is_some());
            assert!(entry.get("name").is_some());
            assert!(entry.get("kind").is_some());
            assert!(entry.get("file").is_some());
            // brief: signature must be absent.
            assert!(entry.get("signature").is_none());
        }
    }

    #[test]
    fn file_symbols_top_level_only_filters_out_methods() {
        let g = locked(small_graph());
        let r = get_file_symbols(&g, "/a.cpp", true, true);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let arr = parsed.as_array().unwrap();
        // 3 symbols total, but `do_thing` has parent="Bar" so it's filtered.
        assert_eq!(arr.len(), 2);
        for entry in arr {
            assert!(
                entry.get("parent").is_none(),
                "no parent on top-level entries"
            );
        }
    }

    #[test]
    fn file_symbols_brief_false_includes_signature() {
        let g = locked(small_graph());
        let r = get_file_symbols(&g, "/a.cpp", false, false);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let arr = parsed.as_array().unwrap();
        for entry in arr {
            assert!(
                entry.get("signature").is_some(),
                "signature included in non-brief"
            );
        }
    }

    // --- search_symbols ---

    fn search_input<'a>() -> SearchSymbolsInput<'a> {
        SearchSymbolsInput {
            brief: true,
            ..SearchSymbolsInput::default()
        }
    }

    #[test]
    fn search_symbols_no_filter_errors() {
        let g = locked(small_graph());
        let r = search_symbols(&g, search_input());
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            body_text(&r),
            "'query', 'kind', 'namespace', or 'language' is required"
        );
    }

    #[test]
    fn search_symbols_all_empty_strings_errors() {
        let g = locked(small_graph());
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some(""),
                kind: Some(""),
                namespace: Some(""),
                language: Some(""),
                ..search_input()
            },
        );
        assert_eq!(r.is_error, Some(true));
    }

    #[test]
    fn search_symbols_unknown_kind_errors() {
        let g = locked(small_graph());
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                kind: Some("widget"),
                ..search_input()
            },
        );
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "invalid kind: widget");
    }

    #[test]
    fn search_symbols_unknown_language_errors() {
        let g = locked(small_graph());
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                language: Some("ruby"),
                ..search_input()
            },
        );
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "invalid language: ruby");
    }

    #[test]
    fn search_symbols_returns_pagination_envelope() {
        let g = locked(small_graph());
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("foo"),
                limit: Some(10),
                offset: Some(0),
                ..search_input()
            },
        );
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert!(parsed.get("results").is_some());
        assert_eq!(parsed["total"], serde_json::json!(1));
        assert_eq!(parsed["offset"], serde_json::json!(0));
        assert_eq!(parsed["limit"], serde_json::json!(10));
    }

    #[test]
    fn search_symbols_default_limit_when_zero() {
        let g = locked(small_graph());
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("foo"),
                limit: Some(0),
                ..search_input()
            },
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        // limit=0 normalized to 20.
        assert_eq!(parsed["limit"], serde_json::json!(20));
    }

    #[test]
    fn search_symbols_kind_only_filter_accepted() {
        let g = locked(small_graph());
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                kind: Some("function"),
                ..search_input()
            },
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        // 1 Function symbol in the small graph (foo).
        assert_eq!(parsed["total"], serde_json::json!(1));
    }

    #[test]
    fn search_symbols_language_only_filter_accepted() {
        let g = locked(small_graph());
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                language: Some("cpp"),
                ..search_input()
            },
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        // All three sample symbols are Cpp.
        assert_eq!(parsed["total"], serde_json::json!(3));
    }

    // --- get_symbol_detail ---

    #[test]
    fn get_symbol_detail_missing_param_errors() {
        let g = locked(Graph::new());
        let r = get_symbol_detail(&g, "");
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'symbol' is required");
    }

    #[test]
    fn get_symbol_detail_known_id_returns_full_symbol() {
        let g = locked(small_graph());
        let r = get_symbol_detail(&g, "/a.cpp:foo");
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(parsed["name"], serde_json::json!("foo"));
        assert_eq!(parsed["kind"], serde_json::json!("function"));
        // Non-brief: signature present.
        assert!(parsed.get("signature").is_some());
    }

    #[test]
    fn get_symbol_detail_unknown_id_with_suggestions() {
        let g = locked(small_graph());
        // "fo" is a substring of "foo" — graph.search_symbols should suggest it.
        let r = get_symbol_detail(&g, "fo");
        assert_eq!(r.is_error, Some(true));
        let text = body_text(&r);
        assert!(text.starts_with("symbol not found: \"fo\""), "got: {text}");
        assert!(text.contains("Did you mean: "), "got: {text}");
    }

    #[test]
    fn get_symbol_detail_unknown_id_no_suggestions() {
        let g = locked(Graph::new());
        let r = get_symbol_detail(&g, "nope");
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "symbol not found: \"nope\"");
    }

    // --- get_symbol_summary ---

    #[test]
    fn symbol_summary_whole_graph() {
        let g = locked(small_graph());
        let r = get_symbol_summary(&g, None);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        // 3 symbols all in namespace "".
        let inner = parsed[""].as_object().unwrap();
        assert_eq!(inner["function"], serde_json::json!(1));
        assert_eq!(inner["class"], serde_json::json!(1));
        assert_eq!(inner["method"], serde_json::json!(1));
    }

    #[test]
    fn symbol_summary_empty_graph_returns_empty_object() {
        let g = locked(Graph::new());
        let r = get_symbol_summary(&g, None);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let obj = parsed.as_object().unwrap();
        assert!(obj.is_empty());
    }

    #[test]
    fn symbol_summary_file_scoped() {
        let mut g = small_graph();
        g.merge_file_graph(FileGraph {
            path: "/b.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("only_in_b", SymbolKind::Function, "/b.cpp", "")],
            edges: Vec::new(),
        });
        let g = locked(g);
        let r = get_symbol_summary(&g, Some("/b.cpp"));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let inner = parsed[""].as_object().unwrap();
        assert_eq!(inner["function"], serde_json::json!(1));
        // No method or class — those are in /a.cpp only.
        assert!(!inner.contains_key("method"));
        assert!(!inner.contains_key("class"));
    }
}
