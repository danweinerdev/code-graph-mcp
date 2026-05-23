//! Symbol-query handlers: `get_file_symbols`, `search_symbols`,
//! `get_symbol_detail`, `get_symbol_summary`.
//!
//! Each function takes the shared graph (read-locked by the caller) and
//! returns a `CallToolResult` ready to hand back to rmcp. Error wording
//! matches the Go reference at `internal/tools/symbols.go` byte-for-byte.

use std::path::Path;

use code_graph_core::{paths, symbol_id, Symbol};
use code_graph_graph::{Graph, SearchParams};
use parking_lot::RwLock;
use rmcp::model::CallToolResult;

use super::{
    byte_budget_take, kind_str, parse_kind, parse_language, suggest_symbols, symbol_to_result,
    tool_error, tool_success_json, Page, SearchSymbolsResponse, SummaryRow, SymbolResult,
    ENVELOPE_OVERHEAD_BYTES,
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
/// When `count_only = true`, the handler returns the sentinel response
/// shape `Page { results: [], total, offset: 0, limit: 0,
/// truncated: false, next_offset: None }` after computing `total` via the
/// cheap path (filter + count), without ever materializing
/// `SymbolResult`s or invoking the byte-budget helper. The empty-raw-set
/// check still runs first, so a misspelled file path surfaces the
/// diagnostic error wording even on a count_only call. `limit: 0` is a
/// deliberate exception to the "envelope echoes resolved limit" contract
/// (documented in CLAUDE.md).
///
/// `#[allow(clippy::too_many_arguments)]`: the existing call-site
/// convention for `get_file_symbols` (and `get_orphans`) is positional
/// args. `count_only` is the 8th positional parameter, preserving that
/// convention for the ~25 existing call sites (tests, watch handlers,
/// integration). The
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

    // Normalize the user-supplied file argument before graph lookup.
    // `normalize_user_path` returns the canonical form
    // when the path exists on disk (resolving `.` / `..` and stripping the
    // Windows `\\?\` extended-path prefix when the short form is valid) and
    // falls back to a lexical strip otherwise. On Linux with an already-
    // canonical path this is effectively identity, so existing call sites
    // (and snapshot tests) are byte-identical.
    let path = paths::normalize_user_path(file);
    let symbols = graph.read().file_symbols(&path);
    // Raw-set-empty -> existing tool error. Wording preserved byte-for-byte
    // so agents that match against this string keep working. This branch
    // executes BEFORE the byte-budget step so a misspelled file path
    // always surfaces the diagnostic error wording rather than an
    // empty Page<T>.
    if symbols.is_empty() {
        return tool_error(format!("no symbols found in file: {file}"));
    }

    // Count-only short-circuit: count the post-filter match set WITHOUT
    // materializing SymbolResults or
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
        // "envelope echoes resolved limit" contract. count_only callers
        // opted out of paging; echoing a would-have-been limit would
        // mislead them into thinking there's a record page to fetch. The
        // exception is documented in CLAUDE.md alongside the count_only
        // sub-block.
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

    // Route through byte_budget_take so the page honors the byte budget:
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
    /// Phase E.3 (C): restrict matches to symbols whose file is at or
    /// under this directory prefix. Empty / absent = whole graph.
    pub subtree: Option<&'a str>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
    pub brief: bool,
    /// When `true`, the handler returns the sentinel envelope
    /// (`results: []`, `total: <real count>`, `offset: 0`, `limit: 0`,
    /// `truncated: false`, `next_offset: None`) without materializing
    /// `SymbolResult`s. Wired into the early-return path; the Graph-layer
    /// heap short-circuit (`SearchParams.count_only`) ensures the
    /// BinaryHeap<TopEntry> is never constructed on this path.
    pub count_only: bool,
    /// When `true`, search by edit distance instead of regex/substring.
    /// `query` must be a plain identifier (`^[A-Za-z0-9_]+$`); regex
    /// metacharacters are rejected with a tool error. `max_distance`
    /// controls the threshold (or falls back to the length-adaptive
    /// default used by the suggestion path). Results are sorted by
    /// `(edit_distance asc, name asc)` so the closest matches paginate
    /// first.
    ///
    /// Pairs with the zero-result Levenshtein fallback the
    /// `search_symbols` happy path already runs: `near=true` exposes
    /// the same fuzzy machinery as the primary mode rather than a
    /// failure-only suggestion.
    pub near: bool,
    /// Max edit distance for `near=true` mode. When `None`, falls back
    /// to the length-adaptive default (0 for length 0-1, 1 for 2-11,
    /// 2 for 12-17, 3 for 18+). Ignored when `near=false`.
    /// Clamped to 8 — a higher cap doesn't meaningfully discriminate
    /// "fuzzy hit" from "totally different identifier" and the DP
    /// cost grows with the cap.
    pub max_distance: Option<u32>,
}

/// `search_symbols` body. Validates that at least one filter was supplied,
/// parses string-typed filters into their typed forms, then delegates to
/// `Graph::search`. The pagination envelope is always present — `total`
/// is reported pre-pagination so callers can render "page X of Y" UIs.
///
/// **Architectural exception:**
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
        // addition (the first consumer of `Symbol::language`). Listing
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
    // Normalize subtree the same way every path-taking tool does.
    // Scope validation lives at the server-dispatch boundary in
    // `CodeGraphServer::validate_subtree`; by the time this function
    // is called, `input.subtree` is either absent / empty (no filter)
    // or an already-canonical path proven to be at or under the
    // indexed root.
    let resolved_subtree: Option<std::path::PathBuf> = input
        .subtree
        .filter(|s| !s.is_empty())
        .map(code_graph_core::paths::normalize_user_path);

    // Count-only short-circuit: delegate to `Graph::search` with
    // `count_only=true` so the
    // BinaryHeap<TopEntry> is never constructed. `sr.symbols` is guaranteed
    // empty on this path; only `sr.total` (the pre-pagination match count)
    // is meaningful. Emit the documented sentinel envelope.
    // The `^…$` anchored-zero `suggestions` enrichment deliberately does NOT
    // apply on this path: count_only callers opted out of the records-bearing
    // response, and a suggestion list would breach the < 1 KB sentinel
    // contract. Anchored-exact misses under count_only get a bare count only.
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
        // `limit: 0` is a deliberate exception to the
        // "envelope echoes resolved limit" contract. count_only callers
        // opted out of paging; echoing a would-have-been limit would
        // mislead them into thinking there's a record page to fetch. The
        // exception is documented in CLAUDE.md alongside the count_only
        // sub-block.
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

    // Near mode (fuzzy / edit-distance) dispatch. Branches BEFORE the
    // regex/substring path. `query` is required and must be a plain
    // identifier (no regex metacharacters) — the edit-distance pass
    // operates over the symbol-name dictionary, not over regex
    // matches. `count_only` is incompatible with near mode: a
    // count-only fuzzy search would still have to do the full N-scan
    // (no heap short-circuit makes sense here), so we reject the
    // combination rather than silently incurring the cost. Kind /
    // language / namespace filters compose normally as a
    // post-distance filter pass.
    if input.near {
        if query_str.is_empty() {
            return tool_error("near mode requires a non-empty 'query'");
        }
        if !is_plain_identifier(query_str) {
            return tool_error(
                "near mode requires a plain identifier query (no regex metacharacters); \
                 use the default mode if you need regex matching",
            );
        }
        // Clamp + length-adapt the distance threshold.
        let max_distance = input
            .max_distance
            .map(|d| (d as usize).min(8))
            .unwrap_or_else(|| max_distance_for_query(query_str.len()));
        return near_search(
            graph,
            query_str,
            max_distance,
            parsed_kind,
            parsed_language,
            namespace_str,
            resolved_limit,
            resolved_offset,
            input.brief,
            max_bytes,
        );
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

    let page = Page::<SymbolResult> {
        results,
        total: sr.total,
        offset: resolved_offset,
        limit: resolved_limit,
        truncated,
        next_offset,
    };

    // Anchored-zero suggestion trigger. Only an *exact-match* query
    // (`^…$`) that found zero results earns did-you-mean candidates: an
    // anchored query expresses "I expect this precise symbol to exist", so
    // a zero-result anchored query is most likely a typo of a real symbol.
    // A non-anchored zero-result query is treated as "this concept is not
    // in the codebase" and gets NO suggestions — the absence is the
    // answer, not an error to recover from. `query_str` is the raw,
    // untransformed user input the tool received, so the `^`/`$` test
    // keys off exactly what the caller typed.
    //
    // The candidate pool comes from a broad substring match on the
    // anchors-stripped inner pattern via `Graph::search_symbols(inner,
    // None)` (no kind filter), taking the first five matches' symbol-id
    // strings. `Graph::search_symbols` returns `Vec<Symbol>`, so the id
    // string is constructed with `symbol_id` (the same idiom
    // `suggest_symbols` uses); it is intentionally NOT reused here because
    // it returns a comma-joined `String` shaped for error messages, and
    // its other callers depend on that form.
    //
    // A degenerate `"^$"` query strips to an empty inner pattern. Calling
    // the broad matcher with `""` would match every symbol in the graph
    // and surface noise, so the empty-inner case is short-circuited to no
    // suggestions — an exact-match request for the empty string has no
    // meaningful "did you mean".
    let suggestions: Vec<String> = if page.total == 0
        && query_str.starts_with('^')
        && query_str.ends_with('$')
        && query_str.len() >= 2
    {
        let inner = &query_str[1..query_str.len() - 1];
        if inner.is_empty() {
            Vec::new()
        } else {
            // First try the existing substring matcher — it handles the
            // common "user typed half the name" case cheaply.
            let substring_hits: Vec<String> = graph
                .read()
                .search_symbols(inner, None)
                .iter()
                .take(5)
                .map(symbol_id)
                .collect();
            if !substring_hits.is_empty() {
                substring_hits
            } else {
                // Substring matcher came up empty: the user likely typed
                // a typo (off by an edit or two) rather than a half-name.
                // Run an edit-distance pass over symbol names and return
                // the closest hits. Threshold scales with name length so
                // single-char queries don't match every short name.
                //
                // Only fire on plain-identifier inner patterns —
                // anything containing regex metacharacters is presumed
                // to be an intentional regex, not a name to fuzzy-match.
                if is_plain_identifier(inner) {
                    levenshtein_suggestions(&graph.read(), inner, 5)
                } else {
                    Vec::new()
                }
            }
        }
    } else {
        Vec::new()
    };

    let response = SearchSymbolsResponse { page, suggestions };
    tool_success_json(&response)
}

/// Whether `s` consists of identifier-safe bytes only — ASCII letters,
/// digits, and `_`. Used to gate the Levenshtein fallback so we don't
/// fuzzy-match patterns that contain regex metacharacters (the user
/// presumably wrote a regex intentionally, not a typo).
///
/// Unicode identifiers (e.g. CJK) currently fall outside the
/// fast-path; they go through the standard substring matcher. Adding
/// Unicode identifier support here is straightforward
/// (`UnicodeXID::is_xid_continue`) but pulls a new dependency for a
/// low-value case — deferred until a user actually hits it.
fn is_plain_identifier(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Return up to `limit` symbols whose name is within an
/// length-adaptive Levenshtein distance of `inner`. Sorted by
/// ascending distance, then alphabetically.
///
/// The distance threshold scales with name length: 1 edit at lengths
/// 2-11, 2 edits at 12-17, 3 edits at 18+. This avoids "every short
/// name matches every 1-char query" while still giving meaningful
/// hits on long identifiers (`FAchievmentsClient` → `FAchievementsClient`
/// is a 1-edit fix at length 19).
///
/// Cost: O(N × len²) where N is the symbol name count (~700k on
/// Engine-scale codebases) and `len` is `inner.len()`. The standard
/// two-row Wagner-Fischer DP is fast enough at typical query lengths
/// (~50ms on Engine) for the failure-path-only fallback. A BK-tree or
/// min-hash index would amortize this if benchmarks ever justify it;
/// the unconditional N-scan keeps the implementation slim until then.
fn levenshtein_suggestions(graph: &Graph, inner: &str, limit: usize) -> Vec<String> {
    let max_distance = max_distance_for_query(inner.len());
    let mut candidates: Vec<(usize, &Symbol)> = Vec::new();
    let inner_chars: Vec<char> = inner.chars().collect();
    for sym in graph.all_symbols() {
        let name_chars: Vec<char> = sym.name.chars().collect();
        // Length-difference quick-reject: two strings whose lengths
        // differ by more than `max_distance` cannot be within
        // `max_distance` edits of each other.
        if name_chars.len().abs_diff(inner_chars.len()) > max_distance {
            continue;
        }
        let d = levenshtein(&inner_chars, &name_chars, max_distance);
        if d <= max_distance {
            candidates.push((d, sym));
        }
    }
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));
    candidates
        .into_iter()
        .take(limit)
        .map(|(_, s)| symbol_id(s))
        .collect()
}

/// Length-adaptive max-edit-distance gate. Documented in the
/// [`levenshtein_suggestions`] doc-comment.
fn max_distance_for_query(len: usize) -> usize {
    match len {
        0..=1 => 0,
        2..=11 => 1,
        12..=17 => 2,
        _ => 3,
    }
}

/// Wagner-Fischer two-row Levenshtein with early-exit when the
/// minimum value in the current row exceeds `cap`. Returns the true
/// distance if it is `<= cap`, otherwise any value `> cap` (the
/// caller only uses the `<= cap` comparison).
///
/// `a` and `b` are passed as char slices so multi-byte code points
/// count as one edit (e.g. accented characters), not one per byte.
fn levenshtein(a: &[char], b: &[char], cap: usize) -> usize {
    let n = a.len();
    let m = b.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    // Two-row DP. `prev` holds row i-1; `curr` is row i.
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr: Vec<usize> = vec![0; m + 1];
    for i in 1..=n {
        curr[0] = i;
        let mut row_min = curr[0];
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = std::cmp::min(
                std::cmp::min(curr[j - 1] + 1, prev[j] + 1),
                prev[j - 1] + cost,
            );
            if curr[j] < row_min {
                row_min = curr[j];
            }
        }
        // Early-exit: if no cell in the current row is within `cap`,
        // every subsequent row can only stay the same or grow, so the
        // final value will also exceed `cap`.
        if row_min > cap {
            return cap + 1;
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

/// `search_symbols(near=true, …)` body. Edit-distance scan over the
/// whole symbol-name dictionary, post-filtered by kind / language /
/// namespace, sorted `(distance asc, name asc)`, paginated, then
/// byte-budgeted via the same envelope-overhead accounting the
/// regex/substring path uses. Returns the standard `Page<SymbolResult>`
/// envelope so consumers can switch between modes without reshaping
/// their deserializer.
///
/// Compared with the failure-only Levenshtein in the suggestion path:
/// this version (a) is opt-in, (b) returns full SymbolResult records
/// instead of bare symbol-id strings, (c) honors `total` /
/// `truncated` / `next_offset` for pagination, and (d) composes with
/// kind/language/namespace filters.
#[allow(clippy::too_many_arguments)]
fn near_search(
    graph: &RwLock<Graph>,
    query: &str,
    max_distance: usize,
    kind: Option<code_graph_core::SymbolKind>,
    language: Option<code_graph_core::Language>,
    namespace: &str,
    resolved_limit: u32,
    resolved_offset: u32,
    brief: bool,
    max_bytes: usize,
) -> CallToolResult {
    let query_chars: Vec<char> = query.chars().collect();
    let lower_ns = namespace.to_lowercase();

    // Phase 1: scan all symbols, keep those within `max_distance`. The
    // length-difference quick-reject prunes most candidates without
    // entering the DP. Apply kind/language/namespace post-filters
    // alongside so we never compute distance for a symbol that would
    // have been dropped anyway.
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

    // Phase 2: sort by (distance asc, name asc) so the closest matches
    // paginate first and ties resolve deterministically.
    hits.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.name.cmp(&b.1.name)));

    // Phase 3: paginate via byte_budget_take on the SymbolResult-mapped
    // form. The mapping happens before the cut so each candidate is
    // sized against the budget as JSON, not as Rust.
    let mapped: Vec<SymbolResult> = hits
        .into_iter()
        .map(|(_d, s)| symbol_to_result(&s, brief))
        .collect();
    let (results, _kept, truncated, next_offset) =
        super::byte_budget_take(mapped, resolved_offset, resolved_limit, max_bytes);

    let response = Page::<SymbolResult> {
        results,
        total,
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

/// `get_symbol_summary` body. Returns a [`Page`]`<`[`SummaryRow`]`>`
/// envelope where each row is one `(namespace, kind, count)` triple.
///
/// The response is the shared `Page<T>` envelope rather than a nested
/// `HashMap<String, HashMap<&'static str, u32>>` so the summary handler
/// reuses the existing pagination + byte-budget machinery (the nested
/// shape caused a 196 KB UE-scale rejection). Pagination flow:
/// flatten → sort by `(namespace, kind)` → `byte_budget_take`. Empty
/// namespaces display as `<global>` via a row-build-time substitution;
/// the graph's `Symbol.namespace` field is never mutated, and
/// `search_symbols(namespace="")` still filters by the empty string.
///
/// Defaults: `limit = 100`, `offset = 0`. `limit = 0` means "use the
/// default" (mirrors `get_orphans` and `get_file_symbols`); `limit` is
/// silently clamped at 1000. `offset >= total` returns an empty `results`
/// page with the correct `total`. Rows are sorted by `(namespace, kind)`
/// ascending so page 1 + page 2 partition the rows deterministically
/// across calls. The `<global>` substitution happens BEFORE the sort, so
/// `<global>` rows sort wherever `<` lands in ASCII (between `;` and `=`).
///
/// When `count_only = true`, the handler returns the sentinel
/// response shape `Page { results: [], total, offset: 0, limit: 0,
/// truncated: false, next_offset: None }` without flattening, sorting, or
/// invoking the byte-budget helper. `total` is the count of distinct
/// `(namespace, kind)` pairs across the summary — i.e. the row count the
/// paginated path would emit — NOT the sum of per-pair symbol counts.
/// Mirrors `get_orphans` / `search_symbols` / `get_file_symbols` count_only
/// semantics. `count_only` callers opt out of paging, so `limit: 0` is a
/// deliberate exception to the "envelope echoes resolved limit" contract
/// (see CLAUDE.md).
pub fn get_symbol_summary(
    graph: &RwLock<Graph>,
    file: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
    count_only: bool,
    max_bytes: usize,
) -> CallToolResult {
    let path: Option<&Path> = file.filter(|s| !s.is_empty()).map(Path::new);
    let summary = graph.read().symbol_summary(path);

    // Count-only short-circuit: emit the sentinel envelope
    // WITHOUT flattening rows or invoking `byte_budget_take`. `total` is
    // the number of distinct `(namespace, kind)` pairs — i.e. the row
    // count the paginated path below would emit (one row per inner-map
    // entry). Summing inner-map lengths matches the nested-loop count
    // exactly, so `count_only=true` and `count_only=false` agree on `total`.
    if count_only {
        let total: u32 = summary.values().map(|m| m.len()).sum::<usize>() as u32;
        // `limit: 0` is a deliberate exception to the
        // "envelope echoes resolved limit" contract.
        // count_only callers opted out of paging; echoing a would-have-been
        // limit would mislead them into thinking there's a record page to
        // fetch. The exception is documented in CLAUDE.md alongside the
        // count_only sub-block.
        let response = Page::<SummaryRow> {
            results: vec![],
            total,
            offset: 0,
            limit: 0,
            truncated: false,
            next_offset: None,
        };
        return tool_success_json(&response);
    }

    // Flatten the nested map: one row per (namespace, kind) pair. The
    // empty-namespace -> `<global>` rename is applied at row
    // build time, BEFORE the sort, and only here — the graph's
    // `Symbol.namespace` stays empty and `search_symbols(namespace="")`
    // still filters by the empty string. The rename is intentionally
    // asymmetric: display label vs query filter.
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

    // Sort by `(namespace, kind)` ascending so pagination is deterministic
    // across calls. `Graph::symbol_summary` walks a HashMap, so without
    // this canonicalization the page boundaries would shift between runs.
    // The sort key matches the contract documented on `SummaryRow`.
    rows.sort_by(|a, b| (a.namespace.as_str(), a.kind).cmp(&(b.namespace.as_str(), b.kind)));

    let total = rows.len() as u32;

    // Resolve defaults: zero-or-missing limit -> 100; clamp at 1000.
    // Mirrors `get_orphans` / `get_file_symbols` conventions.
    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(100).min(1000);
    let resolved_offset = offset.unwrap_or(0);

    // Route through byte_budget_take so the page honors the byte budget:
    // the helper internally applies offset+limit skip/take and stops early
    // if the running serialized byte count would exceed
    // `max_bytes - ENVELOPE_OVERHEAD_BYTES`. `total_kept` from the helper is
    // `results.len() as u32`, NOT the pre-pagination row count — that's
    // `total` captured above and held unchanged.
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
        // All three include id/name/kind/line; namespace is "" so omitted.
        // SymbolResult carries no `file` field — the id already encodes
        // it via `code_graph_core::symbol_id` (`file:name` /
        // `file:Parent::name`). Clients recover via
        // `code_graph_core::id_to_file`.
        for entry in arr {
            assert!(entry.get("id").is_some());
            assert!(entry.get("name").is_some());
            assert!(entry.get("kind").is_some());
            // `file` must NOT be in the record (intentionally not serialized).
            assert!(entry.get("file").is_none());
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

    // --- pagination invariants --------------------------------------------

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

    // --- byte-budget invariants -------------------------------------------

    #[test]
    fn file_symbols_byte_budget_truncates_oversized_page() {
        // A tight `max_bytes` must make `get_file_symbols` stop emitting
        // records before reaching `limit`, surface `truncated=true`, and
        // report a usable `next_offset`.
        //
        // Fixture: 30 free functions named `func_000`..`func_029` in
        // `/big.cpp`. Each serialized SymbolResult in brief mode is ~60-70
        // bytes (`{"id":"/big.cpp:func_NNN","name":"func_NNN","kind":
        // "function","line":1}` plus the helper's +1 inter-record comma).
        // SymbolResult carries no `file` field — the `id` already
        // encodes it.
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
        // CRITICAL invariant: an empty raw set returns the documented
        // error envelope (NOT a Page<T>) regardless
        // of `max_bytes`. The byte-budget step runs after the empty-raw-set
        // check, so a tight budget cannot mask the diagnostic error.
        let g = locked(Graph::new());
        // Even with a pathologically tight budget that would normally
        // truncate everything, the empty-raw-set branch is preserved.
        let r = get_file_symbols(&g, "/missing.cpp", false, true, None, None, false, 0);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "no symbols found in file: /missing.cpp");
    }

    // --- user-path normalization ------------------------------------------

    #[test]
    fn file_symbols_resolves_dot_segments_to_canonical_lookup() {
        // The handler wraps the user-supplied `file` argument with
        // `paths::normalize_user_path` before the graph
        // lookup. This test plants a symbol in the graph keyed by a real
        // canonical filesystem path, then queries the handler twice — once
        // with the canonical form, once with a `./` + `subdir/..` injected
        // form that resolves to the same canonical via `dunce::canonicalize`.
        // Both calls must succeed and return the same record set.
        //
        // The path must exist on disk so the canonicalize branch is exercised
        // (the lexical-fallback branch on a non-existent path would NOT
        // resolve dot segments, per `paths.rs` test `(d)`).
        let tmp = tempfile::TempDir::new().expect("create tempdir");
        let sub = tmp.path().join("sub");
        std::fs::create_dir(&sub).expect("create sub dir");
        let file_path = tmp.path().join("file.cpp");
        std::fs::write(&file_path, "// empty\n").expect("write file");

        // Capture the canonical form the graph will be keyed by. On Linux
        // this is identity for an already-canonical path; the explicit
        // canonicalize step keeps the test correct under symlinked tempdirs
        // (e.g. macOS `/var` -> `/private/var`).
        let canonical = paths::canonicalize(&file_path).expect("canonicalize file");
        let canonical_str = canonical
            .to_str()
            .expect("canonical path is valid UTF-8 on Linux");

        // Build a graph whose Symbol is keyed by the canonical path.
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: canonical_str.to_string(),
            language: Language::Cpp,
            symbols: vec![sym("the_func", SymbolKind::Function, canonical_str, "")],
            edges: Vec::new(),
        });
        let g = locked(g);

        // (1) Canonical form — the baseline. Asserts the fixture is sound
        // before we exercise the normalize path.
        let r_canonical = get_file_symbols(
            &g,
            canonical_str,
            false,
            true,
            None,
            None,
            false,
            NO_BYTE_BUDGET,
        );
        assert!(r_canonical.is_error.is_none() || r_canonical.is_error == Some(false));
        let (arr_canonical, total_canonical, _, _) = page_parts(&r_canonical);
        assert_eq!(arr_canonical.len(), 1);
        assert_eq!(total_canonical, 1);

        // (2) `./sub/../file.cpp` form — the load-bearing assertion. Without
        // `normalize_user_path`, this string would fail an exact-match graph
        // lookup against the canonical key and trip the "no symbols found"
        // error.
        let messy = tmp.path().join(".").join("sub").join("..").join("file.cpp");
        let messy_str = messy.to_str().expect("messy path is valid UTF-8 on Linux");
        // Sanity: the messy form is NOT byte-equal to the canonical form,
        // so a successful query proves the normalize step did real work.
        assert_ne!(
            messy_str, canonical_str,
            "messy fixture must differ from canonical for the test to be meaningful"
        );

        let r_messy = get_file_symbols(
            &g,
            messy_str,
            false,
            true,
            None,
            None,
            false,
            NO_BYTE_BUDGET,
        );
        assert!(
            r_messy.is_error.is_none() || r_messy.is_error == Some(false),
            "messy form must succeed after normalize: body={}",
            body_text(&r_messy),
        );
        let (arr_messy, total_messy, _, _) = page_parts(&r_messy);
        assert_eq!(
            arr_messy.len(),
            1,
            "messy form must return the same record set"
        );
        assert_eq!(total_messy, 1);
        // Same id round-trips through both forms.
        assert_eq!(arr_messy[0]["id"], arr_canonical[0]["id"]);
    }

    // --- count_only invariants --------------------------------------------

    #[test]
    fn file_symbols_count_only_returns_sentinel_envelope_under_1kb() {
        // When count_only=true, the handler emits
        // Page { results: [], total: <real count>, offset: 0,
        // limit: 0, truncated: false, next_offset: None } regardless of how
        // many records WOULD have been returned. Serialized envelope size
        // must stay < 1KB even at the 1000-symbol scale.
        //
        // Asserts: (a) results is empty, (b) total reflects the true match
        // count (not zero), (c) limit=0 (count_only opts out of paging, a
        // deliberate exception to the "envelope echoes resolved limit"
        // contract; see CLAUDE.md),
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
            subtree: None,
            brief: true,
            ..SearchSymbolsInput::default()
        }
    }

    /// Two-file graph for subtree-filter tests: one symbol under /a,
    /// one under /b. Both match `query="foo"`. Subtree filter should
    /// narrow to whichever subtree the test selects.
    fn graph_with_foos_in_two_subtrees() -> Graph {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/a/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym_in("foo", SymbolKind::Function, "/a/x.cpp")],
            edges: vec![],
        });
        g.merge_file_graph(FileGraph {
            path: "/b/y.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym_in("foo", SymbolKind::Function, "/b/y.cpp")],
            edges: vec![],
        });
        g
    }

    /// Local sym constructor to avoid pulling in another helper module.
    fn sym_in(name: &str, kind: SymbolKind, file: &str) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind,
            file: file.to_string(),
            line: 1,
            column: 0,
            end_line: 1,
            signature: String::new(),
            namespace: String::new(),
            parent: String::new(),
            language: Language::Cpp,
        }
    }

    #[test]
    fn search_symbols_subtree_filter_narrows_via_trie_iter_subtree() {
        // Phase E.3 (C) payoff site for search_symbols: a `subtree="/a"`
        // query routes through `Graph::search`'s `subtree_files`
        // pre-walk (which uses `PathTrie::iter_subtree`) and matches
        // ONLY the /a symbol, not the /b one — even though both match
        // the regex.
        let g = locked(graph_with_foos_in_two_subtrees());
        let input = SearchSymbolsInput {
            query: Some("foo"),
            subtree: Some("/a"),
            ..search_input()
        };
        let r = search_symbols(&g, input, NO_BYTE_BUDGET);
        let body: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let results = body["results"].as_array().unwrap();
        assert_eq!(
            results.len(),
            1,
            "subtree=/a should drop the /b match; body={body}"
        );
        // SymbolId carries the file path as a prefix — check that the
        // returned record is the /a one, regardless of which optional
        // brief/full fields the wire shape happens to expose.
        let sid = results[0]["id"].as_str().unwrap();
        assert!(
            sid.starts_with("/a/x.cpp"),
            "expected symbol from /a/x.cpp; got id={sid}"
        );
        assert_eq!(body["total"].as_u64().unwrap(), 1);

        // No subtree → both match.
        let input_all = SearchSymbolsInput {
            query: Some("foo"),
            ..search_input()
        };
        let r_all = search_symbols(&g, input_all, NO_BYTE_BUDGET);
        let body_all: serde_json::Value = serde_json::from_str(&body_text(&r_all)).unwrap();
        assert_eq!(body_all["results"].as_array().unwrap().len(), 2);
        assert_eq!(body_all["total"].as_u64().unwrap(), 2);
    }

    #[test]
    fn search_symbols_subtree_count_only_uses_subtree() {
        // count_only path must respect the subtree filter — otherwise
        // count_only would report graph-wide totals where the
        // materializing path reports subtree-scoped results.
        let g = locked(graph_with_foos_in_two_subtrees());
        let input = SearchSymbolsInput {
            query: Some("foo"),
            subtree: Some("/a"),
            count_only: true,
            ..search_input()
        };
        let r = search_symbols(&g, input, NO_BYTE_BUDGET);
        let body: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(body["total"].as_u64().unwrap(), 1);
        assert!(body["results"].as_array().unwrap().is_empty());
    }

    #[test]
    fn search_symbols_response_flatten_hoists_page_fields_and_keeps_suggestions() {
        // The `#[serde(flatten)]` on `page` must hoist every Page envelope
        // field to the top level (NOT nest them under a `"page"` key), and
        // `suggestions` rides alongside as an additive sibling array. This
        // is the wire-shape contract a suggesting search_symbols response
        // must honor so agents pattern-matching on top-level
        // results/total/offset/limit keep working unchanged.
        use super::super::SearchSymbolsResponse;

        let resp = SearchSymbolsResponse {
            page: Page {
                results: vec![],
                total: 0,
                offset: 0,
                limit: 20,
                truncated: false,
                next_offset: None,
            },
            suggestions: vec!["Foo".into(), "Bar".into()],
        };
        let json = serde_json::to_string(&resp).unwrap();

        // Flatten hoisted the envelope fields to the top level.
        assert!(
            json.contains(r#""results":[]"#),
            "flatten must hoist `results` to the top level: {json}"
        );
        assert!(
            json.contains(r#""total":0"#),
            "flatten must hoist `total` to the top level: {json}"
        );
        assert!(
            json.contains(r#""offset":0"#),
            "flatten must hoist `offset` to the top level: {json}"
        );
        assert!(
            json.contains(r#""limit":20"#),
            "flatten must hoist `limit` to the top level: {json}"
        );
        // No `"page"` wrapper key — flatten erases the nesting.
        assert!(
            !json.contains(r#""page""#),
            "flatten must NOT emit a `page` wrapper key: {json}"
        );
        // Suggestions present, top-level, in declaration order.
        assert!(
            json.contains(r#""suggestions":["Foo","Bar"]"#),
            "suggestions must serialize as a top-level array: {json}"
        );

        // Re-parse and assert structurally (not just substring) that the
        // envelope fields live at the root, never under a `page` object.
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let obj = v.as_object().unwrap();
        assert!(obj.contains_key("results"));
        assert!(obj.contains_key("total"));
        assert!(obj.contains_key("truncated"));
        assert!(obj.contains_key("next_offset"));
        assert!(
            !obj.contains_key("page"),
            "flattened response must not carry a nested `page` object"
        );
        assert_eq!(
            obj["suggestions"],
            serde_json::json!(["Foo", "Bar"]),
            "suggestions array round-trips at the root"
        );
    }

    #[test]
    fn search_symbols_response_empty_suggestions_omits_the_key_entirely() {
        // `skip_serializing_if = "Vec::is_empty"` contract: an empty
        // suggestions list must be ABSENT from the JSON (no
        // `"suggestions":[]`), so a non-suggesting response is
        // byte-identical to the legacy bare Page<SymbolResult> envelope.
        use super::super::SearchSymbolsResponse;

        let resp = SearchSymbolsResponse {
            page: Page {
                results: vec![],
                total: 0,
                offset: 0,
                limit: 20,
                truncated: false,
                next_offset: None,
            },
            suggestions: vec![],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            !json.contains("suggestions"),
            "empty suggestions must be absent, not serialized as []: {json}"
        );
        // The flattened envelope must still be byte-identical to a bare
        // Page<SymbolResult> with the same field values — proving the
        // wrapper is wire-compatible when not suggesting.
        let bare: Page<SymbolResult> = Page {
            results: vec![],
            total: 0,
            offset: 0,
            limit: 20,
            truncated: false,
            next_offset: None,
        };
        assert_eq!(
            json,
            serde_json::to_string(&bare).unwrap(),
            "non-suggesting response must match the legacy bare envelope byte-for-byte"
        );
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
                subtree: None,
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
                subtree: None,
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
                subtree: None,
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
                subtree: None,
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
                subtree: None,
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

    /// Anchored exact-match query that's one edit off a real symbol
    /// must surface a Levenshtein suggestion. The substring matcher
    /// alone cannot find this (the typo doesn't contain a substring
    /// of any real symbol) — only the edit-distance fallback can.
    ///
    /// Note: the typo MUST be within the length-adaptive distance gate.
    /// `^Actr$` (len 4) → `AActor` (len 6) has |len_diff|=2, which
    /// exceeds the max_distance=1 threshold for length-4 queries — so
    /// the gate quick-rejects it before computing the edit distance.
    /// `^AActr$` (len 5, distance 1 from `AActor`) is the right shape.
    #[test]
    fn search_symbols_anchored_typo_falls_back_to_levenshtein() {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("AActor", SymbolKind::Class, "/x.cpp", "")],
            edges: Vec::new(),
        });
        let g = locked(g);
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("^AActr$"), // 1 edit from "AActor" (insert 'o' between 't' and 'r')
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(parsed["total"], serde_json::json!(0), "no exact match");
        let suggestions: Vec<String> = parsed["suggestions"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            suggestions.iter().any(|s| s.ends_with(":AActor")),
            "Levenshtein fallback must suggest AActor for the 1-edit typo; got {suggestions:?}"
        );
    }

    /// Long-name typo (2 edits at length 19) must still match through
    /// the length-adaptive distance gate.
    #[test]
    fn search_symbols_anchored_long_typo_two_edits_returns_suggestion() {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("FAchievementsClient", SymbolKind::Class, "/x.cpp", "")],
            edges: Vec::new(),
        });
        let g = locked(g);
        // Missing 'e' AND wrong case on 'c' = 2 edits.
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("^FAchievmentsClient$"),
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(parsed["total"], serde_json::json!(0));
        let suggestions: Vec<String> = parsed["suggestions"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            suggestions
                .iter()
                .any(|s| s.ends_with(":FAchievementsClient")),
            "long-typo Levenshtein must suggest FAchievementsClient; got {suggestions:?}"
        );
    }

    /// A query containing regex metacharacters must NOT trigger the
    /// Levenshtein fallback — the user wrote a regex intentionally.
    #[test]
    fn search_symbols_regex_query_does_not_fuzzy_match() {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("AActor", SymbolKind::Class, "/x.cpp", "")],
            edges: Vec::new(),
        });
        let g = locked(g);
        // The inner pattern contains `.*` — a regex, not an identifier.
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("^.*ZZZNoSuchSymbol.*$"),
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(parsed["total"], serde_json::json!(0));
        // No suggestions key emitted because nothing matched.
        assert!(
            parsed.get("suggestions").is_none()
                || parsed["suggestions"].as_array().unwrap().is_empty(),
            "regex query must not trigger fuzzy fallback"
        );
    }

    #[test]
    fn levenshtein_distance_matches_known_pairs() {
        let a: Vec<char> = "kitten".chars().collect();
        let b: Vec<char> = "sitting".chars().collect();
        assert_eq!(levenshtein(&a, &b, 5), 3);
    }

    #[test]
    fn levenshtein_early_exit_returns_over_cap_value() {
        let a: Vec<char> = "abcdef".chars().collect();
        let b: Vec<char> = "zzzzzz".chars().collect();
        // True distance is 6; cap of 2 forces early-exit.
        let d = levenshtein(&a, &b, 2);
        assert!(d > 2, "early-exit must return a value > cap; got {d}");
    }

    #[test]
    fn max_distance_for_query_length_adaptive() {
        assert_eq!(max_distance_for_query(0), 0);
        assert_eq!(max_distance_for_query(1), 0);
        assert_eq!(max_distance_for_query(5), 1);
        assert_eq!(max_distance_for_query(11), 1);
        assert_eq!(max_distance_for_query(12), 2);
        assert_eq!(max_distance_for_query(17), 2);
        assert_eq!(max_distance_for_query(100), 3);
    }

    #[test]
    fn is_plain_identifier_basic_cases() {
        assert!(is_plain_identifier("AActor"));
        assert!(is_plain_identifier("snake_case_name"));
        assert!(is_plain_identifier("CamelCase123"));
        assert!(!is_plain_identifier(""));
        assert!(!is_plain_identifier("foo.bar"));
        assert!(!is_plain_identifier("foo*"));
        assert!(!is_plain_identifier("foo bar"));
        assert!(!is_plain_identifier(".*"));
    }

    /// `near=true` returns symbols within edit distance, sorted by
    /// closest match first. Edit-distance arithmetic (manually
    /// verified):
    /// - `Actor` → `Actor`   = 0
    /// - `Actor` → `AActor`  = 1 (insert leading `A`)
    /// - `Actor` → `Actr`    = 1 (delete `o`)
    /// - `Actor` → `Vector`  = 2 (substitute `A`→`V`, insert `e`)
    /// - `Actor` → `Reactor` = 3 (insert `R`, insert `e`, substitute
    ///   `A`→`a`-style case shift) — verified by the algorithm; out of
    ///   range at `max_distance=2`.
    #[test]
    fn search_symbols_near_mode_returns_within_distance_sorted_by_closest() {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("Actor", SymbolKind::Class, "/x.cpp", ""),   // d=0
                sym("AActor", SymbolKind::Class, "/x.cpp", ""),  // d=1
                sym("Actr", SymbolKind::Class, "/x.cpp", ""),    // d=1
                sym("Vector", SymbolKind::Class, "/x.cpp", ""),  // d=2
                sym("Reactor", SymbolKind::Class, "/x.cpp", ""), // d=3 (excluded)
            ],
            edges: Vec::new(),
        });
        let g = locked(g);
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("Actor"),
                near: true,
                max_distance: Some(2),
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let results: Vec<&str> = parsed["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        assert!(results.contains(&"Actor"), "d=0 present; got {results:?}");
        assert!(results.contains(&"AActor"), "d=1 present; got {results:?}");
        assert!(results.contains(&"Actr"), "d=1 present; got {results:?}");
        assert!(results.contains(&"Vector"), "d=2 present; got {results:?}");
        assert!(
            !results.contains(&"Reactor"),
            "d=3 must be excluded at max_distance=2; got {results:?}"
        );
        // First result must be the exact match (distance 0).
        assert_eq!(
            results.first(),
            Some(&"Actor"),
            "sort must put closest-match first; got {results:?}"
        );
    }

    /// `near=true` with regex metacharacters in query is rejected as
    /// a tool error — near mode is for plain identifiers only.
    #[test]
    fn search_symbols_near_mode_rejects_regex_query() {
        let g = locked(small_graph());
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("^Foo.*$"),
                near: true,
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        assert_eq!(r.is_error, Some(true));
        let body = body_text(&r);
        assert!(
            body.contains("plain identifier"),
            "rejection must name the plain-identifier requirement; got: {body}"
        );
    }

    /// `near=true` with empty query is rejected — fuzzy match needs
    /// something to compare against.
    #[test]
    fn search_symbols_near_mode_rejects_empty_query() {
        let g = locked(small_graph());
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some(""),
                kind: Some("function"), // satisfy the at-least-one-filter check
                near: true,
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        assert_eq!(r.is_error, Some(true));
        let body = body_text(&r);
        assert!(
            body.contains("non-empty 'query'"),
            "rejection must mention non-empty query requirement; got: {body}"
        );
    }

    /// `near=true` composes with kind/language filters: only symbols
    /// matching BOTH the distance AND the filters appear. Two symbols
    /// named `Actor` in different files (so they get distinct symbol
    /// IDs and both survive the merge) — one a Class, one a Function.
    /// Filter to `kind="class"` and only the Class entry should appear.
    #[test]
    fn search_symbols_near_mode_composes_with_kind_filter() {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/a.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("Actor", SymbolKind::Class, "/a.cpp", "")],
            edges: Vec::new(),
        });
        g.merge_file_graph(FileGraph {
            path: "/b.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("Actor", SymbolKind::Function, "/b.cpp", "")],
            edges: Vec::new(),
        });
        let g = locked(g);
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("Actor"),
                kind: Some("class"),
                near: true,
                max_distance: Some(0),
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let results: Vec<(&str, &str)> = parsed["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["name"].as_str().unwrap(), r["kind"].as_str().unwrap()))
            .collect();
        assert_eq!(
            results.len(),
            1,
            "kind filter must drop the Function entry; got {results:?}"
        );
        assert_eq!(results[0], ("Actor", "class"));
    }

    /// `near=true` with no `max_distance` uses the length-adaptive
    /// default — same threshold table as the suggestion fallback.
    #[test]
    fn search_symbols_near_mode_default_distance_is_length_adaptive() {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("AActor", SymbolKind::Class, "/x.cpp", "")],
            edges: Vec::new(),
        });
        let g = locked(g);
        // "AActr" → "AActor" is distance 1; default max_distance for
        // length 5 is 1, so this should match.
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                query: Some("AActr"),
                near: true,
                max_distance: None,
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(parsed["total"], serde_json::json!(1));
    }

    #[test]
    fn search_symbols_kind_only_filter_accepted() {
        let g = locked(small_graph());
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                subtree: None,
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
                subtree: None,
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
        // A tight `max_bytes` must make the handler trim its
        // already-sliced page from
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
                subtree: None,
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
                subtree: None,
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
                subtree: None,
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
                subtree: None,
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
                subtree: None,
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
                subtree: None,
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
                subtree: None,
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

    // --- search_symbols count_only invariants -----------------------------

    #[test]
    fn search_symbols_count_only_returns_sentinel_envelope_under_1kb() {
        // When count_only=true, the handler emits
        // Page { results: [], total: <real count>, offset: 0,
        // limit: 0, truncated: false, next_offset: None } regardless of how
        // many records WOULD have been returned. Serialized envelope must
        // stay < 1KB even at the 1000-match scale.
        //
        // Asserts: (a) results is empty, (b) total reflects the true
        // pre-pagination match count from Graph::search (not zero),
        // (c) limit=0 (deliberate exception to the "envelope echoes
        // resolved limit" contract, documented in CLAUDE.md),
        // (d) truncated=false and next_offset is None, (e) serialized body
        // is well under 1024 bytes regardless of input scale.
        //
        // `count_only` is threaded into `SearchParams`, so the
        // BinaryHeap<TopEntry> is never constructed on this path — the
        // wire-format contract above is unchanged.
        let g = locked(graph_with_n_broad_matches(1000));
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                subtree: None,
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
                subtree: None,
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
                subtree: None,
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
                subtree: None,
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
                subtree: None,
                kind: Some("widget"),
                count_only: true,
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "invalid kind: widget");
    }

    // --- search_symbols anchored-zero suggestion trigger ------------------

    /// Graph for the anchored-zero suggestion tests. Holds a class literally
    /// named `ExistingClass` (so `^ExistingClass$` matches exactly) and a
    /// class named `NotFoundClassHelper` whose name *contains*
    /// `NotFoundClass` as a substring (so the anchors-stripped broad match
    /// surfaces it as a candidate even though `^NotFoundClass$` matches
    /// nothing exactly).
    fn suggestion_graph() -> Graph {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/s.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("ExistingClass", SymbolKind::Class, "/s.cpp", ""),
                sym("NotFoundClassHelper", SymbolKind::Class, "/s.cpp", ""),
            ],
            edges: Vec::new(),
        });
        g
    }

    #[test]
    fn anchored_exact_zero_result_query_emits_substring_suggestions() {
        // `^NotFoundClass$` is an exact-match request: the case-insensitive
        // `(?i)^NotFoundClass$` regex matches no symbol (the only near name
        // is `NotFoundClassHelper`, which the anchors exclude), so
        // `total == 0`. The anchors-stripped inner pattern `NotFoundClass`
        // is then broad-matched, surfacing `NotFoundClassHelper` as a
        // did-you-mean candidate. `suggestions` must be PRESENT, hold the
        // symbol-id string, and be capped at five entries.
        let g = locked(suggestion_graph());
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                subtree: None,
                query: Some("^NotFoundClass$"),
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(
            parsed["total"],
            serde_json::json!(0),
            "anchored exact query must find zero results"
        );
        let suggestions = parsed
            .get("suggestions")
            .expect("anchored zero-result query must emit a `suggestions` key");
        let arr = suggestions.as_array().expect("suggestions is an array");
        assert!(
            !arr.is_empty() && arr.len() <= 5,
            "suggestions must hold 1..=5 entries, got {}",
            arr.len()
        );
        assert_eq!(
            arr[0],
            serde_json::json!("/s.cpp:NotFoundClassHelper"),
            "suggestion entries are symbol-id strings"
        );
    }

    #[test]
    fn non_anchored_zero_result_query_omits_suggestions_key() {
        // A non-anchored zero-result query expresses "this concept is not
        // in the codebase", not "typo of a real symbol". It earns NO
        // suggestions: the `suggestions` key must be ABSENT entirely (not
        // an empty `[]`), keeping the response byte-identical to the legacy
        // bare envelope. `Zzz_absent_Zzz` matches nothing in the fixture.
        let g = locked(suggestion_graph());
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                subtree: None,
                query: Some("Zzz_absent_Zzz"),
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(
            parsed["total"],
            serde_json::json!(0),
            "the query must find zero results"
        );
        assert!(
            parsed.get("suggestions").is_none(),
            "a non-anchored zero-result query must NOT carry a `suggestions` key: {parsed}"
        );
        assert!(
            !body_text(&r).contains("suggestions"),
            "the `suggestions` key must be wholly absent from the wire JSON"
        );
    }

    #[test]
    fn anchored_query_with_results_omits_suggestions_key() {
        // `^ExistingClass$` matches the literally-named `ExistingClass`
        // symbol exactly, so `total >= 1`. With results present there is
        // nothing to suggest: the `suggestions` key must be ABSENT.
        let g = locked(suggestion_graph());
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                subtree: None,
                query: Some("^ExistingClass$"),
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let total = parsed["total"].as_u64().unwrap();
        assert!(
            total >= 1,
            "anchored query for an existing symbol must find >=1 result, got {total}"
        );
        assert!(
            parsed.get("suggestions").is_none(),
            "a query with results must NOT carry a `suggestions` key: {parsed}"
        );
        assert!(
            !body_text(&r).contains("suggestions"),
            "the `suggestions` key must be wholly absent from the wire JSON"
        );
    }

    #[test]
    fn degenerate_caret_dollar_query_does_not_panic_and_omits_suggestions() {
        // Guard pin: `^$` is anchored (`^`-prefixed AND `$`-suffixed) with
        // `len() == 2`, so the slice `[1..len-1]` is the empty string. An
        // empty inner pattern would broad-match every symbol in the graph,
        // so the handler short-circuits the empty-inner case to NO
        // suggestions rather than dumping the whole index. The regex
        // `(?i)^$` itself matches no symbol here, so `total == 0` — this
        // is the anchored-zero path, and the guard (not the trigger) is
        // what suppresses suggestions. Must not panic on the slice.
        let g = locked(suggestion_graph());
        let r = search_symbols(
            &g,
            SearchSymbolsInput {
                subtree: None,
                query: Some("^$"),
                ..search_input()
            },
            NO_BYTE_BUDGET,
        );
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(
            parsed["total"],
            serde_json::json!(0),
            "`^$` matches no symbol in the fixture"
        );
        assert!(
            parsed.get("suggestions").is_none(),
            "empty inner pattern must short-circuit to NO suggestions, not match all symbols: {parsed}"
        );
        assert!(
            !body_text(&r).contains("suggestions"),
            "empty inner pattern: the `suggestions` key must be wholly absent from the wire JSON: {parsed}"
        );
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
    //
    // The response shape is a `Page<SummaryRow>` envelope (flat rows)
    // rather than a nested `HashMap<String, HashMap<&str, u32>>`. The
    // assertions below read the flat-row shape; pagination, sort,
    // `<global>` rename, and `count_only` are covered by the test
    // groups further down.

    /// Pick a single `SummaryRow` out of the response by `(namespace, kind)`.
    /// Panics with a helpful message if no row matches — callers want a
    /// loud failure, not a silent skip, when the shape regresses.
    fn pick_count(results: &[serde_json::Value], namespace: &str, kind: &str) -> u32 {
        for row in results {
            if row["namespace"] == serde_json::json!(namespace)
                && row["kind"] == serde_json::json!(kind)
            {
                return row["count"].as_u64().expect("count is integer") as u32;
            }
        }
        panic!(
            "no row matched (namespace={namespace:?}, kind={kind:?}) in {:?}",
            results,
        );
    }

    fn has_row(results: &[serde_json::Value], namespace: &str, kind: &str) -> bool {
        results.iter().any(|row| {
            row["namespace"] == serde_json::json!(namespace)
                && row["kind"] == serde_json::json!(kind)
        })
    }

    #[test]
    fn symbol_summary_whole_graph() {
        let g = locked(small_graph());
        let r = get_symbol_summary(&g, None, None, None, false, NO_BYTE_BUDGET);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        // The response is a `Page<SummaryRow>`. All 3 sample symbols
        // carry namespace="" in the graph; the display rename surfaces
        // them under `<global>` in the response.
        let results = parsed["results"].as_array().expect("results array");
        assert_eq!(pick_count(results, "<global>", "function"), 1);
        assert_eq!(pick_count(results, "<global>", "class"), 1);
        assert_eq!(pick_count(results, "<global>", "method"), 1);
        // Display-rename invariant: no row carries the bare empty string.
        assert!(!has_row(results, "", "function"));
        assert!(!has_row(results, "", "class"));
        assert!(!has_row(results, "", "method"));
        // total reflects the row count (3 rows for 3 distinct
        // (namespace, kind) pairs in the small graph fixture).
        assert_eq!(parsed["total"].as_u64().unwrap(), 3);
    }

    #[test]
    fn symbol_summary_empty_graph_returns_empty_envelope() {
        let g = locked(Graph::new());
        let r = get_symbol_summary(&g, None, None, None, false, NO_BYTE_BUDGET);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        // An empty graph yields an empty Page envelope, NOT a bare
        // empty object. The shape is intentional.
        let results = parsed["results"].as_array().expect("results array");
        assert!(results.is_empty());
        assert_eq!(parsed["total"].as_u64().unwrap(), 0);
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
        let r = get_symbol_summary(&g, Some("/b.cpp"), None, None, false, NO_BYTE_BUDGET);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let results = parsed["results"].as_array().expect("results array");
        // An empty namespace renders as `<global>` in the response.
        assert_eq!(pick_count(results, "<global>", "function"), 1);
        // No method or class rows — those are in /a.cpp only.
        assert!(!has_row(results, "<global>", "method"));
        assert!(!has_row(results, "<global>", "class"));
    }

    // --- pagination tests ---------------------------------------------------
    //
    // `get_symbol_summary` wires real pagination through. These tests pin:
    //   * basic round-trip with multiple namespaces (deterministic ordering),
    //   * sort stability across two pages,
    //   * `limit=0` resolves to the default (mirrors `get_orphans`),
    //   * `offset >= total` returns an empty page with correct `total`,
    //   * a >100-row fixture triggers byte-budget truncation under the
    //     production `[response].max_bytes` default.

    use super::super::test_helpers::page_extras;

    /// Build a graph with N distinct namespaces, each carrying one Function
    /// symbol, so `symbol_summary` yields exactly N rows of
    /// `(namespace_i, "function", 1)` once sorted. Used by the multi-page
    /// + budget-bite tests below.
    fn multi_namespace_graph(n: usize) -> Graph {
        let mut g = Graph::new();
        for i in 0..n {
            let ns = format!("ns_{:04}", i);
            let file = format!("/f_{:04}.cpp", i);
            let s = Symbol {
                name: format!("func_{:04}", i),
                kind: SymbolKind::Function,
                file: file.clone(),
                line: 1,
                column: 0,
                end_line: 1,
                signature: String::new(),
                namespace: ns,
                parent: String::new(),
                language: Language::Cpp,
            };
            g.merge_file_graph(FileGraph {
                path: file,
                language: Language::Cpp,
                symbols: vec![s],
                edges: Vec::new(),
            });
        }
        g
    }

    #[test]
    fn symbol_summary_basic_round_trip_multiple_namespaces() {
        // Two namespaces ("alpha", "beta") plus the default ""-namespace
        // rows from `small_graph` (rendered as `<global>`) —
        // three distinct namespaces, multiple kinds. Asserts `total`,
        // `results.len()`, and that two back-to-back calls produce the
        // same ordering (sort is deterministic).
        let mut g = small_graph();
        g.merge_file_graph(FileGraph {
            path: "/alpha.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![Symbol {
                name: "a_fn".to_string(),
                kind: SymbolKind::Function,
                file: "/alpha.cpp".to_string(),
                line: 1,
                column: 0,
                end_line: 1,
                signature: String::new(),
                namespace: "alpha".to_string(),
                parent: String::new(),
                language: Language::Cpp,
            }],
            edges: Vec::new(),
        });
        g.merge_file_graph(FileGraph {
            path: "/beta.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![Symbol {
                name: "b_fn".to_string(),
                kind: SymbolKind::Function,
                file: "/beta.cpp".to_string(),
                line: 1,
                column: 0,
                end_line: 1,
                signature: String::new(),
                namespace: "beta".to_string(),
                parent: String::new(),
                language: Language::Cpp,
            }],
            edges: Vec::new(),
        });
        let g = locked(g);

        let r1 = get_symbol_summary(&g, None, None, None, false, NO_BYTE_BUDGET);
        let (results1, total1, offset1, limit1) = page_parts(&r1);
        let (truncated1, next1) = page_extras(&r1);

        // small_graph: 3 rows (function/class/method in ns="" → rendered
        // as `<global>`); alpha + beta add 2 more rows (function in
        // ns="alpha", "beta"). Total = 5.
        assert_eq!(total1, 5);
        assert_eq!(results1.len(), 5);
        assert_eq!(offset1, 0);
        assert_eq!(limit1, 100);
        assert!(!truncated1);
        assert_eq!(next1, None);

        // Determinism: a second call yields identical row order.
        let r2 = get_symbol_summary(&g, None, None, None, false, NO_BYTE_BUDGET);
        let (results2, _, _, _) = page_parts(&r2);
        assert_eq!(
            results1, results2,
            "row order must be deterministic across repeated calls",
        );

        // Sort order: namespace asc, then kind asc. The `<global>`
        // rename happens at row-build time (i.e. BEFORE the sort), so
        // `<` (ASCII 60) sorts before `a` (97) and `b` (98), and
        // the `<global>` rows still come first. Expected sequence:
        //   ("<global>", "class"),
        //   ("<global>", "function"),
        //   ("<global>", "method"),
        //   ("alpha",    "function"),
        //   ("beta",     "function").
        let expected_ns_kind: Vec<(&str, &str)> = vec![
            ("<global>", "class"),
            ("<global>", "function"),
            ("<global>", "method"),
            ("alpha", "function"),
            ("beta", "function"),
        ];
        let got: Vec<(String, String)> = results1
            .iter()
            .map(|row| {
                (
                    row["namespace"].as_str().unwrap().to_string(),
                    row["kind"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let want: Vec<(String, String)> = expected_ns_kind
            .iter()
            .map(|(ns, k)| (ns.to_string(), k.to_string()))
            .collect();
        assert_eq!(got, want, "sort must be (namespace, kind) ascending");
    }

    #[test]
    fn symbol_summary_sort_stable_across_two_pages() {
        // Build a graph with 8 distinct (namespace, kind) rows; fetch page
        // 1 with limit=4, page 2 with offset=4 limit=4; concatenate and
        // assert equality with the full sorted result.
        let g = locked(multi_namespace_graph(8));

        let full = get_symbol_summary(&g, None, None, None, false, NO_BYTE_BUDGET);
        let (full_rows, full_total, _, _) = page_parts(&full);
        assert_eq!(full_total, 8);
        assert_eq!(full_rows.len(), 8);

        let p1 = get_symbol_summary(&g, None, Some(4), Some(0), false, NO_BYTE_BUDGET);
        let (p1_rows, p1_total, p1_offset, p1_limit) = page_parts(&p1);
        let (p1_truncated, p1_next) = page_extras(&p1);
        assert_eq!(p1_total, 8);
        assert_eq!(p1_rows.len(), 4);
        assert_eq!(p1_offset, 0);
        assert_eq!(p1_limit, 4);
        // Hit the limit cap before any byte cap fires; truncated stays false
        // and next_offset stays None — caller pages via offset+limit.
        assert!(!p1_truncated);
        assert_eq!(p1_next, None);

        let p2 = get_symbol_summary(&g, None, Some(4), Some(4), false, NO_BYTE_BUDGET);
        let (p2_rows, p2_total, p2_offset, p2_limit) = page_parts(&p2);
        assert_eq!(p2_total, 8);
        assert_eq!(p2_rows.len(), 4);
        assert_eq!(p2_offset, 4);
        assert_eq!(p2_limit, 4);

        // page 1 + page 2 must equal the full sorted result, row-for-row.
        let mut concat = p1_rows.clone();
        concat.extend(p2_rows.iter().cloned());
        assert_eq!(
            concat, full_rows,
            "page1 + page2 must equal the full sorted result",
        );
    }

    #[test]
    fn symbol_summary_limit_zero_resolves_to_default() {
        // limit=0 must behave identically to limit=None (default 100).
        // Mirrors `get_orphans` (Decision documented on the handler).
        let g = locked(multi_namespace_graph(50));

        let r_zero = get_symbol_summary(&g, None, Some(0), None, false, NO_BYTE_BUDGET);
        let (rows_zero, total_zero, offset_zero, limit_zero) = page_parts(&r_zero);
        let r_none = get_symbol_summary(&g, None, None, None, false, NO_BYTE_BUDGET);
        let (rows_none, total_none, offset_none, limit_none) = page_parts(&r_none);

        // Both report 50 rows, default-resolved limit=100, no truncation.
        assert_eq!(total_zero, 50);
        assert_eq!(total_none, 50);
        assert_eq!(limit_zero, 100, "limit=0 must resolve to default 100");
        assert_eq!(limit_none, 100);
        assert_eq!(offset_zero, 0);
        assert_eq!(offset_none, 0);
        // Same payload.
        assert_eq!(rows_zero, rows_none);
    }

    #[test]
    fn symbol_summary_offset_past_total_returns_empty_page() {
        // offset >= total: empty `results`, total still echoes the
        // pre-pagination row count, truncated=false, next_offset=None.
        let g = locked(multi_namespace_graph(5));

        let r = get_symbol_summary(&g, None, Some(10), Some(99), false, NO_BYTE_BUDGET);
        let (rows, total, offset, limit) = page_parts(&r);
        let (truncated, next) = page_extras(&r);
        assert!(rows.is_empty(), "offset past total yields empty page");
        assert_eq!(total, 5, "total reflects pre-pagination row count");
        assert_eq!(offset, 99);
        assert_eq!(limit, 10);
        assert!(!truncated);
        assert_eq!(next, None);
    }

    #[test]
    fn symbol_summary_limit_clamped_to_thousand() {
        // limit values above 1000 are silently clamped to 1000; the echoed
        // `limit` in the envelope reflects the resolved value.
        let g = locked(multi_namespace_graph(3));
        let r = get_symbol_summary(&g, None, Some(5000), None, false, NO_BYTE_BUDGET);
        let (_, _, _, limit) = page_parts(&r);
        assert_eq!(limit, 1000, "limit must clamp at 1000");
    }

    #[test]
    fn symbol_summary_over_one_hundred_rows_caps_at_default_limit() {
        // >100-row fixture: build 1200 distinct namespaces (each yielding
        // one row). Call with default limit; under the production 100KB
        // `max_bytes` this MUST cap the page at exactly 100 rows (the
        // count cap). Pins the regression an earlier stub was hiding:
        // the stub ignored `limit` entirely and emitted all 1200 rows.
        //
        // Each SummaryRow serialized is roughly
        //   {"namespace":"ns_0123","kind":"function","count":1}
        // ≈ 55 bytes. 100KB budget minus ENVELOPE_OVERHEAD_BYTES (512)
        // leaves room for ~1800 rows by byte budget alone — so the count
        // cap (limit=100) bites first. `truncated` is false on this path
        // (byte_budget_take only sets truncated when the BYTE budget
        // bites, not when the count cap hits) and `next_offset` is null;
        // the caller pages via `offset + limit` per the documented
        // `Page<T>` envelope contract.
        let g = locked(multi_namespace_graph(1200));

        // Use the production default `max_bytes` to mirror real callers.
        let default_max_bytes = code_graph_core::RootConfig::default().response.max_bytes;
        let r = get_symbol_summary(&g, None, None, None, false, default_max_bytes);
        let (rows, total, offset, limit) = page_parts(&r);
        let (truncated, next) = page_extras(&r);

        assert_eq!(total, 1200, "total reflects pre-pagination row count");
        assert_eq!(rows.len(), 100, "default page must cap at limit=100");
        assert_eq!(limit, 100);
        assert_eq!(offset, 0);
        // Count cap path: limit reached cleanly, byte budget not consulted.
        assert!(!truncated, "count-cap path returns truncated=false");
        assert_eq!(next, None, "count-cap path returns next_offset=null");
    }

    #[test]
    fn symbol_summary_byte_budget_triggers_truncation_with_large_limit() {
        // Same >100-row fixture, but raised `limit` past the count cap so
        // the BYTE budget bites instead. With max_bytes shrunk to ~2KB
        // (budget = 2048 - 512 = 1536) and ~55 bytes per row, only ~25
        // rows fit before the budget rejects the next one. limit=1000
        // ensures the count cap never bites first.
        //
        // This is the load-bearing complement to the previous test: it
        // pins the truncation half of that regression. Without
        // pagination wired through, the stub would have emitted all
        // 1200 rows and overflowed any client harness's response budget.
        let g = locked(multi_namespace_graph(1200));

        let r = get_symbol_summary(&g, None, Some(1000), None, false, 2048);
        let (rows, total, offset, limit) = page_parts(&r);
        let (truncated, next) = page_extras(&r);

        assert_eq!(total, 1200);
        assert_eq!(offset, 0);
        assert_eq!(limit, 1000);
        assert!(
            rows.len() < 1000,
            "byte budget must trim well below the count cap; got {}",
            rows.len()
        );
        assert!(truncated, "byte budget must set truncated=true");
        let next_offset = next.expect("truncated pages must carry next_offset");
        assert_eq!(
            next_offset as usize,
            rows.len(),
            "next_offset must equal the count of records emitted in this page",
        );
    }

    // --- <global> rename tests --------------------------------------------
    //
    // The empty namespace renders as the literal string `<global>` when
    // building each `SummaryRow` — a
    // row-build-time substitution that does NOT mutate `Symbol.namespace`
    // in the graph. The asymmetry is load-bearing: display label is
    // `<global>`, but `search_symbols(namespace="")` still filters by the
    // empty string. Two tests pin each side of that asymmetry.

    /// Build a graph with both a global-scope symbol (namespace = `""`) and
    /// a namespaced symbol (namespace = `"bar"`). The fixture is the
    /// minimal shape that exercises the rename without dragging in the
    /// other `small_graph` rows; the two tests below share it.
    fn global_and_namespaced_graph() -> Graph {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/lib.rs".to_string(),
            language: Language::Rust,
            symbols: vec![
                Symbol {
                    name: "foo".to_string(),
                    kind: SymbolKind::Function,
                    file: "/lib.rs".to_string(),
                    line: 1,
                    column: 0,
                    end_line: 1,
                    signature: "fn foo()".to_string(),
                    namespace: String::new(),
                    parent: String::new(),
                    language: Language::Rust,
                },
                Symbol {
                    name: "inside_bar".to_string(),
                    kind: SymbolKind::Function,
                    file: "/lib.rs".to_string(),
                    line: 5,
                    column: 0,
                    end_line: 5,
                    signature: "fn inside_bar()".to_string(),
                    namespace: "bar".to_string(),
                    parent: String::new(),
                    language: Language::Rust,
                },
            ],
            edges: Vec::new(),
        });
        g
    }

    #[test]
    fn summary_renames_empty_namespace_to_global() {
        // Indexes a fixture with both a global-scope symbol (namespace = "")
        // and a namespaced symbol (namespace = "bar"). Asserts:
        //   * a row exists with namespace="<global>" and the correct count,
        //   * a row exists with namespace="bar" and the correct count,
        //   * no row carries the bare empty string (rename is complete).
        let g = locked(global_and_namespaced_graph());

        let r = get_symbol_summary(&g, None, None, None, false, NO_BYTE_BUDGET);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let results = parsed["results"].as_array().expect("results array");

        // Two distinct (namespace, kind) pairs: ("<global>", "function")
        // for `foo`, and ("bar", "function") for `inside_bar`.
        assert_eq!(parsed["total"].as_u64().unwrap(), 2);
        assert_eq!(pick_count(results, "<global>", "function"), 1);
        assert_eq!(pick_count(results, "bar", "function"), 1);

        // No row carries the bare empty string — the rename is complete,
        // not a "both versions emitted" half-rename.
        assert!(
            !has_row(results, "", "function"),
            "empty-namespace rows must be renamed, not duplicated",
        );

        // Sort order: namespace asc, then kind asc. `<global>` (ASCII '<'
        // = 60) sorts before `bar` (ASCII 'b' = 98), so the global row
        // comes first. Pinning this order locks in the documented
        // "rename happens BEFORE the sort" contract; if the sort moved
        // ahead of the rename, the ordering would flip ("" sorts before
        // "bar"  - same direction by accident — so check the actual
        // strings, not just the relative order).
        let got: Vec<(String, String)> = results
            .iter()
            .map(|row| {
                (
                    row["namespace"].as_str().unwrap().to_string(),
                    row["kind"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("<global>".to_string(), "function".to_string()),
                ("bar".to_string(), "function".to_string()),
            ],
            "rows must sort after the rename, with `<global>` carrying the literal display label",
        );
    }

    #[test]
    fn search_symbols_with_empty_namespace_filter_still_finds_globals_after_summary() {
        // Pins the asymmetry: the `<global>` rename is DISPLAY-ONLY (it
        // happens only in `get_symbol_summary`'s row build). The graph's
        // `Symbol.namespace` stays empty, and `search_symbols` still
        // filters global-scope symbols via `namespace=""` (or, equivalently,
        // by leaving the filter empty — which in this handler means "no
        // namespace filter"). Running the summary first demonstrates the
        // call does not mutate the underlying graph state.
        let g = locked(global_and_namespaced_graph());

        // Run the summary first; assert the `<global>` rename surfaces.
        let r_summary = get_symbol_summary(&g, None, None, None, false, NO_BYTE_BUDGET);
        let parsed_summary: serde_json::Value =
            serde_json::from_str(&body_text(&r_summary)).unwrap();
        let summary_results = parsed_summary["results"].as_array().expect("results array");
        assert!(
            has_row(summary_results, "<global>", "function"),
            "summary must render the global-scope symbol under `<global>`",
        );

        // Now query the graph for the global-scope symbol via search.
        // `search_symbols` treats namespace="" as "no namespace filter"
        // (see `Graph::search` — empty `lower_ns` short-circuits the
        // contains check); the query must therefore return `foo`. The
        // load-bearing assertion is that NO `<global>` literal appears in
        // the search response's namespace field — confirming the rename
        // never touched the graph state, only the summary's row builder.
        let r_search = search_symbols(
            &g,
            SearchSymbolsInput {
                subtree: None,
                query: Some("foo"),
                namespace: Some(""),
                brief: true,
                ..SearchSymbolsInput::default()
            },
            NO_BYTE_BUDGET,
        );
        assert!(r_search.is_error.is_none() || r_search.is_error == Some(false));
        let parsed_search: serde_json::Value = serde_json::from_str(&body_text(&r_search)).unwrap();
        let search_results = parsed_search["results"].as_array().expect("results array");
        let names: Vec<&str> = search_results
            .iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect();
        assert!(
            names.contains(&"foo"),
            "search_symbols must still find the global-scope symbol; got names={names:?}",
        );

        // The graph's Symbol.namespace is empty for `foo`; the search
        // response either omits the namespace field (skip_serializing_if
        // on empty String — see `SymbolResult`) or carries `""`. EITHER
        // way, it must NOT carry the `<global>` display literal — the
        // rename is scoped to `get_symbol_summary`.
        for row in search_results {
            if let Some(ns) = row.get("namespace").and_then(|v| v.as_str()) {
                assert_ne!(
                    ns, "<global>",
                    "search_symbols must never surface the `<global>` display label; \
                     that rename is scoped to get_symbol_summary",
                );
            }
        }
    }

    // --- count_only invariants ----------------------------------------------
    //
    // `get_symbol_summary` takes a `count_only: bool` argument. When true,
    // the handler returns the sentinel envelope (`results: []`,
    // `limit: 0`, `offset: 0`, `truncated: false`, `next_offset: None`)
    // and reports `total` as the count of distinct `(namespace, kind)`
    // pairs — i.e. the number of rows the paginated path would emit, NOT
    // the sum of per-pair symbol counts. The pair-vs-symbol distinction
    // is load-bearing; `symbol_summary_count_only_does_not_count_individual_symbols`
    // pins it against the worst-case shape (many symbols, one pair).
    //
    // The sentinel shape mirrors the `get_orphans` / `search_symbols` /
    // `get_file_symbols` count_only paths, so a single client
    // deserializer covers all four tools.

    /// Build a graph with N Function symbols all in namespace `"foo"`. The
    /// inner map under `summary["foo"]` therefore has exactly ONE entry
    /// `(SymbolKind::Function -> N)`, so the row count is 1 regardless of
    /// N. Used by the load-bearing
    /// `count_only_does_not_count_individual_symbols` test below.
    fn many_symbols_one_pair_graph(n: usize) -> Graph {
        let mut g = Graph::new();
        let mut symbols = Vec::with_capacity(n);
        for i in 0..n {
            symbols.push(Symbol {
                name: format!("func_{i:04}"),
                kind: SymbolKind::Function,
                file: "/foo.cpp".to_string(),
                line: (i as u32) + 1,
                column: 0,
                end_line: (i as u32) + 1,
                signature: String::new(),
                namespace: "foo".to_string(),
                parent: String::new(),
                language: Language::Cpp,
            });
        }
        g.merge_file_graph(FileGraph {
            path: "/foo.cpp".to_string(),
            language: Language::Cpp,
            symbols,
            edges: Vec::new(),
        });
        g
    }

    /// Build a graph with exactly 5 distinct `(namespace, kind)` pairs.
    /// Used by the sentinel-shape test to pin `total = 5` against a
    /// fixture whose row count is known by construction.
    fn five_pair_graph() -> Graph {
        let mut g = Graph::new();
        // (ns="alpha", kind=Function)
        // (ns="alpha", kind=Class)
        // (ns="beta", kind=Function)
        // (ns="beta", kind=Method) — `Method` requires a parent
        // (ns="", kind=Function) — surfaces as `<global>` in row output
        //   but still contributes one `(namespace, kind)` pair to total.
        let symbols = vec![
            Symbol {
                name: "a_fn".to_string(),
                kind: SymbolKind::Function,
                file: "/p.cpp".to_string(),
                line: 1,
                column: 0,
                end_line: 1,
                signature: String::new(),
                namespace: "alpha".to_string(),
                parent: String::new(),
                language: Language::Cpp,
            },
            Symbol {
                name: "AClass".to_string(),
                kind: SymbolKind::Class,
                file: "/p.cpp".to_string(),
                line: 2,
                column: 0,
                end_line: 2,
                signature: String::new(),
                namespace: "alpha".to_string(),
                parent: String::new(),
                language: Language::Cpp,
            },
            Symbol {
                name: "b_fn".to_string(),
                kind: SymbolKind::Function,
                file: "/p.cpp".to_string(),
                line: 3,
                column: 0,
                end_line: 3,
                signature: String::new(),
                namespace: "beta".to_string(),
                parent: String::new(),
                language: Language::Cpp,
            },
            Symbol {
                name: "b_method".to_string(),
                kind: SymbolKind::Method,
                file: "/p.cpp".to_string(),
                line: 4,
                column: 0,
                end_line: 4,
                signature: String::new(),
                namespace: "beta".to_string(),
                parent: "AClass".to_string(),
                language: Language::Cpp,
            },
            Symbol {
                name: "g_fn".to_string(),
                kind: SymbolKind::Function,
                file: "/p.cpp".to_string(),
                line: 5,
                column: 0,
                end_line: 5,
                signature: String::new(),
                namespace: String::new(),
                parent: String::new(),
                language: Language::Cpp,
            },
        ];
        g.merge_file_graph(FileGraph {
            path: "/p.cpp".to_string(),
            language: Language::Cpp,
            symbols,
            edges: Vec::new(),
        });
        g
    }

    #[test]
    fn symbol_summary_count_only_returns_row_count() {
        // When count_only=true the handler MUST emit the sentinel envelope
        // exactly: `results: []`, `total = <distinct (ns, kind) pair count>`,
        // `offset: 0`, `limit: 0`, `truncated: false`, `next_offset: null`.
        // The fixture's 5 pairs are constructed by hand so this test fails
        // loudly if either (a) the row-count math drifts, or (b) the
        // sentinel shape regresses on any field.
        let g = locked(five_pair_graph());

        let r = get_symbol_summary(&g, None, None, None, true, NO_BYTE_BUDGET);

        let body = body_text(&r);
        // count_only must produce a sub-1KB response regardless of input
        // scale — pins the same contract as get_orphans / search_symbols /
        // get_file_symbols.
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
        assert_eq!(
            total, 5,
            "total must equal the (namespace, kind) pair count"
        );
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
    fn symbol_summary_count_only_total_matches_paginated_total() {
        // The count_only short-circuit MUST agree with the paginated path
        // on `total`. The paginated path's `total` is the post-flatten
        // pre-pagination row count (`rows.len()` after the nested-loop
        // build); the count_only path computes the same number via
        // `summary.values().map(|m| m.len()).sum()`. This test pins that
        // the two formulas remain in lockstep — if one ever drifts (e.g.
        // someone adds a filter to the flatten loop but forgets to mirror
        // it in count_only), the assertion fails.
        let g = locked(five_pair_graph());

        let r_count = get_symbol_summary(&g, None, None, None, true, NO_BYTE_BUDGET);
        let parsed_count: serde_json::Value = serde_json::from_str(&body_text(&r_count)).unwrap();
        let total_count = parsed_count["total"].as_u64().unwrap();

        let r_page = get_symbol_summary(&g, None, None, None, false, NO_BYTE_BUDGET);
        let (_results, total_page, _offset, _limit) = page_parts(&r_page);

        assert_eq!(
            total_count, total_page as u64,
            "count_only total must equal paginated total for the same query",
        );
    }

    #[test]
    fn symbol_summary_count_only_does_not_count_individual_symbols() {
        // Load-bearing semantic distinction from the verification field:
        // `total` is the row count (count of distinct `(namespace, kind)`
        // pairs), NOT the sum of symbol counts. A fixture with 50 symbols
        // all `(namespace="foo", kind=Function)` collapses to ONE row of
        // `{namespace: "foo", kind: "function", count: 50}`, so
        // `count_only=true` must report `total = 1`.
        //
        // If a future refactor accidentally returns the symbol sum (50)
        // instead of the pair count (1), this test fails loudly. That's
        // the specific regression the verification field is asking us to
        // pin — the formula and the wording (`m.len()`, NOT `m.values().sum()`)
        // are easy to swap by mistake.
        let g = locked(many_symbols_one_pair_graph(50));

        let r = get_symbol_summary(&g, None, None, None, true, NO_BYTE_BUDGET);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let total = parsed["total"].as_u64().unwrap();

        assert_eq!(
            total, 1,
            "count_only total counts (namespace, kind) PAIRS, not individual symbols; \
             50 symbols all in (foo, Function) collapse to 1 row -> total=1",
        );

        // Sanity: the paginated path agrees with the (count_only=false)
        // semantics — one row, count=50. This isn't the assertion that
        // protects against the symbol-sum bug (that's the count_only check
        // above), but it pins the per-row `count` field stays correct.
        let r_page = get_symbol_summary(&g, None, None, None, false, NO_BYTE_BUDGET);
        let (results, total_page, _offset, _limit) = page_parts(&r_page);
        assert_eq!(total_page, 1, "paginated total must also be 1 row");
        assert_eq!(results.len(), 1, "exactly one row materialized");
        assert_eq!(
            results[0]["count"].as_u64().unwrap(),
            50,
            "the single row's `count` field is the per-pair symbol count (50)",
        );
    }
}
