//! Typed core for `get_file_symbols`, `search_symbols`, `get_symbol_detail`,
//! `get_symbol_summary`, and phase 1's `get_symbol_at`.
//!
//! All five are GATED tools (confirmed against the `server.rs` call sites at
//! `get_file_symbols`, `search_symbols`, `get_symbol_detail`,
//! `get_symbol_summary`, `get_symbol_at`): each function here calls the
//! core-level [`require_indexed`] at its own entry — a NEW call site per
//! Design Decision 8, since `handlers::symbols` never contained one.
//!
//! `handlers::symbols`'s five public functions keep their exact signatures
//! (no `ServerInner`/indexed-flag parameter — Decision 3), so they cannot
//! supply a real indexed flag to the core. Each adapter hardcodes
//! `indexed = true`: the MCP path already ran `ServerInner::require_indexed`
//! in `server.rs` before ever reaching the handler, so `true` is always
//! correct on that path. The `indexed` parameter exists so a future direct
//! caller (e.g. Track B's CLI) can pass its own real flag and get the
//! domain error `handlers::symbols`'s existing tests never had to exercise.
//!
//! `SearchSymbolsInput<'a>` stays in `handlers::symbols` (Decision 4); this
//! module imports it rather than moving it.
//!
//! Pure-logic helpers (`is_plain_identifier`, `max_distance_for_query`,
//! `levenshtein`) stay in `handlers::symbols` rather than moving here,
//! mirroring Decision 4's precedent for `core::query`'s helper set:
//! `handlers::symbols`'s own `#[cfg(test)]` module asserts against them
//! directly via `use super::*`, and moving them would force edits to those
//! (deliberately unmodified, Decision 6) tests. `levenshtein_suggestions`,
//! `common_prefix_chars`, and `near_search` are NOT pure logic — they build
//! response shapes and are not directly unit-tested — so they move here in
//! full, with `near_search` changed to return `ToolResult<Page<SymbolResult>>`
//! instead of `CallToolResult`.

use std::path::Path;

use code_graph_core::{paths, symbol_id, Symbol};
use code_graph_graph::{Graph, SearchParams};
use parking_lot::RwLock;

use crate::core::{require_indexed, ToolError, ToolOk, ToolResult};
use crate::handlers::symbols::{
    is_plain_identifier, levenshtein, max_distance_for_query, SearchSymbolsInput,
};
use crate::handlers::{
    byte_budget_take, kind_str, parse_kind, parse_language, suggest_symbols, symbol_to_result,
    EnclosingSymbol, Page, SearchSymbolsResponse, SummaryRow, SymbolResult,
    ENVELOPE_OVERHEAD_BYTES,
};

/// Re-exported so a second front-end (the CLI, Designs/CommandLineInterface
/// Decision 1) can construct the input without importing `handlers` — the
/// layer that carries the unguarded hardcoded-`indexed` adapters.
pub use crate::handlers::symbols::SearchSymbolsInput as SearchInput;

/// `get_file_symbols` body. Body moved verbatim from
/// `handlers::symbols::get_file_symbols`, plus the core `require_indexed`
/// call at entry (Decision 8). See the handler doc-comment (unchanged, and
/// authoritative) for the full behavioural contract — empty-raw-set error
/// wording, `count_only` sentinel shape, and default/clamp resolution.
#[allow(clippy::too_many_arguments)]
pub fn get_file_symbols(
    graph: &RwLock<Graph>,
    indexed: bool,
    file: &str,
    top_level_only: bool,
    brief: bool,
    limit: Option<u32>,
    offset: Option<u32>,
    count_only: bool,
    max_bytes: usize,
) -> ToolResult<Page<SymbolResult>> {
    require_indexed(indexed)?;

    if file.is_empty() {
        return Err(ToolError("'file' is required".to_string()));
    }

    let path = paths::normalize_user_path(file);
    let symbols = graph.read().file_symbols(&path);
    if symbols.is_empty() {
        return Err(ToolError(format!("no symbols found in file: {file}")));
    }

    if count_only {
        let total = if top_level_only {
            symbols.iter().filter(|s| s.parent.is_empty()).count() as u32
        } else {
            symbols.len() as u32
        };
        let response = Page::<SymbolResult> {
            results: vec![],
            total,
            offset: 0,
            limit: 0,
            truncated: false,
            next_offset: None,
        };
        return Ok(ToolOk::Value(response));
    }

    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(100).min(1000);
    let resolved_offset = offset.unwrap_or(0);

    let mut results: Vec<SymbolResult> = Vec::with_capacity(symbols.len());
    for s in &symbols {
        if top_level_only && !s.parent.is_empty() {
            continue;
        }
        results.push(symbol_to_result(s, brief));
    }

    let total = results.len() as u32;

    results.sort_by(|a, b| a.id.cmp(&b.id));

    let (records, _total_kept, truncated, next_offset) =
        byte_budget_take(results, resolved_offset, resolved_limit, max_bytes);

    let response = Page::<SymbolResult> {
        results: records,
        total,
        offset: resolved_offset,
        limit: resolved_limit,
        truncated,
        next_offset,
    };
    Ok(ToolOk::Value(response))
}

/// `get_symbol_at` body (phase 1). Body moved verbatim from
/// `handlers::symbols::get_symbol_at`, plus the core `require_indexed` call
/// at entry (Decision 8).
pub fn get_symbol_at(
    graph: &RwLock<Graph>,
    indexed: bool,
    file: &str,
    line: u32,
    limit: Option<u32>,
    offset: Option<u32>,
    max_bytes: usize,
) -> ToolResult<Page<EnclosingSymbol>> {
    require_indexed(indexed)?;

    if file.is_empty() {
        return Err(ToolError("'file' is required".to_string()));
    }
    if line == 0 {
        return Err(ToolError(
            "'line' must be >= 1 (lines are 1-based)".to_string(),
        ));
    }

    let path = paths::normalize_user_path(file);

    let g = graph.read();
    if !g.has_file(&path) {
        drop(g);
        return Err(ToolError(format!("file not found: {file:?}")));
    }
    let symbols = g.symbols_at_line(&path, line);
    drop(g);

    let total = symbols.len() as u32;

    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(100).min(1000);
    let resolved_offset = offset.unwrap_or(0);

    let results: Vec<EnclosingSymbol> = symbols
        .iter()
        .map(|s| EnclosingSymbol {
            symbol_id: symbol_id(s),
            name: s.name.clone(),
            kind: kind_str(s.kind).to_string(),
            line: s.line,
            end_line: s.end_line,
            span_lines: s.end_line.saturating_sub(s.line),
            parent: s.parent.clone(),
            namespace: s.namespace.clone(),
        })
        .collect();

    let (records, _total_kept, truncated, next_offset) =
        byte_budget_take(results, resolved_offset, resolved_limit, max_bytes);

    let response = Page::<EnclosingSymbol> {
        results: records,
        total,
        offset: resolved_offset,
        limit: resolved_limit,
        truncated,
        next_offset,
    };
    Ok(ToolOk::Value(response))
}

/// `search_symbols` body. Body moved verbatim from
/// `handlers::symbols::search_symbols`, plus the core `require_indexed`
/// call at entry (Decision 8). See the handler doc-comment (unchanged, and
/// authoritative) for the byte-budget-trim architectural exception this
/// function preserves.
pub fn search_symbols(
    graph: &RwLock<Graph>,
    indexed: bool,
    input: SearchSymbolsInput<'_>,
    max_bytes: usize,
) -> ToolResult<SearchSymbolsResponse> {
    require_indexed(indexed)?;

    let query_str = input.query.unwrap_or("");
    let kind_str_ref = input.kind.unwrap_or("");
    let namespace_str = input.namespace.unwrap_or("");
    let language_str = input.language.unwrap_or("");

    if query_str.is_empty()
        && kind_str_ref.is_empty()
        && namespace_str.is_empty()
        && language_str.is_empty()
    {
        return Err(ToolError(
            "'query', 'kind', 'namespace', or 'language' is required".to_string(),
        ));
    }

    let parsed_kind = if kind_str_ref.is_empty() {
        None
    } else {
        match parse_kind(kind_str_ref) {
            Some(k) => Some(k),
            None => return Err(ToolError(format!("invalid kind: {kind_str_ref}"))),
        }
    };

    let parsed_language = if language_str.is_empty() {
        None
    } else {
        match parse_language(language_str) {
            Some(l) => Some(l),
            None => return Err(ToolError(format!("invalid language: {language_str}"))),
        }
    };

    let resolved_limit = input.limit.filter(|&l| l > 0).unwrap_or(20).min(1000);
    let resolved_offset = input.offset.unwrap_or(0);
    let resolved_subtree: Option<std::path::PathBuf> = input
        .subtree
        .filter(|s| !s.is_empty())
        .map(code_graph_core::paths::normalize_user_path);

    if input.near && input.count_only {
        return Err(ToolError(
            "near mode is incompatible with count_only; drop one of them \
             (a fuzzy search has no bounded count short-circuit)"
                .to_string(),
        ));
    }

    if input.count_only {
        let sr = graph.read().search(SearchParams {
            pattern: query_str.to_string(),
            kind: parsed_kind,
            namespace: namespace_str.to_string(),
            language: parsed_language,
            limit: 0,
            offset: 0,
            count_only: true,
            subtree: resolved_subtree.clone(),
        });
        let response = Page::<SymbolResult> {
            results: vec![],
            total: sr.total,
            offset: 0,
            limit: 0,
            truncated: false,
            next_offset: None,
        };
        return Ok(ToolOk::Value(SearchSymbolsResponse {
            page: response,
            suggestions: Vec::new(),
        }));
    }

    if input.near {
        if query_str.is_empty() {
            return Err(ToolError(
                "near mode requires a non-empty 'query'".to_string(),
            ));
        }
        if !is_plain_identifier(query_str) {
            return Err(ToolError(
                "near mode requires a plain identifier query (no regex metacharacters); \
                 use the default mode if you need regex matching"
                    .to_string(),
            ));
        }
        let max_distance = input
            .max_distance
            .map(|d| (d as usize).min(8))
            .unwrap_or_else(|| max_distance_for_query(query_str.len()));
        let page = near_search(
            graph,
            query_str,
            max_distance,
            parsed_kind,
            parsed_language,
            namespace_str,
            resolved_subtree.as_deref(),
            resolved_limit,
            resolved_offset,
            input.brief,
            max_bytes,
        );
        return Ok(ToolOk::Value(SearchSymbolsResponse {
            page,
            suggestions: Vec::new(),
        }));
    }

    let sr = graph.read().search(SearchParams {
        pattern: query_str.to_string(),
        kind: parsed_kind,
        namespace: namespace_str.to_string(),
        language: parsed_language,
        limit: resolved_limit,
        offset: resolved_offset,
        count_only: false,
        subtree: resolved_subtree,
    });

    let page: Vec<SymbolResult> = sr
        .symbols
        .iter()
        .map(|s| symbol_to_result(s, input.brief))
        .collect();

    let budget = max_bytes.saturating_sub(ENVELOPE_OVERHEAD_BYTES);
    let mut results: Vec<SymbolResult> = Vec::with_capacity(page.len());
    let mut running_bytes: usize = 0;
    for record in page {
        let serialized_len = serde_json::to_string(&record).map(|s| s.len()).unwrap_or(0);
        let projected = running_bytes
            .saturating_add(serialized_len)
            .saturating_add(1);
        if projected > budget {
            break;
        }
        running_bytes = projected;
        results.push(record);
    }

    // `Graph::search` returns at most `resolved_limit` records, so unlike
    // `byte_budget_take` this handler cannot use iterator lookahead. Its
    // pre-pagination `total` supplies the equivalent evidence: if more
    // records exist after the emitted prefix, the page is truncated whether
    // that prefix stopped at the count limit or the byte budget.
    let emitted = results.len() as u32;
    let next = resolved_offset.saturating_add(emitted);
    let truncated = sr.total > next;
    let next_offset = truncated.then_some(next);

    let page = Page::<SymbolResult> {
        results,
        total: sr.total,
        offset: resolved_offset,
        limit: resolved_limit,
        truncated,
        next_offset,
    };

    let suggestions: Vec<String> = if page.total == 0
        && query_str.starts_with('^')
        && query_str.ends_with('$')
        && query_str.len() >= 2
    {
        let inner = &query_str[1..query_str.len() - 1];
        if inner.is_empty() {
            Vec::new()
        } else if is_plain_identifier(inner) {
            let near_hits = levenshtein_suggestions(&graph.read(), inner, 5);
            if !near_hits.is_empty() {
                near_hits
            } else {
                graph
                    .read()
                    .search_symbols(inner, None)
                    .iter()
                    .take(5)
                    .map(symbol_id)
                    .collect()
            }
        } else {
            graph
                .read()
                .search_symbols(inner, None)
                .iter()
                .take(5)
                .map(symbol_id)
                .collect()
        }
    } else {
        Vec::new()
    };

    Ok(ToolOk::Value(SearchSymbolsResponse { page, suggestions }))
}

/// Return up to `limit` symbols whose name is within a length-adaptive
/// Levenshtein distance of `inner`. Body moved verbatim from
/// `handlers::symbols::levenshtein_suggestions`.
fn levenshtein_suggestions(graph: &Graph, inner: &str, limit: usize) -> Vec<String> {
    let max_distance = max_distance_for_query(inner.len());
    let mut candidates: Vec<(usize, &Symbol)> = Vec::new();
    let inner_chars: Vec<char> = inner.chars().collect();
    let inner_char_len = inner_chars.len();
    for sym in graph.all_symbols() {
        let name_chars: Vec<char> = sym.name.chars().collect();
        if name_chars.len().abs_diff(inner_char_len) > max_distance {
            continue;
        }
        let d = levenshtein(&inner_chars, &name_chars, max_distance);
        if d <= max_distance {
            candidates.push((d, sym));
        }
    }
    candidates.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| {
                common_prefix_chars(&b.1.name, inner).cmp(&common_prefix_chars(&a.1.name, inner))
            })
            .then_with(|| {
                let a_shorter = a.1.name.chars().count() < inner_char_len;
                let b_shorter = b.1.name.chars().count() < inner_char_len;
                a_shorter.cmp(&b_shorter)
            })
            .then_with(|| {
                let a_diff = (a.1.name.chars().count() as i64 - inner_char_len as i64).abs();
                let b_diff = (b.1.name.chars().count() as i64 - inner_char_len as i64).abs();
                a_diff.cmp(&b_diff)
            })
            .then_with(|| a.1.name.cmp(&b.1.name))
    });
    candidates
        .into_iter()
        .take(limit)
        .map(|(_, s)| symbol_id(s))
        .collect()
}

/// Count leading characters `a` and `b` share (UTF-8-safe). Body moved
/// verbatim from `handlers::symbols::common_prefix_chars`.
fn common_prefix_chars(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

/// `search_symbols(near=true, …)` body. Body moved verbatim from
/// `handlers::symbols::near_search`, with the return type changed from
/// `CallToolResult` to the already-assembled `Page<SymbolResult>` (the
/// caller wraps it in `SearchSymbolsResponse`/`ToolOk::Value`).
#[allow(clippy::too_many_arguments)]
fn near_search(
    graph: &RwLock<Graph>,
    query: &str,
    max_distance: usize,
    kind: Option<code_graph_core::SymbolKind>,
    language: Option<code_graph_core::Language>,
    namespace: &str,
    subtree: Option<&std::path::Path>,
    resolved_limit: u32,
    resolved_offset: u32,
    brief: bool,
    max_bytes: usize,
) -> Page<SymbolResult> {
    let query_chars: Vec<char> = query.chars().collect();
    let lower_ns = namespace.to_lowercase();

    let g = graph.read();
    let mut hits: Vec<(usize, Symbol)> = Vec::new();
    for sym in g.all_symbols() {
        if let Some(k) = kind {
            if sym.kind != k {
                continue;
            }
        }
        if let Some(l) = language {
            if sym.language != l {
                continue;
            }
        }
        if !lower_ns.is_empty() && !sym.namespace.to_lowercase().contains(&lower_ns) {
            continue;
        }
        if let Some(prefix) = subtree {
            if !std::path::Path::new(&sym.file).starts_with(prefix) {
                continue;
            }
        }
        let name_chars: Vec<char> = sym.name.chars().collect();
        if name_chars.len().abs_diff(query_chars.len()) > max_distance {
            continue;
        }
        let d = levenshtein(&query_chars, &name_chars, max_distance);
        if d <= max_distance {
            hits.push((d, sym.clone()));
        }
    }
    drop(g);

    let total = hits.len() as u32;

    let query_char_len = query_chars.len();
    hits.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| {
                common_prefix_chars(&b.1.name, query).cmp(&common_prefix_chars(&a.1.name, query))
            })
            .then_with(|| {
                let a_shorter = a.1.name.chars().count() < query_char_len;
                let b_shorter = b.1.name.chars().count() < query_char_len;
                a_shorter.cmp(&b_shorter)
            })
            .then_with(|| {
                let a_diff = (a.1.name.chars().count() as i64 - query_char_len as i64).abs();
                let b_diff = (b.1.name.chars().count() as i64 - query_char_len as i64).abs();
                a_diff.cmp(&b_diff)
            })
            .then_with(|| a.1.name.cmp(&b.1.name))
    });

    let mapped: Vec<SymbolResult> = hits
        .into_iter()
        .map(|(_d, s)| symbol_to_result(&s, brief))
        .collect();
    let (results, _kept, truncated, next_offset) =
        byte_budget_take(mapped, resolved_offset, resolved_limit, max_bytes);

    Page::<SymbolResult> {
        results,
        total,
        offset: resolved_offset,
        limit: resolved_limit,
        truncated,
        next_offset,
    }
}

/// `get_symbol_detail` body. Body moved verbatim from
/// `handlers::symbols::get_symbol_detail`, plus the core `require_indexed`
/// call at entry (Decision 8).
pub fn get_symbol_detail(
    graph: &RwLock<Graph>,
    indexed: bool,
    symbol: &str,
) -> ToolResult<SymbolResult> {
    require_indexed(indexed)?;

    if symbol.is_empty() {
        return Err(ToolError("'symbol' is required".to_string()));
    }

    let g = graph.read();
    if let Some(s) = g.symbol_detail(symbol) {
        let result = symbol_to_result(&s, false);
        return Ok(ToolOk::Value(result));
    }

    let suggestions = suggest_symbols(&g, symbol, 5);
    drop(g);
    if suggestions.is_empty() {
        Err(ToolError(format!("symbol not found: {symbol:?}")))
    } else {
        Err(ToolError(format!(
            "symbol not found: {symbol:?}. Did you mean: {suggestions}?"
        )))
    }
}

/// `get_symbol_summary` body. Body moved verbatim from
/// `handlers::symbols::get_symbol_summary`, plus the core `require_indexed`
/// call at entry (Decision 8).
pub fn get_symbol_summary(
    graph: &RwLock<Graph>,
    indexed: bool,
    file: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
    count_only: bool,
    max_bytes: usize,
) -> ToolResult<Page<SummaryRow>> {
    require_indexed(indexed)?;

    let path: Option<&Path> = file.filter(|s| !s.is_empty()).map(Path::new);
    let summary = graph.read().symbol_summary(path);

    if count_only {
        let total: u32 = summary.values().map(|m| m.len()).sum::<usize>() as u32;
        let response = Page::<SummaryRow> {
            results: vec![],
            total,
            offset: 0,
            limit: 0,
            truncated: false,
            next_offset: None,
        };
        return Ok(ToolOk::Value(response));
    }

    let mut rows: Vec<SummaryRow> = Vec::new();
    for (ns, kinds) in summary {
        let display_ns = if ns.is_empty() {
            "<global>".to_string()
        } else {
            ns.clone()
        };
        for (k, count) in kinds {
            rows.push(SummaryRow {
                namespace: display_ns.clone(),
                kind: kind_str(k),
                count,
            });
        }
    }

    rows.sort_by(|a, b| (a.namespace.as_str(), a.kind).cmp(&(b.namespace.as_str(), b.kind)));

    let total = rows.len() as u32;

    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(100).min(1000);
    let resolved_offset = offset.unwrap_or(0);

    let (results, _total_kept, truncated, next_offset) =
        byte_budget_take(rows, resolved_offset, resolved_limit, max_bytes);

    let response = Page::<SummaryRow> {
        results,
        total,
        offset: resolved_offset,
        limit: resolved_limit,
        truncated,
        next_offset,
    };
    Ok(ToolOk::Value(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph_core::{FileGraph, Language, SymbolKind};

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

    fn locked(g: Graph) -> RwLock<Graph> {
        RwLock::new(g)
    }

    // --- AC-28: unindexed => Err(ToolError), discriminable without
    // serialization ------------------------------------------------------

    #[test]
    fn get_file_symbols_unindexed_returns_typed_error() {
        let g = locked(Graph::new());
        let err = match get_file_symbols(
            &g,
            false,
            "/a.cpp",
            false,
            true,
            None,
            None,
            false,
            usize::MAX,
        ) {
            Err(e) => e,
            Ok(_) => panic!("unindexed get_file_symbols must error"),
        };
        assert_eq!(err.0, "no codebase indexed — call analyze_codebase first");
    }

    #[test]
    fn get_symbol_at_unindexed_returns_typed_error() {
        let g = locked(Graph::new());
        let err = match get_symbol_at(&g, false, "/a.cpp", 1, None, None, usize::MAX) {
            Err(e) => e,
            Ok(_) => panic!("unindexed get_symbol_at must error"),
        };
        assert_eq!(err.0, "no codebase indexed — call analyze_codebase first");
    }

    #[test]
    fn search_symbols_unindexed_returns_typed_error() {
        let g = locked(Graph::new());
        let input = SearchSymbolsInput {
            query: Some("foo"),
            ..Default::default()
        };
        let err = match search_symbols(&g, false, input, usize::MAX) {
            Err(e) => e,
            Ok(_) => panic!("unindexed search_symbols must error"),
        };
        assert_eq!(err.0, "no codebase indexed — call analyze_codebase first");
    }

    #[test]
    fn get_symbol_detail_unindexed_returns_typed_error() {
        let g = locked(Graph::new());
        let err = match get_symbol_detail(&g, false, "/a.cpp:foo") {
            Err(e) => e,
            Ok(_) => panic!("unindexed get_symbol_detail must error"),
        };
        assert_eq!(err.0, "no codebase indexed — call analyze_codebase first");
    }

    #[test]
    fn get_symbol_summary_unindexed_returns_typed_error() {
        let g = locked(Graph::new());
        let err = match get_symbol_summary(&g, false, None, None, None, false, usize::MAX) {
            Err(e) => e,
            Ok(_) => panic!("unindexed get_symbol_summary must error"),
        };
        assert_eq!(err.0, "no codebase indexed — call analyze_codebase first");
    }

    // --- count_only sentinel shape survives -------------------------------

    #[test]
    fn get_file_symbols_count_only_sentinel_shape() {
        let g = locked(graph_with_n_file_symbols(5));
        let result = get_file_symbols(
            &g,
            true,
            "/big.cpp",
            false,
            true,
            None,
            None,
            true,
            usize::MAX,
        );
        match result {
            Ok(ToolOk::Value(page)) => {
                assert!(page.results.is_empty());
                assert_eq!(page.total, 5);
                assert_eq!(page.offset, 0);
                assert_eq!(page.limit, 0);
                assert!(!page.truncated);
                assert_eq!(page.next_offset, None);
            }
            Ok(ToolOk::Text(s)) => panic!("expected Ok(Value(_)), got Text({s})"),
            Err(e) => panic!("expected Ok(Value(_)), got Err({})", e.0),
        }
    }

    #[test]
    fn get_symbol_summary_count_only_sentinel_shape() {
        let g = locked(graph_with_n_file_symbols(5));
        let result = get_symbol_summary(&g, true, None, None, None, true, usize::MAX);
        match result {
            Ok(ToolOk::Value(page)) => {
                assert!(page.results.is_empty());
                assert!(page.total > 0);
                assert_eq!(page.offset, 0);
                assert_eq!(page.limit, 0);
                assert!(!page.truncated);
                assert_eq!(page.next_offset, None);
            }
            _ => panic!("expected Ok(Value(_))"),
        }
    }

    #[test]
    fn search_symbols_count_only_sentinel_shape() {
        let g = locked(graph_with_n_file_symbols(5));
        let input = SearchSymbolsInput {
            query: Some("func"),
            count_only: true,
            ..Default::default()
        };
        let result = search_symbols(&g, true, input, usize::MAX);
        match result {
            Ok(ToolOk::Value(response)) => {
                assert!(response.page.results.is_empty());
                assert_eq!(response.page.total, 5);
                assert_eq!(response.page.offset, 0);
                assert_eq!(response.page.limit, 0);
                assert!(!response.page.truncated);
                assert_eq!(response.page.next_offset, None);
                assert!(response.suggestions.is_empty());
            }
            _ => panic!("expected Ok(Value(_))"),
        }
    }

    // --- AC-29: byte-capped truncation + paging resume, as typed fields ---

    #[test]
    fn get_file_symbols_byte_capped_page_reports_typed_truncation_and_resumes() {
        let g = locked(graph_with_n_file_symbols(150));

        // A tight max_bytes forces the byte-budget path to cut the page
        // before `limit` (100) is reached.
        let max_bytes = ENVELOPE_OVERHEAD_BYTES + 400;
        let page1 = match get_file_symbols(
            &g, true, "/big.cpp", false, true, None, None, false, max_bytes,
        ) {
            Ok(ToolOk::Value(p)) => p,
            Ok(ToolOk::Text(s)) => panic!("expected Ok(Value(_)), got Text({s})"),
            Err(e) => panic!("expected Ok(Value(_)), got Err({})", e.0),
        };
        assert!(page1.truncated, "tight byte budget must truncate");
        let next = page1
            .next_offset
            .expect("truncated page must carry next_offset");
        assert!(
            (next as usize) > page1.results.len().saturating_sub(1),
            "next_offset must be strictly past the last emitted record"
        );

        // Re-call at next_offset with a generous budget: must resume with
        // no gap and no repetition against page1.
        let page2 = match get_file_symbols(
            &g,
            true,
            "/big.cpp",
            false,
            true,
            None,
            Some(next),
            false,
            usize::MAX,
        ) {
            Ok(ToolOk::Value(p)) => p,
            Ok(ToolOk::Text(s)) => panic!("expected Ok(Value(_)), got Text({s})"),
            Err(e) => panic!("expected Ok(Value(_)), got Err({})", e.0),
        };
        assert!(!page2.results.is_empty(), "resumed page must not be empty");

        let mut ids: Vec<String> = page1
            .results
            .iter()
            .chain(page2.results.iter())
            .map(|s| s.id.clone())
            .collect();
        let before = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), before, "no duplicate ids across the two pages");

        // Combined with everything strictly before `next` (there is none,
        // since offset started at 0) the two pages must not skip an id: the
        // union of both pages sorted must be a contiguous prefix of the
        // full 150-symbol id space starting at func_000.
        assert_eq!(ids.len(), page1.results.len() + page2.results.len());
    }
}
