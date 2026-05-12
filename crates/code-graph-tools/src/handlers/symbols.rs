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
    byte_budget_take, kind_str, parse_kind, parse_language, suggest_symbols, symbol_to_result,
    tool_error, tool_success_json, Page, SymbolResult, ENVELOPE_OVERHEAD_BYTES,
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
///
/// When `count_only = true` (Phase 3 of `PaginatedResponseSizeSafety`),
/// the handler returns the sentinel response shape `Page { results: [],
/// total, offset: 0, limit: 0, truncated: false, next_offset: None }`
/// after computing `total` via the cheap path (filter + count), without
/// ever materializing `SymbolResult`s or invoking the byte-budget helper.
/// The empty-raw-set check still runs first, so a misspelled file path
/// surfaces the diagnostic error wording even on a count_only call. See
/// plan Decision 9 for why `limit: 0` is a deliberate exception to the
/// "envelope echoes resolved limit" contract.
///
/// `#[allow(clippy::too_many_arguments)]`: the existing call-site
/// convention for `get_file_symbols` (and `get_orphans`) is positional
/// args. Phase 3.2 of `PaginatedResponseSizeSafety` adds `count_only` as
/// the 8th positional parameter to preserve that convention for the ~25
/// existing call sites (tests, watch handlers, integration). The
/// `GenerateDiagramInput` / `SearchSymbolsInput` struct pattern is used
/// elsewhere in this module where the arg count crossed the threshold
/// before any consumers existed; here, we accept the lint to avoid a
/// breaking refactor across every caller.
#[allow(clippy::too_many_arguments)]
pub fn get_file_symbols(
    graph: &RwLock<Graph>,
    file: &str,
    top_level_only: bool,
    brief: bool,
    limit: Option<u32>,
    offset: Option<u32>,
    count_only: bool,
    max_bytes: usize,
) -> CallToolResult {
    if file.is_empty() {
        return tool_error("'file' is required");
    }

    let symbols = graph.read().file_symbols(Path::new(file));
    // Raw-set-empty -> existing tool error. Wording preserved byte-for-byte
    // so agents that match against this string keep working. This branch
    // executes BEFORE the byte-budget step (PaginationOverhaul Phase 3
    // decision) so a misspelled file path always surfaces the diagnostic
    // error wording rather than an empty Page<T>.
    if symbols.is_empty() {
        return tool_error(format!("no symbols found in file: {file}"));
    }

    // Count-only short-circuit (Phase 3.2 of PaginatedResponseSizeSafety):
    // count the post-filter match set WITHOUT materializing SymbolResults or
    // invoking `byte_budget_take`. Order is load-bearing — must run AFTER
    // the empty-raw-set check (so misspelled files keep the diagnostic
    // error wording) but BEFORE the materialization step below.
    if count_only {
        let total = if top_level_only {
            symbols.iter().filter(|s| s.parent.is_empty()).count() as u32
        } else {
            symbols.len() as u32
        };
        // `limit: 0` is a deliberate exception to the
        // "envelope echoes resolved limit" contract — see plan Decision 9.
        // count_only callers opted out of paging; echoing a would-have-been
        // limit would mislead them into thinking there's a record page to
        // fetch. The exception is documented in CLAUDE.md alongside the
        // count_only sub-block (Phase 4.2).
        let response = Page::<SymbolResult> {
            results: vec![],
            total,
            offset: 0,
            limit: 0,
            truncated: false,
            next_offset: None,
        };
        return tool_success_json(&response);
    }

    // Resolve defaults: zero-or-missing limit -> 100; clamp at 1000.
    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(100).min(1000);
    let resolved_offset = offset.unwrap_or(0);

    // Apply `top_level_only` filter into a Vec<SymbolResult>. Build the
    // Vec eagerly so we can sort + slice + count it in subsequent steps.
    // The filter runs BEFORE `total` is captured so `total` reflects the
    // post-filter, pre-pagination match count.
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

    // Route through byte_budget_take (Phase 2 of PaginatedResponseSizeSafety):
    // the helper internally applies offset+limit skip/take and stops early
    // if the running serialized byte count would exceed
    // `max_bytes - ENVELOPE_OVERHEAD_BYTES`. `total_kept` from the helper is
    // `results.len() as u32`, NOT the pre-pagination match count — that's
    // `total` captured above and held unchanged.
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
    /// When `true`, the handler returns the sentinel envelope
    /// (`results: []`, `total: <real count>`, `offset: 0`, `limit: 0`,
    /// `truncated: false`, `next_offset: None`) without materializing
    /// `SymbolResult`s. Wired into the early-return path in Phase 3.2
    /// of `PaginatedResponseSizeSafety`; the Graph-layer heap short-
    /// circuit lands in task 3.3.
    pub count_only: bool,
}

/// `search_symbols` body. Validates that at least one filter was supplied,
/// parses string-typed filters into their typed forms, then delegates to
/// `Graph::search`. The pagination envelope is always present — `total`
/// is reported pre-pagination so callers can render "page X of Y" UIs.
///
/// **Architectural exception (Phase 2 of `PaginatedResponseSizeSafety`):**
/// unlike the four materializing handlers (orphans, file_symbols, callers,
/// callees) that operate on a full match set before pagination, this handler
/// receives an already-sliced page from `Graph::search` (the heap inside
/// `search` keeps only `offset + limit` records). The byte-budget trim is
/// applied at the handler layer here as a post-process on `sr.symbols`,
/// NOT via `byte_budget_take` (whose `offset`/`limit` semantics don't apply
/// to an already-paginated page).
///
/// Truncation distinction matters: `sr.symbols.len() < resolved_limit` is
/// normal end-of-match-set (Graph::search exhausted the underlying match
/// set at this offset); we report `truncated=false` in that case. Only set
/// `truncated=true` when the byte-budget trim DROPS records from the page
/// returned by `Graph::search`. `total` always carries `sr.total` (the
/// pre-pagination match count from `Graph::search`).
pub fn search_symbols(
    graph: &RwLock<Graph>,
    input: SearchSymbolsInput<'_>,
    max_bytes: usize,
) -> CallToolResult {
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

    // Count-only short-circuit (Phase 3.2 of PaginatedResponseSizeSafety):
    // delegate to `Graph::search` and emit the sentinel envelope using
    // `sr.total` (the pre-pagination match count is reported regardless of
    // limit/offset, so the count is correct even without a Graph-layer
    // short-circuit). Phase 3.3 will thread `count_only` into `SearchParams`
    // so the `BinaryHeap<TopEntry>` is never constructed; for 3.2 the heap
    // is still built but the resulting page is discarded. Acceptable interim
    // per the plan's task 3.2 spec.
    if input.count_only {
        // Pass `limit = 1` to minimize the heap's memory footprint during
        // this interim path — `sr.total` is independent of the limit, so
        // a small limit yields the correct count without paying the cost
        // of a full-page allocation. Cannot use `limit = 0`: `Graph::search`
        // normalizes 0 to the default 20 at queries.rs (the contract is
        // "0 means use the default"), so the heap would still allocate
        // 20 slots. `limit = 1` is the smallest value that bypasses that
        // normalization.
        let sr = graph.read().search(SearchParams {
            pattern: query_str.to_string(),
            kind: parsed_kind,
            namespace: namespace_str.to_string(),
            language: parsed_language,
            limit: 1,
            offset: 0,
        });
        // `limit: 0` is a deliberate exception to the
        // "envelope echoes resolved limit" contract — see plan Decision 9.
        // count_only callers opted out of paging; echoing a would-have-been
        // limit would mislead them into thinking there's a record page to
        // fetch. The exception is documented in CLAUDE.md alongside the
        // count_only sub-block (Phase 4.2).
        let response = Page::<SymbolResult> {
            results: vec![],
            total: sr.total,
            offset: 0,
            limit: 0,
            truncated: false,
            next_offset: None,
        };
        return tool_success_json(&response);
    }

    let sr = graph.read().search(SearchParams {
        pattern: query_str.to_string(),
        kind: parsed_kind,
        namespace: namespace_str.to_string(),
        language: parsed_language,
        limit: resolved_limit,
        offset: resolved_offset,
    });

    // `sr.symbols` is the already-sliced page (length <= resolved_limit).
    // Map to SymbolResult eagerly so we can size each record against the
    // byte budget.
    let page: Vec<SymbolResult> = sr
        .symbols
        .iter()
        .map(|s| symbol_to_result(s, input.brief))
        .collect();

    // Handler-layer trim: iterate the mapped page, accumulating
    // serialized-JSON byte counts (plus a +1 inter-record comma, mirroring
    // `byte_budget_take`'s accounting). Stop at the first record whose
    // admission would push the running total past
    // `max_bytes - ENVELOPE_OVERHEAD_BYTES`. The dropped record's index `k`
    // (within `page`) becomes the offset delta for `next_offset`.
    //
    // saturating_sub guards against pathological `max_bytes` smaller than
    // `ENVELOPE_OVERHEAD_BYTES` (including `max_bytes == 0`): budget
    // becomes 0 and the first record's projected total trips the cutoff.
    //
    // If we exhaust the page without the budget biting, `truncated` stays
    // `false` and `next_offset` stays `None` — including the case where
    // `Graph::search` returned a short page (end of match set). Only the
    // budget-driven trim sets `truncated=true`.
    let budget = max_bytes.saturating_sub(ENVELOPE_OVERHEAD_BYTES);
    let mut results: Vec<SymbolResult> = Vec::with_capacity(page.len());
    let mut running_bytes: usize = 0;
    let mut truncated = false;
    let mut next_offset: Option<u32> = None;

    for record in page {
        // Production records (SymbolResult) hold only plain owned data and
        // never fail to serialize; the unwrap_or(0) fallback mirrors
        // `byte_budget_take` and only exists to satisfy the Serialize bound.
        let serialized_len = serde_json::to_string(&record).map(|s| s.len()).unwrap_or(0);
        // +1 covers the inter-record comma. Over-counts by 1 on the first
        // record — intentional headroom, same as `byte_budget_take`.
        let projected = running_bytes
            .saturating_add(serialized_len)
            .saturating_add(1);
        if projected > budget {
            // Budget bites: this record is the first one DROPPED. `k` is the
            // count of records ALREADY kept; `resolved_offset + k` is where
            // the next call should re-page from to pick up this dropped
            // record as its first entry — no overlap, no gap at the trim
            // boundary.
            let k = results.len() as u32;
            truncated = true;
            next_offset = Some(resolved_offset.saturating_add(k));
            break;
        }
        running_bytes = projected;
        results.push(record);
    }

    let response = Page::<SymbolResult> {
        results,
        total: sr.total,
        offset: resolved_offset,
        limit: resolved_limit,
        truncated,
        next_offset,
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
    use super::super::NO_BYTE_BUDGET;
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
        let r = get_file_symbols(&g, "", false, true, None, None, false, NO_BYTE_BUDGET);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'file' is required");
    }

    #[test]
    fn file_symbols_unknown_file_errors() {
        let g = locked(Graph::new());
        let r = get_file_symbols(
            &g,
            "/missing.cpp",
            false,
            true,
            None,
            None,
            false,
            NO_BYTE_BUDGET,
        );
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
        let r = get_file_symbols(
            &g,
            "/missing.cpp",
            false,
            true,
            Some(50),
            Some(10),
            false,
            NO_BYTE_BUDGET,
        );
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
        let r = get_file_symbols(
            &g,
            "/methods_only.cpp",
            true,
            true,
            None,
            None,
            false,
            NO_BYTE_BUDGET,
        );
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
        let r = get_file_symbols(&g, "/a.cpp", false, true, None, None, false, NO_BYTE_BUDGET);
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
        let r = get_file_symbols(&g, "/a.cpp", true, true, None, None, false, NO_BYTE_BUDGET);
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
        let r = get_file_symbols(
            &g,
            "/a.cpp",
            false,
            false,
            None,
            None,
            false,
            NO_BYTE_BUDGET,
        );
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
        let r = get_file_symbols(
            &g,
            "/big.cpp",
            false,
            true,
            None,
            None,
            false,
            NO_BYTE_BUDGET,
        );
        let (arr, total, offset, limit) = page_parts(&r);
        assert_eq!(arr.len(), 100);
        assert_eq!(total, 120);
        assert_eq!(offset, 0);
        assert_eq!(limit, 100);
    }

    #[test]
    fn file_symbols_page_1_and_page_2_cover_full_set() {
        let g = locked(graph_with_n_file_symbols(150));
        let p1 = get_file_symbols(
            &g,
            "/big.cpp",
            false,
            true,
            Some(100),
            Some(0),
            false,
            NO_BYTE_BUDGET,
        );
        let p2 = get_file_symbols(
            &g,
            "/big.cpp",
            false,
            true,
            Some(100),
            Some(100),
            false,
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
        let r1 = get_file_symbols(
            &g,
            "/big.cpp",
            false,
            true,
            Some(50),
            Some(0),
            false,
            NO_BYTE_BUDGET,
        );
        let r2 = get_file_symbols(
            &g,
            "/big.cpp",
            false,
            true,
            Some(50),
            Some(50),
            false,
            NO_BYTE_BUDGET,
        );
        let r3 = get_file_symbols(
            &g,
            "/big.cpp",
            false,
            true,
            Some(10),
            Some(140),
            false,
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
    fn file_symbols_limit_clamps_at_1000() {
        // 5-symbol fixture with limit = 999_999 silently clamps to 1000;
        // the response echoes the clamped value. The 5-row count also
        // verifies take(1000) doesn't drop entries on a small set.
        let g = locked(graph_with_n_file_symbols(5));
        let r = get_file_symbols(
            &g,
            "/big.cpp",
            false,
            true,
            Some(999_999),
            None,
            false,
            NO_BYTE_BUDGET,
        );
        let (arr, _, _, limit) = page_parts(&r);
        assert_eq!(limit, 1000);
        assert_eq!(arr.len(), 5);
    }

    #[test]
    fn file_symbols_zero_limit_uses_default() {
        let g = locked(graph_with_n_file_symbols(5));
        let r = get_file_symbols(
            &g,
            "/big.cpp",
            false,
            true,
            Some(0),
            None,
            false,
            NO_BYTE_BUDGET,
        );
        let (_, _, _, limit) = page_parts(&r);
        assert_eq!(limit, 100);
    }

    #[test]
    fn file_symbols_offset_beyond_total_returns_empty() {
        let g = locked(graph_with_n_file_symbols(5));
        let r = get_file_symbols(
            &g,
            "/big.cpp",
            false,
            true,
            None,
            Some(999),
            false,
            NO_BYTE_BUDGET,
        );
        let (arr, total, offset, limit) = page_parts(&r);
        assert!(arr.is_empty());
        assert_eq!(total, 5);
        assert_eq!(offset, 999);
        assert_eq!(limit, 100);
    }

    // --- Phase 2 byte-budget invariants -----------------------------------

    #[test]
    fn file_symbols_byte_budget_truncates_oversized_page() {
        // Phase 2 of PaginatedResponseSizeSafety: a tight `max_bytes` must
        // make `get_file_symbols` stop emitting records before reaching
        // `limit`, surface `truncated=true`, and report a usable `next_offset`.
        //
        // Fixture: 30 free functions named `func_000`..`func_029` in
        // `/big.cpp`. Each serialized SymbolResult in brief mode is ~80-90
        // bytes (`{"id":"/big.cpp:func_NNN","name":"func_NNN","kind":
        // "function","file":"/big.cpp","line":1}` plus the helper's +1
        // inter-record comma).
        //
        // Pick `max_bytes = ENVELOPE_OVERHEAD_BYTES + 300`: budget after
        // overhead reservation is 300 bytes, fits a handful of records
        // before the next would push past. Asks for `limit=20` so the byte
        // budget (not the count cap) is what bites. Asserts documented
        // truncation semantics: `truncated=true`, `next_offset=Some(n)`
        // with `n > offset=0`, `results.len() < limit=20`, and
        // `total >= results.len() + offset`.
        use super::super::ENVELOPE_OVERHEAD_BYTES;
        let g = locked(graph_with_n_file_symbols(30));
        let max_bytes = ENVELOPE_OVERHEAD_BYTES + 300;
        let r = get_file_symbols(
            &g,
            "/big.cpp",
            false,
            true,
            Some(20),
            Some(0),
            false,
            max_bytes,
        );

        let (arr, total, offset, limit) = page_parts(&r);
        let (truncated, next_offset) = super::super::test_helpers::page_extras(&r);

        assert!(truncated, "tight max_bytes must produce truncated=true");
        assert!(
            (arr.len() as u32) < limit,
            "truncation must stop before hitting the count cap: results.len()={} >= limit={}",
            arr.len(),
            limit,
        );
        assert!(
            !arr.is_empty(),
            "budget should still admit at least one record",
        );
        match next_offset {
            Some(n) => assert!(
                n > offset,
                "next_offset must point past the current page: next_offset={n} <= offset={offset}",
            ),
            None => panic!("truncated=true must set next_offset=Some(n)"),
        }
        assert!(
            total >= arr.len() as u32 + offset,
            "total must be at least the records seen so far: total={total} < results.len()+offset={}",
            arr.len() as u32 + offset,
        );
        // Sanity: total still reflects the full pre-pagination match count.
        assert_eq!(total, 30, "total is the pre-pagination match count");
    }

    #[test]
    fn file_symbols_byte_budget_no_truncation_with_no_budget() {
        // Anti-regression: with NO_BYTE_BUDGET (= usize::MAX), the handler's
        // existing behavior is preserved exactly — no truncation, no
        // next_offset. Locks the contract that byte-budget wiring doesn't
        // affect callers that opt out.
        let g = locked(graph_with_n_file_symbols(30));
        let r = get_file_symbols(
            &g,
            "/big.cpp",
            false,
            true,
            Some(20),
            Some(0),
            false,
            NO_BYTE_BUDGET,
        );
        let (arr, total, _, _) = page_parts(&r);
        let (truncated, next_offset) = super::super::test_helpers::page_extras(&r);
        assert_eq!(arr.len(), 20);
        assert_eq!(total, 30);
        assert!(!truncated);
        assert_eq!(next_offset, None);
    }

    #[test]
    fn file_symbols_byte_budget_does_not_change_empty_raw_set_error() {
        // CRITICAL invariant per PaginationOverhaul Phase 3: empty raw set
        // returns the documented error envelope (NOT a Page<T>) regardless
        // of `max_bytes`. The byte-budget step runs after the empty-raw-set
        // check, so a tight budget cannot mask the diagnostic error.
        let g = locked(Graph::new());
        // Even with a pathologically tight budget that would normally
        // truncate everything, the empty-raw-set branch is preserved.
        let r = get_file_symbols(&g, "/missing.cpp", false, true, None, None, false, 0);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "no symbols found in file: /missing.cpp");
    }

    // --- Phase 3 count_only invariants ------------------------------------

    #[test]
    fn file_symbols_count_only_returns_sentinel_envelope_under_1kb() {
        // Phase 3.2 of PaginatedResponseSizeSafety: when count_only=true, the
        // handler emits Page { results: [], total: <real count>, offset: 0,
        // limit: 0, truncated: false, next_offset: None } regardless of how
        // many records WOULD have been returned. Serialized envelope size
        // must stay < 1KB even at the 1000-symbol scale.
        //
        // Asserts: (a) results is empty, (b) total reflects the true match
        // count (not zero), (c) limit=0 (deliberate exception to the
        // "envelope echoes resolved limit" contract per plan Decision 9),
        // (d) truncated=false and next_offset is None, (e) serialized body
        // is well under 1024 bytes regardless of input scale.
        let g = locked(graph_with_n_file_symbols(1000));
        let r = get_file_symbols(
            &g,
            "/big.cpp",
            false,
            true,
            None,
            None,
            true,
            NO_BYTE_BUDGET,
        );

        let body = body_text(&r);
        assert!(
            body.len() < 1024,
            "count_only response must be < 1KB; got {} bytes",
            body.len(),
        );

        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let results = parsed["results"].as_array().unwrap();
        let total = parsed["total"].as_u64().unwrap();
        let offset = parsed["offset"].as_u64().unwrap();
        let limit = parsed["limit"].as_u64().unwrap();
        let truncated = parsed["truncated"].as_bool().unwrap();
        let next_offset = parsed["next_offset"].clone();

        assert!(results.is_empty(), "count_only must emit empty results");
        assert_eq!(total, 1000, "total must reflect true match count");
        assert_eq!(offset, 0, "count_only emits offset=0");
        assert_eq!(
            limit, 0,
            "count_only emits limit=0 (Decision 9 exception to envelope echo rule)"
        );
        assert!(!truncated, "count_only must never set truncated=true");
        assert_eq!(
            next_offset,
            serde_json::Value::Null,
            "count_only must emit next_offset=null"
        );
    }

    #[test]
    fn file_symbols_count_only_respects_top_level_only_filter() {
        // The count_only short-circuit must apply the same top_level_only
        // filter as the materializing path. small_graph() has 3 symbols
        // (foo, Bar, Bar::do_thing); top_level_only=true filters out the
        // method, so total drops from 3 to 2.
        let g = locked(small_graph());

        // top_level_only=false => 3 symbols.
        let r = get_file_symbols(&g, "/a.cpp", false, true, None, None, true, NO_BYTE_BUDGET);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(parsed["total"].as_u64().unwrap(), 3);
        assert!(parsed["results"].as_array().unwrap().is_empty());

        // top_level_only=true => 2 symbols (foo, Bar; Bar::do_thing has parent=Bar).
        let r = get_file_symbols(&g, "/a.cpp", true, true, None, None, true, NO_BYTE_BUDGET);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(parsed["total"].as_u64().unwrap(), 2);
        assert!(parsed["results"].as_array().unwrap().is_empty());
    }

    #[test]
    fn file_symbols_count_only_empty_raw_set_still_errors() {
        // The count_only check runs AFTER the empty-raw-set diagnostic, so
        // a misspelled file path STILL surfaces the canonical
        // "no symbols found in file: <file>" tool error rather than a
        // Page<T> with total=0. This preserves the diagnostic-UX guard.
        let g = locked(Graph::new());
        let r = get_file_symbols(
            &g,
            "/missing.cpp",
            false,
            true,
            None,
            None,
            true,
            NO_BYTE_BUDGET,
        );
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "no symbols found in file: /missing.cpp");
    }

    #[test]
    fn file_symbols_count_only_empty_file_param_still_errors() {
        // The count_only check runs AFTER required-arg validation; empty
        // file param still surfaces the canonical "'file' is required"
        // tool error.
        let g = locked(Graph::new());
        let r = get_file_symbols(&g, "", false, true, None, None, true, NO_BYTE_BUDGET);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'file' is required");
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
        let r = search_symbols(&g, search_input(), NO_BYTE_BUDGET);
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
        let r = search_symbols(&g, search_input(), NO_BYTE_BUDGET);
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
            NO_BYTE_BUDGET,
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
            NO_BYTE_BUDGET,
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
            NO_BYTE_BUDGET,
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
            NO_BYTE_BUDGET,
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
            NO_BYTE_BUDGET,
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
            NO_BYTE_BUDGET,
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
            NO_BYTE_BUDGET,
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        // All three sample symbols are Cpp.
        assert_eq!(parsed["total"], serde_json::json!(3));
    }

    // --- search_symbols byte-budget invariants ----------------------------

    /// Build a single-file graph with `n` free functions named
    /// `match_NNN` for `n < 1000`. Zero-padding keeps the natural
    /// `symbol_id` sort order predictable: ids are
    /// `"/broad.cpp:match_000".."/broad.cpp:match_NNN"`. All symbols pass a
    /// `query="match"` filter, so `Graph::search` returns up to
    /// `offset+limit` of them — perfect for exercising the handler-layer
    /// trim against an already-sliced page.
    fn graph_with_n_broad_matches(n: usize) -> Graph {
        let mut g = Graph::new();
        let mut symbols: Vec<Symbol> = Vec::with_capacity(n);
        for i in 0..n {
            symbols.push(sym(
                &format!("match_{i:03}"),
                SymbolKind::Function,
                "/broad.cpp",
                "",
            ));
        }
        g.merge_file_graph(FileGraph {
            path: "/broad.cpp".to_string(),
            language: Language::Cpp,
            symbols,
            edges: Vec::new(),
        });
        g
    }

    #[test]
    fn search_symbols_byte_budget_truncates_oversized_page() {
        // Phase 2.5 of PaginatedResponseSizeSafety: a tight `max_bytes`
        // must make the handler trim its already-sliced page from
        // `Graph::search` and surface `truncated=true` with a usable
        // `next_offset`. Architectural distinction from the other four
        // paginated handlers: search_symbols receives a page that's
        // already <= resolved_limit records long, so the trim happens at
        // the handler layer (NOT via `byte_budget_take`).
        //
        // 100 symbols total in the graph — `query="match"` matches all of
        // them, so `sr.total` is 100 (pre-pagination match count from
        // `Graph::search`). Ask for limit=50; with a tight budget only a
        // handful of records fit before the budget bites.
        use super::super::ENVELOPE_OVERHEAD_BYTES;
        let g = locked(graph_with_n_broad_matches(100));
        let max_bytes = ENVELOPE_OVERHEAD_BYTES + 400;
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("match"),
                limit: Some(50),
                offset: Some(0),
                ..search_input()
            },
            max_bytes,
        );

        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let truncated = parsed["truncated"].as_bool().expect("truncated field");
        let next_offset = parsed["next_offset"].as_u64();
        let total = parsed["total"].as_u64().unwrap();
        let results = parsed["results"].as_array().unwrap();
        let limit = parsed["limit"].as_u64().unwrap();

        assert!(truncated, "tight max_bytes must produce truncated=true");
        let k = next_offset.expect("truncated=true must set next_offset=Some(n)");
        assert!(
            k < limit,
            "next_offset must point before the count cap: next_offset={k} >= limit={limit}",
        );
        assert!(
            (results.len() as u64) < limit,
            "trim must stop before hitting the count cap: results.len()={} >= limit={limit}",
            results.len(),
        );
        assert!(
            !results.is_empty(),
            "budget should still admit at least one record",
        );
        // `total` is the pre-pagination match count from `Graph::search`,
        // NOT the page size. 100 symbols all match `query="match"`.
        assert_eq!(
            total, 100,
            "total is the pre-pagination match count, not results.len()",
        );
    }

    #[test]
    fn search_symbols_byte_budget_records_not_corrupted_by_trim() {
        // Pin: the handler-layer trim drops complete records, never half-
        // serialized ones. Take the last KEPT record's `id` and verify it
        // round-trips through `id_to_file` to a non-empty file path —
        // proves the id field is intact and the JSON envelope didn't slice
        // a record mid-string.
        use super::super::ENVELOPE_OVERHEAD_BYTES;
        let g = locked(graph_with_n_broad_matches(100));
        let max_bytes = ENVELOPE_OVERHEAD_BYTES + 400;
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("match"),
                limit: Some(50),
                offset: Some(0),
                ..search_input()
            },
            max_bytes,
        );

        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let results = parsed["results"].as_array().unwrap();
        assert!(
            !results.is_empty(),
            "fixture sized so at least one record fits the budget",
        );
        let last = results.last().unwrap();
        let id = last["id"].as_str().expect("last record has a string id");
        let file = code_graph_core::id_to_file(id);
        assert!(
            !file.is_empty(),
            "id_to_file must resolve to a non-empty path; id={id} truncated mid-serialization?",
        );
        assert_eq!(
            file, "/broad.cpp",
            "id must round-trip back to the fixture's file path",
        );
    }

    #[test]
    fn search_symbols_byte_budget_re_paging_correctness() {
        // CRITICAL: re-paging from `next_offset` must produce exactly the
        // (k+1)-th record from the first call's underlying sorted match
        // set (the record AT index `k` that the trim DROPPED). No overlap,
        // no gap at the trim boundary.
        //
        // Strategy: call once with a tight budget that truncates at index
        // k; record `next_offset = Some(resolved_offset + k)`. Call again
        // with `offset = next_offset` and NO_BYTE_BUDGET (so the full page
        // returns). The second call's first record's `id` must equal the
        // id at sorted-position `k` in the full match set — easy to
        // compute independently because `match_NNN` ids sort lexically.
        use super::super::ENVELOPE_OVERHEAD_BYTES;
        let g = locked(graph_with_n_broad_matches(100));
        let max_bytes = ENVELOPE_OVERHEAD_BYTES + 400;

        // First call: tight budget, expect truncation.
        let r1 = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("match"),
                limit: Some(50),
                offset: Some(0),
                ..search_input()
            },
            max_bytes,
        );
        let p1: serde_json::Value = serde_json::from_str(&body_text(&r1)).unwrap();
        let truncated1 = p1["truncated"].as_bool().unwrap();
        let next_offset1 = p1["next_offset"]
            .as_u64()
            .expect("first call must truncate") as u32;
        let results1 = p1["results"].as_array().unwrap();
        assert!(truncated1, "first call must trip the budget");

        // Independent oracle: ids sort lexically. The (k+1)-th record's id
        // is `/broad.cpp:match_<next_offset1>` (zero-padded to 3 digits).
        let expected_first_id_of_page_2 = format!("/broad.cpp:match_{next_offset1:03}");

        // Second call: re-page from `next_offset` with no budget cap. The
        // first record of the returned page is the one the trim DROPPED.
        let r2 = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("match"),
                limit: Some(50),
                offset: Some(next_offset1),
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        let p2: serde_json::Value = serde_json::from_str(&body_text(&r2)).unwrap();
        let results2 = p2["results"].as_array().unwrap();
        assert!(
            !results2.is_empty(),
            "second call must return at least the dropped record",
        );
        let first_id_of_page_2 = results2[0]["id"].as_str().unwrap();
        assert_eq!(
            first_id_of_page_2, expected_first_id_of_page_2,
            "re-paging boundary mismatch: first record of page 2 must equal the (k+1)-th record from page 1's underlying sorted set",
        );

        // Anti-overlap: the last id of page 1 must NOT equal the first id
        // of page 2 (no duplicate record at the trim boundary).
        let last_id_of_page_1 = results1.last().expect("page 1 has at least one record")["id"]
            .as_str()
            .unwrap();
        assert_ne!(
            last_id_of_page_1, first_id_of_page_2,
            "trim must not leave overlap at the boundary",
        );

        // Anti-gap: ids are lexically dense (`match_000`..`match_099`), so
        // the first record of page 2 must be the immediate successor of
        // page 1's last record.
        let last_seq: u32 = last_id_of_page_1
            .strip_prefix("/broad.cpp:match_")
            .and_then(|s| s.parse().ok())
            .expect("page 1 last id must end with a 3-digit suffix");
        let first_seq: u32 = first_id_of_page_2
            .strip_prefix("/broad.cpp:match_")
            .and_then(|s| s.parse().ok())
            .expect("page 2 first id must end with a 3-digit suffix");
        assert_eq!(
            first_seq,
            last_seq + 1,
            "trim must leave no gap at the boundary: page 1 ends at seq {last_seq}, page 2 starts at seq {first_seq}",
        );
    }

    #[test]
    fn search_symbols_byte_budget_no_truncation_with_no_budget() {
        // Anti-regression: with NO_BYTE_BUDGET (= usize::MAX) the handler
        // returns the full page from `Graph::search` unchanged —
        // `truncated=false`, `next_offset=None`. Locks the contract that
        // byte-budget wiring doesn't affect callers that opt out.
        let g = locked(graph_with_n_broad_matches(100));
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("match"),
                limit: Some(50),
                offset: Some(0),
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let results = parsed["results"].as_array().unwrap();
        let truncated = parsed["truncated"].as_bool().unwrap();
        let next_offset = parsed["next_offset"].clone();
        let total = parsed["total"].as_u64().unwrap();

        assert_eq!(
            results.len(),
            50,
            "NO_BYTE_BUDGET must return the full page from Graph::search",
        );
        assert!(!truncated, "NO_BYTE_BUDGET must never set truncated=true");
        assert_eq!(
            next_offset,
            serde_json::Value::Null,
            "NO_BYTE_BUDGET must set next_offset=null",
        );
        assert_eq!(total, 100, "total is the pre-pagination match count");
    }

    #[test]
    fn search_symbols_short_page_not_marked_truncated() {
        // Architectural pin: when `Graph::search` returns a short page
        // (the underlying match set is exhausted at this offset),
        // `truncated` MUST stay `false` even with a small budget — as
        // long as the budget is large enough to fit every record on the
        // returned page. A short page is end-of-match-set, NOT a
        // budget-driven trim.
        //
        // Fixture: 5 matches total. Ask for limit=20. Graph::search
        // returns all 5 (page shorter than limit). With generous budget,
        // the trim loop exhausts the page without biting — truncated must
        // stay false, next_offset must stay None.
        let g = locked(graph_with_n_broad_matches(5));
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("match"),
                limit: Some(20),
                offset: Some(0),
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let results = parsed["results"].as_array().unwrap();
        let truncated = parsed["truncated"].as_bool().unwrap();
        let next_offset = parsed["next_offset"].clone();
        assert_eq!(results.len(), 5);
        assert!(
            !truncated,
            "short page (end of match set) must NOT be marked truncated",
        );
        assert_eq!(next_offset, serde_json::Value::Null);
    }

    #[test]
    fn search_symbols_short_page_under_tight_but_sufficient_budget() {
        // Companion to `search_symbols_short_page_not_marked_truncated`:
        // exercises the interesting case — a short page (5 matches, limit=20)
        // with a finite-but-sufficient budget. The trim loop must exhaust
        // the page without biting; truncated stays false. Pins that the
        // short-page detection isn't accidentally inverted under a real
        // (non-infinite) budget.
        use super::super::ENVELOPE_OVERHEAD_BYTES;
        let g = locked(graph_with_n_broad_matches(5));
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("match"),
                limit: Some(20),
                offset: Some(0),
                ..search_input()
            },
            ENVELOPE_OVERHEAD_BYTES + 2000,
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let results = parsed["results"].as_array().unwrap();
        let truncated = parsed["truncated"].as_bool().unwrap();
        let next_offset = parsed["next_offset"].clone();
        assert_eq!(results.len(), 5);
        assert!(
            !truncated,
            "short page under sufficient budget must NOT be marked truncated",
        );
        assert_eq!(next_offset, serde_json::Value::Null);
    }

    // --- Phase 3 search_symbols count_only invariants ---------------------

    #[test]
    fn search_symbols_count_only_returns_sentinel_envelope_under_1kb() {
        // Phase 3.2 of PaginatedResponseSizeSafety: when count_only=true, the
        // handler emits Page { results: [], total: <real count>, offset: 0,
        // limit: 0, truncated: false, next_offset: None } regardless of how
        // many records WOULD have been returned. Serialized envelope must
        // stay < 1KB even at the 1000-match scale.
        //
        // Asserts: (a) results is empty, (b) total reflects the true
        // pre-pagination match count from Graph::search (not zero),
        // (c) limit=0 (deliberate exception to the "envelope echoes
        // resolved limit" contract per plan Decision 9),
        // (d) truncated=false and next_offset is None, (e) serialized body
        // is well under 1024 bytes regardless of input scale.
        //
        // Note: this task (3.2) accepts the interim cost of building the
        // BinaryHeap inside Graph::search; task 3.3 will thread count_only
        // into SearchParams so the heap is never constructed.
        let g = locked(graph_with_n_broad_matches(1000));
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("match"),
                count_only: true,
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );

        let body = body_text(&r);
        assert!(
            body.len() < 1024,
            "count_only response must be < 1KB; got {} bytes",
            body.len(),
        );

        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        let results = parsed["results"].as_array().unwrap();
        let total = parsed["total"].as_u64().unwrap();
        let offset = parsed["offset"].as_u64().unwrap();
        let limit = parsed["limit"].as_u64().unwrap();
        let truncated = parsed["truncated"].as_bool().unwrap();
        let next_offset = parsed["next_offset"].clone();

        assert!(results.is_empty(), "count_only must emit empty results");
        assert_eq!(total, 1000, "total must reflect true match count");
        assert_eq!(offset, 0, "count_only emits offset=0");
        assert_eq!(
            limit, 0,
            "count_only emits limit=0 (Decision 9 exception to envelope echo rule)"
        );
        assert!(!truncated, "count_only must never set truncated=true");
        assert_eq!(
            next_offset,
            serde_json::Value::Null,
            "count_only must emit next_offset=null"
        );
    }

    #[test]
    fn search_symbols_count_only_total_matches_full_search_total() {
        // count_only must report the same `total` as a regular call —
        // i.e., the pre-pagination match count from Graph::search is
        // independent of count_only. Companion to the 3.3 behavioral test
        // (same query with count_only=false vs true returns equal total).
        let g = locked(graph_with_n_broad_matches(50));

        let r_count = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("match"),
                count_only: true,
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r_count)).unwrap();
        let total_count_only = parsed["total"].as_u64().unwrap();

        let r_full = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("match"),
                limit: Some(1),
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r_full)).unwrap();
        let total_full = parsed["total"].as_u64().unwrap();

        assert_eq!(
            total_count_only, total_full,
            "count_only must report the same total as a regular search",
        );
        assert_eq!(total_count_only, 50);
    }

    #[test]
    fn search_symbols_count_only_still_validates_filters() {
        // The count_only check runs AFTER filter validation; "at least one
        // filter required" and "invalid kind" errors still surface.
        let g = locked(small_graph());

        // No filter supplied -> validation error even with count_only=true.
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                count_only: true,
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            body_text(&r),
            "'query', 'kind', 'namespace', or 'language' is required"
        );

        // Bad kind -> "invalid kind: widget" even with count_only=true.
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                kind: Some("widget"),
                count_only: true,
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "invalid kind: widget");
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
