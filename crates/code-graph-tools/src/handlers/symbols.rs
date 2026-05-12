//! Symbol-query handlers: `get_file_symbols`, `search_symbols`,
//! `get_symbol_detail`, `get_symbol_summary`.
//!
//! Each function takes the shared graph (read-locked by the caller) and
//! returns a `CallToolResult` ready to hand back to rmcp. Error wording
//! matches the Go reference at `internal/tools/symbols.go` byte-for-byte.

use std::collections::HashMap;
use std::path::Path;

use code_graph_graph::{Graph, SearchParams};
use parking_lot::RwLock;
use rmcp::model::CallToolResult;

use super::{
    kind_str, parse_kind, parse_language, suggest_symbols, symbol_to_result, tool_error,
    tool_success_json, Page, SymbolResult,
};

/// `get_file_symbols` body. Returns a tool error when `file` is empty or
/// when the file has no symbols at all (raw set empty — the Go binary's
/// wording is preserved verbatim: `"no symbols found in file: <file>"`).
///
/// Output is the shared [`Page`]`<`[`SymbolResult`]`>` envelope. The
/// post-filter result set is sorted by `symbol_id` ascending so page 1 +
/// page 2 partition the rows deterministically across calls, then sliced
/// by the resolved offset/limit. `total` reports the post-filter,
/// pre-pagination count so callers can render "page X of Y" UIs.
///
/// Order of operations is load-bearing: the empty-raw-set check runs
/// **before** filtering and pagination so a misspelled file path always
/// surfaces the existing diagnostic error wording. A non-empty raw set
/// that filters to empty (e.g. `top_level_only=true` on a file containing
/// only methods) returns an envelope with `results: []` and `total: 0`,
/// not an error — that distinction is what lets the agent tell "wrong
/// file" apart from "filter excluded everything".
///
/// Defaults: `limit = 100`, `offset = 0`. `limit = 0` means "use the
/// default" (mirrors `search_symbols` and `get_orphans`); `limit` is
/// silently clamped at 1000. `offset >= total` returns an empty `results`
/// page with the correct `total`.
pub fn get_file_symbols(
    graph: &RwLock<Graph>,
    file: &str,
    top_level_only: bool,
    brief: bool,
    limit: Option<u32>,
    offset: Option<u32>,
) -> CallToolResult {
    if file.is_empty() {
        return tool_error("'file' is required");
    }

    let symbols = graph.read().file_symbols(Path::new(file));
    // Raw-set-empty -> existing tool error. Wording preserved byte-for-byte
    // so agents that match against this string keep working.
    if symbols.is_empty() {
        return tool_error(format!("no symbols found in file: {file}"));
    }

    // Resolve defaults: zero-or-missing limit -> 100; clamp at 1000.
    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(100).min(1000);
    let resolved_offset = offset.unwrap_or(0);

    // Apply `top_level_only` filter into a Vec<SymbolResult>. Build the
    // Vec eagerly so we can sort + slice + count it in subsequent steps.
    let mut results: Vec<SymbolResult> = Vec::with_capacity(symbols.len());
    for s in &symbols {
        if top_level_only && !s.parent.is_empty() {
            continue;
        }
        results.push(symbol_to_result(s, brief));
    }

    let total = results.len() as u32;

    // Sort by symbol_id ascending so pagination is deterministic across
    // calls. `Graph::file_symbols` returns symbols in graph-merge order
    // which is stable per-build but not part of the wire contract; sorting
    // canonicalizes the sequence.
    results.sort_by(|a, b| a.id.cmp(&b.id));

    // Bounds-safe slice via skip+take.
    let page: Vec<SymbolResult> = results
        .into_iter()
        .skip(resolved_offset as usize)
        .take(resolved_limit as usize)
        .collect();

    let response = Page::<SymbolResult> {
        results: page,
        total,
        offset: resolved_offset,
        limit: resolved_limit,
        truncated: false,
        next_offset: None,
    };
    tool_success_json(&response)
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
        // Four-term message (vs. Go's three) — `language` is a Rust-only filter
        // addition (Phase 1's Symbol::language is its first consumer). Listing
        // it here keeps the error truthful about what satisfies the validation.
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

    let resolved_limit = input.limit.filter(|&l| l > 0).unwrap_or(20).min(1000);
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

    let response = Page::<SymbolResult> {
        results,
        total: sr.total,
        offset: resolved_offset,
        limit: resolved_limit,
        truncated: false,
        next_offset: None,
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
    use super::super::test_helpers::{body_text, page_parts};
    use super::*;
    use code_graph_core::{FileGraph, Language, Symbol, SymbolKind};

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

    // --- get_file_symbols ---

    #[test]
    fn file_symbols_missing_file_param_errors() {
        let g = locked(Graph::new());
        let r = get_file_symbols(&g, "", false, true, None, None);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'file' is required");
    }

    #[test]
    fn file_symbols_unknown_file_errors() {
        let g = locked(Graph::new());
        let r = get_file_symbols(&g, "/missing.cpp", false, true, None, None);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "no symbols found in file: /missing.cpp");
    }

    #[test]
    fn file_symbols_empty_raw_set_returns_error() {
        // Diagnostic-UX guard: empty raw set MUST surface the existing
        // "no symbols found in file: <file>" tool error — NOT an empty
        // pagination envelope. The wording is preserved verbatim because
        // agents may match against this string. Pagination args are not
        // consulted on this path.
        let g = locked(Graph::new());
        let r = get_file_symbols(&g, "/missing.cpp", false, true, Some(50), Some(10));
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "no symbols found in file: /missing.cpp");
        // Body must be the bare error string, not a JSON envelope.
        assert!(serde_json::from_str::<serde_json::Value>(&body_text(&r))
            .map(|v| v.get("results").is_none())
            .unwrap_or(true));
    }

    #[test]
    fn file_symbols_empty_post_filter_returns_empty_envelope() {
        // Raw set non-empty (file has 3 symbols) but `top_level_only=true`
        // on a fixture where every symbol has a parent → envelope with
        // results=[] and total=0. NOT a tool error — that distinction lets
        // the agent tell "wrong file" apart from "filter excluded all rows".
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/methods_only.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("a", SymbolKind::Method, "/methods_only.cpp", "Cls"),
                sym("b", SymbolKind::Method, "/methods_only.cpp", "Cls"),
                sym("c", SymbolKind::Method, "/methods_only.cpp", "Cls"),
            ],
            edges: Vec::new(),
        });
        let g = locked(g);
        let r = get_file_symbols(&g, "/methods_only.cpp", true, true, None, None);
        // Not a tool error.
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let (arr, total, offset, limit) = page_parts(&r);
        assert!(arr.is_empty());
        assert_eq!(total, 0);
        assert_eq!(offset, 0);
        assert_eq!(limit, 100);
    }

    #[test]
    fn file_symbols_returns_full_list_in_brief_mode() {
        let g = locked(small_graph());
        let r = get_file_symbols(&g, "/a.cpp", false, true, None, None);
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let (arr, total, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 3);
        assert_eq!(total, 3);
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
        let r = get_file_symbols(&g, "/a.cpp", true, true, None, None);
        let (arr, total, _, _) = page_parts(&r);
        // 3 symbols total, but `do_thing` has parent="Bar" so it's filtered.
        assert_eq!(arr.len(), 2);
        assert_eq!(total, 2);
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
        let r = get_file_symbols(&g, "/a.cpp", false, false, None, None);
        let (arr, _, _, _) = page_parts(&r);
        for entry in arr {
            assert!(
                entry.get("signature").is_some(),
                "signature included in non-brief"
            );
        }
    }

    // --- Phase 3 pagination invariants ------------------------------------

    /// Build a graph with `n` free-function symbols in a single file. Names
    /// are zero-padded to 3 digits so the natural `symbol_id` sort order is
    /// predictable for assertions.
    fn graph_with_n_file_symbols(n: usize) -> Graph {
        let mut g = Graph::new();
        let mut symbols: Vec<Symbol> = Vec::with_capacity(n);
        for i in 0..n {
            symbols.push(sym(
                &format!("func_{i:03}"),
                SymbolKind::Function,
                "/big.cpp",
                "",
            ));
        }
        g.merge_file_graph(FileGraph {
            path: "/big.cpp".to_string(),
            language: Language::Cpp,
            symbols,
            edges: vec![],
        });
        g
    }

    #[test]
    fn file_symbols_default_limit_is_100() {
        let g = locked(graph_with_n_file_symbols(120));
        let r = get_file_symbols(&g, "/big.cpp", false, true, None, None);
        let (arr, total, offset, limit) = page_parts(&r);
        assert_eq!(arr.len(), 100);
        assert_eq!(total, 120);
        assert_eq!(offset, 0);
        assert_eq!(limit, 100);
    }

    #[test]
    fn file_symbols_page_1_and_page_2_cover_full_set() {
        let g = locked(graph_with_n_file_symbols(150));
        let p1 = get_file_symbols(&g, "/big.cpp", false, true, Some(100), Some(0));
        let p2 = get_file_symbols(&g, "/big.cpp", false, true, Some(100), Some(100));
        let (a1, t1, _, _) = page_parts(&p1);
        let (a2, t2, _, _) = page_parts(&p2);
        assert_eq!(a1.len(), 100);
        assert_eq!(a2.len(), 50);
        assert_eq!(t1, 150);
        assert_eq!(t2, 150);

        let mut ids: Vec<String> = a1
            .iter()
            .chain(a2.iter())
            .map(|e| e["id"].as_str().unwrap().to_string())
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
    fn file_symbols_total_is_pre_pagination_count() {
        let g = locked(graph_with_n_file_symbols(150));
        let r1 = get_file_symbols(&g, "/big.cpp", false, true, Some(50), Some(0));
        let r2 = get_file_symbols(&g, "/big.cpp", false, true, Some(50), Some(50));
        let r3 = get_file_symbols(&g, "/big.cpp", false, true, Some(10), Some(140));
        let (_, t1, _, _) = page_parts(&r1);
        let (_, t2, _, _) = page_parts(&r2);
        let (_, t3, _, _) = page_parts(&r3);
        assert_eq!(t1, 150);
        assert_eq!(t2, 150);
        assert_eq!(t3, 150);
    }

    #[test]
    fn file_symbols_limit_clamps_at_1000() {
        // 5-symbol fixture with limit = 999_999 silently clamps to 1000;
        // the response echoes the clamped value. The 5-row count also
        // verifies take(1000) doesn't drop entries on a small set.
        let g = locked(graph_with_n_file_symbols(5));
        let r = get_file_symbols(&g, "/big.cpp", false, true, Some(999_999), None);
        let (arr, _, _, limit) = page_parts(&r);
        assert_eq!(limit, 1000);
        assert_eq!(arr.len(), 5);
    }

    #[test]
    fn file_symbols_zero_limit_uses_default() {
        let g = locked(graph_with_n_file_symbols(5));
        let r = get_file_symbols(&g, "/big.cpp", false, true, Some(0), None);
        let (_, _, _, limit) = page_parts(&r);
        assert_eq!(limit, 100);
    }

    #[test]
    fn file_symbols_offset_beyond_total_returns_empty() {
        let g = locked(graph_with_n_file_symbols(5));
        let r = get_file_symbols(&g, "/big.cpp", false, true, None, Some(999));
        let (arr, total, offset, limit) = page_parts(&r);
        assert!(arr.is_empty());
        assert_eq!(total, 5);
        assert_eq!(offset, 999);
        assert_eq!(limit, 100);
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
    fn search_symbols_validation_lists_all_four_filters() {
        // Deliberate divergence from Go (which has three terms; Rust has the
        // language filter too). Locked in here so future edits to the message
        // are caught.
        let g = locked(small_graph());
        let r = search_symbols(&g, search_input());
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
