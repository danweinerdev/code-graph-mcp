//! Symbol-level read queries: per-file listing, single-symbol lookup,
//! filtered/paginated search, and the namespace-grouped summary.
//!
//! Mirrors the Go reference at `internal/graph/graph.go` lines 177–320
//! (`FileSymbols`, `SymbolDetail`, `Search`, `SearchSymbols`, `SymbolSummary`)
//! with one new feature: the optional `language` filter on [`SearchParams`],
//! the first consumer of the `Language` field carried on every [`Symbol`].
//!
//! All methods take `&self` — querying never mutates state. Pagination
//! correctness (stable order under repeat queries, `total` reported before
//! slicing) follows the Go semantics exactly so existing MCP clients see
//! identical envelopes.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::path::Path;

use code_graph_core::{symbol_id, Language, Symbol, SymbolKind};
use regex::Regex;

use crate::Graph;

// Test-only counter incremented on every `BinaryHeap::push` call inside
// `Graph::search`. Used by the heap-not-touched test to pin the cost win
// of `count_only=true` to observable behavior: a future refactor that
// accidentally re-introduces heap construction on the count-only path
// would make `search_count_only_does_not_push_heap` fail immediately.
// Reset between measurements via `reset_heap_pushes`; read via
// `heap_pushes`. Hidden behind `#[cfg(test)]` so production builds carry
// no overhead.
//
// **Thread-local on purpose:** cargo test runs `#[test]`s on a thread pool
// by default, and many other tests in this module also exercise
// `Graph::search`. A process-global atomic would race across parallel
// tests, contaminating the count. A `thread_local!` counter is observed
// only by the test on the current thread, so the heap-not-touched test
// can run in parallel with the rest of the suite without serialization.
//
// Plain `//` rather than `///` because `thread_local!` is a macro
// invocation and rustdoc emits `unused_doc_comments` on the inner item.
#[cfg(test)]
thread_local! {
    static HEAP_PUSHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Reset the heap-push counter to zero before a measurement. Test-only.
#[cfg(test)]
pub(crate) fn reset_heap_pushes() {
    HEAP_PUSHES.with(|c| c.set(0));
}

/// Read the current value of the heap-push counter. Test-only.
#[cfg(test)]
pub(crate) fn heap_pushes() -> usize {
    HEAP_PUSHES.with(|c| c.get())
}

/// Increment the counter by 1. Test-only.
#[cfg(test)]
#[inline]
fn bump_heap_pushes() {
    HEAP_PUSHES.with(|c| c.set(c.get().saturating_add(1)));
}

/// Heap entry for the bounded-top-N search algorithm. Keyed by `id`
/// (the precomputed `symbol_id`), with a borrowed `Symbol` reference so
/// the heap doesn't clone every match. The Ord impl makes
/// `BinaryHeap<TopEntry>` a max-heap by `id` — pushing a smaller-id
/// entry after eviction of the current max converges on the N
/// smallest-id matches.
struct TopEntry<'a> {
    id: String,
    sym: &'a Symbol,
}

impl PartialEq for TopEntry<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for TopEntry<'_> {}

impl PartialOrd for TopEntry<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TopEntry<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.id.cmp(&other.id)
    }
}

/// Filters and paging for [`Graph::search`].
///
/// Construct via `Default::default()` and override the fields you care about.
/// Empty `pattern`/`namespace` and `None` `kind`/`language` mean "no filter".
/// `limit == 0` is normalized to the default of 20 so callers that forget to
/// set it still get a bounded response (matches Go's behavior for `Limit <= 0`).
#[derive(Debug, Clone, Default)]
pub struct SearchParams {
    /// Regex (case-insensitive). Falls back to a case-insensitive substring
    /// match if the pattern fails to compile as a regex.
    pub pattern: String,
    /// Exact-match kind filter. `None` keeps all kinds.
    pub kind: Option<SymbolKind>,
    /// Case-insensitive substring filter on `Symbol::namespace`. Empty = off.
    pub namespace: String,
    /// Exact-match language filter. `None` keeps all languages. The Go
    /// reference has no equivalent because the Go binary only ships a
    /// C++ parser.
    pub language: Option<Language>,
    /// Page size. `0` is treated as the default (20).
    pub limit: u32,
    /// Skip the first N matches. Bounded to `total` so out-of-range offsets
    /// return an empty page rather than panicking.
    pub offset: u32,
    /// When `true`, [`Graph::search`] short-circuits before the
    /// `BinaryHeap<TopEntry>` push/pop loop: it walks the match predicate to
    /// compute the exact `total` count, then returns
    /// `SearchResult { symbols: vec![], total }` without ever allocating the
    /// heap or cloning a Symbol. The wire-format envelope is unchanged; the
    /// optimization is internal. `Default` is `false`, so existing call sites
    /// that construct via `..SearchParams::default()` keep their behavior.
    /// Threaded into the search-symbols count-only path (the exception path
    /// for search-symbols-specific count-only — orphans / file_symbols
    /// compute `total` directly from their filtered Vec, no heap involved).
    pub count_only: bool,
}

/// Paginated search response.
///
/// `symbols` is the post-offset/limit slice; `total` is the count *before*
/// pagination so callers can render "page X of Y" UIs without re-querying.
/// Both fields are always present (empty `Vec` and `0` for an empty graph)
/// so JSON serialization never produces `null`.
#[derive(Debug, Clone, Default)]
pub struct SearchResult {
    pub symbols: Vec<Symbol>,
    pub total: u32,
}

impl Graph {
    /// All symbols defined in `path`, in the order they were inserted at
    /// merge time. Returns an empty `Vec` for unknown paths so JSON
    /// serialization yields `[]`, never `null`.
    pub fn file_symbols(&self, path: &Path) -> Vec<Symbol> {
        let Some(entry) = self.files.get(path) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(entry.symbol_ids.len());
        for id in &entry.symbol_ids {
            if let Some(node) = self.nodes.get(id) {
                out.push(node.symbol.clone());
            }
        }
        out
    }

    /// Cloned [`Symbol`] for `id`, or `None` if no such symbol exists. The
    /// MCP handler is responsible for translating `None` into a did-you-mean
    /// suggestion via [`Graph::search_symbols`].
    pub fn symbol_detail(&self, id: &str) -> Option<Symbol> {
        self.nodes.get(id).map(|n| n.symbol.clone())
    }

    /// Filtered, paginated symbol search.
    ///
    /// Filtering order matches the Go reference: kind → language →
    /// namespace → pattern. The pattern is tried as a case-insensitive
    /// regex (`(?i)<pattern>`); compile failures fall back to a
    /// case-insensitive substring match so user-supplied input never
    /// crashes the search. Results are sorted by [`symbol_id`] for
    /// deterministic pagination across repeat queries.
    ///
    /// **Memory + time complexity (per the PaginationOverhaul retro):**
    /// The original implementation cloned every match into a `Vec<Symbol>`
    /// then sorted the full set — O(M) memory and O(M log M) time for any
    /// query where M is the match count. On UE-scale codebases (M ≈ 500k),
    /// a broad query like `kind=function` allocated 500k Symbol clones
    /// just to return 20 rows. The current implementation maintains a
    /// bounded max-heap of size `(offset + limit)` keyed by [`symbol_id`],
    /// keeping only the N lexicographically-smallest IDs seen. `total`
    /// stays exact (incremented on every match). Memory drops to
    /// O(offset + limit); time drops to O(M log(offset + limit)). For
    /// typical pagination (offset=0, limit=20) on M=500k, this is ~25,000×
    /// less memory and ~5× fewer comparisons than the previous algorithm
    /// — and `total` is still exact, unlike an early-exit approach.
    pub fn search(&self, mut params: SearchParams) -> SearchResult {
        // Pre-compute lowercase forms once outside the hot loop. The regex
        // is only built when pattern is non-empty; substring fallback uses
        // `lower_pattern` whether or not the regex compiled (cheap to keep
        // around even if unused).
        let (re, lower_pattern) = if params.pattern.is_empty() {
            (None, String::new())
        } else {
            let compiled = Regex::new(&format!("(?i){}", params.pattern)).ok();
            (compiled, params.pattern.to_lowercase())
        };
        let lower_ns = params.namespace.to_lowercase();

        // count_only short-circuit: walk the match predicate exactly as
        // the materializing path does,
        // but skip the BinaryHeap allocation, the per-match `symbol_id`
        // formatting, and the Vec<Symbol> clone at the end. The returned
        // `total` is byte-identical to the materializing path's total because
        // both branches share the same matchers; only the per-match work
        // diverges. `params.offset` is intentionally ignored — count_only
        // callers opted out of paging, and `total` is the pre-pagination
        // match count by definition.
        if params.count_only {
            let mut total: u32 = 0;
            for node in self.nodes.values() {
                let s = &node.symbol;

                if let Some(k) = params.kind {
                    if s.kind != k {
                        continue;
                    }
                }

                if let Some(l) = params.language {
                    if s.language != l {
                        continue;
                    }
                }

                if !lower_ns.is_empty() && !s.namespace.to_lowercase().contains(&lower_ns) {
                    continue;
                }

                if !params.pattern.is_empty() {
                    let full_name = if s.parent.is_empty() {
                        s.name.clone()
                    } else {
                        format!("{}::{}", s.parent, s.name)
                    };
                    let matched = match &re {
                        Some(r) => r.is_match(&full_name),
                        None => full_name.to_lowercase().contains(&lower_pattern),
                    };
                    if !matched {
                        continue;
                    }
                }

                total = total.saturating_add(1);
            }
            return SearchResult {
                symbols: Vec::new(),
                total,
            };
        }

        // Resolve `limit = 0` to the default 20 on the materializing path.
        // The count_only branch above returns before this line, so it never
        // sees the default — count_only callers pass `limit = 0` as a
        // "don't care" sentinel and `params.limit` stays 0 if they inspect
        // it after the call.
        if params.limit == 0 {
            params.limit = 20;
        }

        // Bounded max-heap: holds at most (offset + limit) entries, the N
        // lexicographically-smallest symbol IDs seen so far. Eviction
        // happens when a new match's ID is smaller than the current max
        // (the heap's root). Initial capacity is bounded so an obscene
        // offset doesn't pre-allocate gigabytes; the heap grows as needed.
        let cap = params.offset.saturating_add(params.limit) as usize;
        let mut top: BinaryHeap<TopEntry<'_>> = BinaryHeap::with_capacity(cap.min(1024));
        let mut total: u32 = 0;

        for node in self.nodes.values() {
            let s = &node.symbol;

            if let Some(k) = params.kind {
                if s.kind != k {
                    continue;
                }
            }

            if let Some(l) = params.language {
                if s.language != l {
                    continue;
                }
            }

            if !lower_ns.is_empty() && !s.namespace.to_lowercase().contains(&lower_ns) {
                continue;
            }

            if !params.pattern.is_empty() {
                let full_name = if s.parent.is_empty() {
                    s.name.clone()
                } else {
                    format!("{}::{}", s.parent, s.name)
                };
                let matched = match &re {
                    Some(r) => r.is_match(&full_name),
                    None => full_name.to_lowercase().contains(&lower_pattern),
                };
                if !matched {
                    continue;
                }
            }

            // This match counts toward `total` regardless of whether it
            // makes it into the top-N heap. The exact total is what the
            // pagination envelope's `total` field surfaces.
            total = total.saturating_add(1);
            if cap == 0 {
                continue;
            }
            let id = symbol_id(s);
            if top.len() < cap {
                top.push(TopEntry { id, sym: s });
                #[cfg(test)]
                bump_heap_pushes();
            } else if let Some(top_max) = top.peek() {
                if id < top_max.id {
                    top.pop();
                    top.push(TopEntry { id, sym: s });
                    #[cfg(test)]
                    bump_heap_pushes();
                }
            }
        }

        // `into_sorted_vec` consumes the heap and returns entries in
        // ascending order by `Ord::cmp` (i.e. ascending by `id`). This is
        // the canonical pagination order — same as the previous algorithm's
        // `sort_by_key(symbol_id)` would have produced. Slicing [offset..]
        // drops the first `offset` items so the returned page contains the
        // [offset..offset+limit) slice of the full sorted set.
        let items = top.into_sorted_vec();
        let start = (params.offset as usize).min(items.len());
        let symbols: Vec<Symbol> = items[start..].iter().map(|e| e.sym.clone()).collect();

        SearchResult { symbols, total }
    }

    /// Legacy convenience wrapper used by the did-you-mean suggester. Returns
    /// up to 100 candidate symbols matching `pattern` (and optional `kind`),
    /// not the default 20, so the suggester has a wide enough pool to rank
    /// against. Carry-forward from the LLMOptimization debrief —
    /// `suggest_symbols` was previously capping at 20 and missing close
    /// matches that ranked just outside the first page.
    pub fn search_symbols(&self, pattern: &str, kind: Option<SymbolKind>) -> Vec<Symbol> {
        let params = SearchParams {
            pattern: pattern.to_string(),
            kind,
            limit: 100,
            ..SearchParams::default()
        };
        self.search(params).symbols
    }

    /// Symbol counts grouped by `(namespace, kind)`. With `file = None` the
    /// summary spans the whole graph; with `file = Some(path)` it is scoped
    /// to symbols whose `file` field equals `path`. Empty graphs (or scoped
    /// queries that match nothing) return an empty `HashMap`, never `None`.
    ///
    /// Non-UTF-8 paths cannot be matched against `Symbol::file` (which is a
    /// `String`), so a non-UTF-8 `file` argument yields an empty summary
    /// rather than panicking on the conversion.
    pub fn symbol_summary(&self, file: Option<&Path>) -> HashMap<String, HashMap<SymbolKind, u32>> {
        let file_str = match file {
            Some(p) => match p.to_str() {
                Some(s) => Some(s),
                // Non-UTF-8 path: nothing in `Symbol::file` (a `String`) can
                // possibly equal it, so short-circuit to an empty summary.
                None => return HashMap::new(),
            },
            None => None,
        };

        let mut summary: HashMap<String, HashMap<SymbolKind, u32>> = HashMap::new();
        for node in self.nodes.values() {
            let s = &node.symbol;
            if let Some(f) = file_str {
                if s.file != f {
                    continue;
                }
            }
            *summary
                .entry(s.namespace.clone())
                .or_default()
                .entry(s.kind)
                .or_insert(0) += 1;
        }
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::{make_fg, sym, sym_full};
    use std::path::PathBuf;

    // --- file_symbols ---

    #[test]
    fn file_symbols_known_path() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("foo", SymbolKind::Function, "/a.cpp"),
                sym("bar", SymbolKind::Function, "/a.cpp"),
            ],
            vec![],
        ));

        let out = g.file_symbols(&PathBuf::from("/a.cpp"));
        assert_eq!(out.len(), 2);
        let names: Vec<&str> = out.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"bar"));
    }

    #[test]
    fn file_symbols_unknown_path_returns_empty() {
        let g = Graph::new();
        let out = g.file_symbols(&PathBuf::from("/never-merged.cpp"));
        // Must be a Vec (not Option), so JSON serializes as `[]` not `null`.
        assert!(out.is_empty());
    }

    // --- symbol_detail ---

    #[test]
    fn symbol_detail_known_id() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("foo", SymbolKind::Function, "/a.cpp")],
            vec![],
        ));
        let detail = g.symbol_detail("/a.cpp:foo");
        assert!(detail.is_some());
        let s = detail.unwrap();
        assert_eq!(s.name, "foo");
        assert_eq!(s.file, "/a.cpp");
    }

    #[test]
    fn symbol_detail_unknown_id() {
        let g = Graph::new();
        assert!(g.symbol_detail("/missing.cpp:nope").is_none());
    }

    // --- search: pattern matching ---

    #[test]
    fn search_regex_match() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("foo", SymbolKind::Function, "/a.cpp"),
                sym("foobar", SymbolKind::Function, "/a.cpp"),
                sym("xfoo", SymbolKind::Function, "/a.cpp"),
            ],
            vec![],
        ));

        // Anchored pattern: matches names starting with `foo`.
        let result = g.search(SearchParams {
            pattern: "^foo".to_string(),
            ..SearchParams::default()
        });
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(result.total, 2);
        assert_eq!(result.symbols.len(), 2);
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"foobar"));
        assert!(!names.contains(&"xfoo"));

        // Exact-anchor: matches `foo` only, not `foobar`.
        let exact = g.search(SearchParams {
            pattern: "^foo$".to_string(),
            ..SearchParams::default()
        });
        let exact_names: Vec<&str> = exact.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(exact.total, 1);
        assert_eq!(exact_names, vec!["foo"]);
    }

    #[test]
    fn search_substring_fallback_when_regex_invalid() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("FooHandler", SymbolKind::Function, "/a.cpp"),
                sym("BarFooBaz", SymbolKind::Function, "/a.cpp"),
                sym("Unrelated", SymbolKind::Function, "/a.cpp"),
            ],
            vec![],
        ));

        // `*` is invalid regex (nothing to repeat). Must fall back to
        // substring instead of panicking — and substring search for `*`
        // won't find any of these names.
        let bad = g.search(SearchParams {
            pattern: "*".to_string(),
            ..SearchParams::default()
        });
        assert_eq!(bad.total, 0);
        assert!(bad.symbols.is_empty());

        // Sanity-check the substring path actually works (case-insensitive).
        let foo = g.search(SearchParams {
            pattern: "foo".to_string(),
            ..SearchParams::default()
        });
        assert_eq!(foo.total, 2);
        let names: Vec<&str> = foo.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"FooHandler"));
        assert!(names.contains(&"BarFooBaz"));

        // Substring-fallback success: pattern is invalid as regex (`[Handler`
        // is unclosed) but lowercased contains `[handler` — wait, `[handler`
        // appears in no name. Use `[oo` which is invalid regex but the
        // lowercase substring `[oo` won't match either. Pick a pattern that
        // is invalid regex AND a real substring of one of our names: `[`
        // alone — invalid regex, and the names contain no `[`. Workaround:
        // build a name that contains `[` literally so the substring branch
        // visibly succeeds.
        let mut g2 = Graph::new();
        g2.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![
                sym("foo[bracket]", SymbolKind::Function, "/b.cpp"),
                sym("plain", SymbolKind::Function, "/b.cpp"),
            ],
            vec![],
        ));
        // `[bracket` is invalid regex (unclosed character class) — must fall
        // back to substring, where it matches `foo[bracket]` and not `plain`.
        let bracket = g2.search(SearchParams {
            pattern: "[bracket".to_string(),
            ..SearchParams::default()
        });
        assert_eq!(bracket.total, 1);
        assert_eq!(bracket.symbols[0].name, "foo[bracket]");
    }

    // --- search: filters ---

    #[test]
    fn search_kind_filter() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("foo", SymbolKind::Function, "/a.cpp"),
                sym("Bar", SymbolKind::Class, "/a.cpp"),
                sym("baz", SymbolKind::Function, "/a.cpp"),
            ],
            vec![],
        ));

        let result = g.search(SearchParams {
            kind: Some(SymbolKind::Function),
            ..SearchParams::default()
        });
        assert_eq!(result.total, 2);
        for s in &result.symbols {
            assert_eq!(s.kind, SymbolKind::Function);
        }
    }

    #[test]
    fn search_namespace_filter() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym_full(
                    "a",
                    SymbolKind::Function,
                    "/a.cpp",
                    "acme::sub",
                    "",
                    Language::Cpp,
                ),
                sym_full(
                    "b",
                    SymbolKind::Function,
                    "/a.cpp",
                    "AcmeUtil",
                    "",
                    Language::Cpp,
                ),
                sym_full(
                    "c",
                    SymbolKind::Function,
                    "/a.cpp",
                    "other",
                    "",
                    Language::Cpp,
                ),
                sym_full("d", SymbolKind::Function, "/a.cpp", "", "", Language::Cpp),
            ],
            vec![],
        ));

        // Case-insensitive substring: "Acme" matches "acme::sub" and "AcmeUtil".
        let result = g.search(SearchParams {
            namespace: "Acme".to_string(),
            ..SearchParams::default()
        });
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(result.total, 2);
        assert!(names.contains(&"a"));
        assert!(names.contains(&"b"));
    }

    #[test]
    fn search_language_filter() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym_full(
                    "cpp_one",
                    SymbolKind::Function,
                    "/a.cpp",
                    "",
                    "",
                    Language::Cpp,
                ),
                sym_full(
                    "rust_one",
                    SymbolKind::Function,
                    "/a.cpp",
                    "",
                    "",
                    Language::Rust,
                ),
                sym_full(
                    "rust_two",
                    SymbolKind::Function,
                    "/a.cpp",
                    "",
                    "",
                    Language::Rust,
                ),
            ],
            vec![],
        ));

        let rust_only = g.search(SearchParams {
            language: Some(Language::Rust),
            ..SearchParams::default()
        });
        assert_eq!(rust_only.total, 2);
        for s in &rust_only.symbols {
            assert_eq!(s.language, Language::Rust);
        }

        let cpp_only = g.search(SearchParams {
            language: Some(Language::Cpp),
            ..SearchParams::default()
        });
        assert_eq!(cpp_only.total, 1);
        assert_eq!(cpp_only.symbols[0].name, "cpp_one");
    }

    #[test]
    fn search_all_filters_combined() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                // Wanted: kind=Function, language=Rust, namespace contains
                // "core", name starts with "process_".
                sym_full(
                    "process_one",
                    SymbolKind::Function,
                    "/a.cpp",
                    "core::pipeline",
                    "",
                    Language::Rust,
                ),
                // Wrong language.
                sym_full(
                    "process_two",
                    SymbolKind::Function,
                    "/a.cpp",
                    "core::pipeline",
                    "",
                    Language::Cpp,
                ),
                // Wrong namespace.
                sym_full(
                    "process_three",
                    SymbolKind::Function,
                    "/a.cpp",
                    "other",
                    "",
                    Language::Rust,
                ),
                // Wrong kind.
                sym_full(
                    "process_four",
                    SymbolKind::Class,
                    "/a.cpp",
                    "core",
                    "",
                    Language::Rust,
                ),
                // Wrong name.
                sym_full(
                    "render",
                    SymbolKind::Function,
                    "/a.cpp",
                    "core",
                    "",
                    Language::Rust,
                ),
            ],
            vec![],
        ));

        let result = g.search(SearchParams {
            pattern: "^process_".to_string(),
            kind: Some(SymbolKind::Function),
            namespace: "core".to_string(),
            language: Some(Language::Rust),
            ..SearchParams::default()
        });
        assert_eq!(result.total, 1);
        assert_eq!(result.symbols[0].name, "process_one");
    }

    #[test]
    fn search_empty_pattern_returns_all_passing_other_filters() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("a", SymbolKind::Function, "/a.cpp"),
                sym("b", SymbolKind::Function, "/a.cpp"),
                sym("c", SymbolKind::Class, "/a.cpp"),
            ],
            vec![],
        ));

        // Empty pattern + Function filter → 2 functions.
        let result = g.search(SearchParams {
            kind: Some(SymbolKind::Function),
            ..SearchParams::default()
        });
        assert_eq!(result.total, 2);

        // Empty pattern + no filters → all symbols.
        let all = g.search(SearchParams::default());
        assert_eq!(all.total, 3);
    }

    // --- search: pagination ---

    #[test]
    fn search_pagination_offset_beyond_total() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("a", SymbolKind::Function, "/a.cpp"),
                sym("b", SymbolKind::Function, "/a.cpp"),
            ],
            vec![],
        ));

        let result = g.search(SearchParams {
            offset: 10, // total is 2, so 10 is well past the end.
            ..SearchParams::default()
        });
        assert_eq!(result.total, 2, "total reported pre-pagination");
        assert!(result.symbols.is_empty(), "no rows past the end");
    }

    #[test]
    fn search_pagination_limit_zero_treated_as_default() {
        let mut g = Graph::new();
        // Insert 25 symbols to differentiate "all" from "first 20".
        let symbols: Vec<Symbol> = (0..25)
            .map(|i| sym(&format!("fn_{i:02}"), SymbolKind::Function, "/a.cpp"))
            .collect();
        g.merge_file_graph(make_fg("/a.cpp", Language::Cpp, symbols, vec![]));

        let result = g.search(SearchParams {
            limit: 0,
            ..SearchParams::default()
        });
        assert_eq!(result.total, 25);
        assert_eq!(result.symbols.len(), 20, "limit=0 → default 20");
    }

    #[test]
    fn search_results_sorted_by_symbol_id() {
        let mut g = Graph::new();
        // Inserted in arbitrary order; symbol_id is `path:Name` so they
        // should come back alphabetized by file then name.
        g.merge_file_graph(make_fg(
            "/z.cpp",
            Language::Cpp,
            vec![
                sym("zeta", SymbolKind::Function, "/z.cpp"),
                sym("alpha", SymbolKind::Function, "/z.cpp"),
            ],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("mu", SymbolKind::Function, "/a.cpp"),
                sym("beta", SymbolKind::Function, "/a.cpp"),
            ],
            vec![],
        ));

        let result = g.search(SearchParams::default());
        let ids: Vec<String> = result.symbols.iter().map(symbol_id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
        // Spot-check the expected ordering.
        assert_eq!(ids[0], "/a.cpp:beta");
        assert_eq!(ids[1], "/a.cpp:mu");
        assert_eq!(ids[2], "/z.cpp:alpha");
        assert_eq!(ids[3], "/z.cpp:zeta");
    }

    // --- search: count_only -------------------------------------------------

    /// Build a graph with `n` free functions named `match_NNN` in `/big.cpp`.
    /// Zero-padded so ids sort lexically. All symbols pass a
    /// `pattern = "match"` filter, so the test exercises a path where every
    /// node hits the heap (or the count-only short-circuit's counter).
    fn graph_with_n_matching(n: usize) -> Graph {
        let mut g = Graph::new();
        let symbols: Vec<Symbol> = (0..n)
            .map(|i| sym(&format!("match_{i:03}"), SymbolKind::Function, "/big.cpp"))
            .collect();
        g.merge_file_graph(make_fg("/big.cpp", Language::Cpp, symbols, vec![]));
        g
    }

    #[test]
    fn search_count_only_returns_zero_symbols_and_real_total() {
        // count_only=true: results MUST be empty; total MUST be the
        // pre-pagination match count (NOT zero, NOT capped by limit).
        let g = graph_with_n_matching(50);
        let result = g.search(SearchParams {
            pattern: "match".to_string(),
            count_only: true,
            ..SearchParams::default()
        });
        assert!(
            result.symbols.is_empty(),
            "count_only must emit empty symbols"
        );
        assert_eq!(result.total, 50, "total must reflect true match count");
    }

    #[test]
    fn search_count_only_total_matches_regular_search_total() {
        // Behavioral test: a count_only=true call MUST report
        // the same `total` as a regular call (limit=1, count_only=false)
        // against the same fixture. `total` is the pre-pagination match count
        // and must be independent of the count_only flag.
        let g = graph_with_n_matching(50);
        let count_only = g.search(SearchParams {
            pattern: "match".to_string(),
            count_only: true,
            limit: 0,
            offset: 0,
            ..SearchParams::default()
        });
        let regular = g.search(SearchParams {
            pattern: "match".to_string(),
            count_only: false,
            limit: 1,
            offset: 0,
            ..SearchParams::default()
        });
        assert_eq!(
            count_only.total, regular.total,
            "count_only and regular search must report the same total"
        );
        assert_eq!(count_only.total, 50);
        assert!(count_only.symbols.is_empty());
        assert_eq!(regular.symbols.len(), 1);
    }

    #[test]
    fn search_count_only_does_not_push_heap() {
        // Pins the cost win of count_only=true to observable behavior:
        // `Graph::search` MUST NOT push anything onto the BinaryHeap when
        // count_only is true. Without this guard, a future refactor could
        // re-introduce heap construction on the count-only path and the wire
        // format would still pass — but the perf optimization would be silently
        // lost. HEAP_PUSHES is a thread-local counter (gated `#[cfg(test)]`),
        // so this test runs unmolested in parallel with the rest of the suite
        // — the increment on the test thread is observed only here.
        let g = graph_with_n_matching(100);

        // Step 1: count_only=true MUST push zero entries.
        reset_heap_pushes();
        let count_only = g.search(SearchParams {
            pattern: "match".to_string(),
            count_only: true,
            ..SearchParams::default()
        });
        let pushes_count_only = heap_pushes();
        assert_eq!(count_only.total, 100);
        assert!(count_only.symbols.is_empty());
        assert_eq!(
            pushes_count_only, 0,
            "count_only=true must skip the heap entirely; got {pushes_count_only} pushes"
        );

        // Step 2: count_only=false MUST push at least once. This sentinel
        // catches the inverse failure mode — a refactor that accidentally
        // disables the counter (e.g. removes the `#[cfg(test)]` increment
        // sites) — which would make Step 1's assertion trivially pass.
        reset_heap_pushes();
        let regular = g.search(SearchParams {
            pattern: "match".to_string(),
            count_only: false,
            limit: 10,
            ..SearchParams::default()
        });
        let pushes_regular = heap_pushes();
        assert_eq!(regular.total, 100);
        assert_eq!(regular.symbols.len(), 10);
        assert!(
            pushes_regular > 0,
            "count_only=false must push the heap (sentinel against a no-op counter); got {pushes_regular} pushes"
        );
    }

    // --- search_symbols (legacy wrapper) ---

    #[test]
    fn search_symbols_uses_higher_default_pool() {
        let mut g = Graph::new();
        // 50 matching symbols — search() default would cap at 20, but the
        // legacy wrapper passes limit=100 so the suggester sees them all.
        let symbols: Vec<Symbol> = (0..50)
            .map(|i| sym(&format!("foo_{i:02}"), SymbolKind::Function, "/a.cpp"))
            .collect();
        g.merge_file_graph(make_fg("/a.cpp", Language::Cpp, symbols, vec![]));

        let suggestions = g.search_symbols("foo", None);
        assert_eq!(suggestions.len(), 50);
    }

    // --- symbol_summary ---

    #[test]
    fn symbol_summary_empty_graph() {
        let g = Graph::new();
        let summary = g.symbol_summary(None);
        assert!(summary.is_empty());

        // Scoped query on empty graph also empty.
        let scoped = g.symbol_summary(Some(&PathBuf::from("/nope.cpp")));
        assert!(scoped.is_empty());
    }

    #[test]
    fn symbol_summary_whole_graph() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym_full(
                    "f1",
                    SymbolKind::Function,
                    "/a.cpp",
                    "ns_a",
                    "",
                    Language::Cpp,
                ),
                sym_full(
                    "f2",
                    SymbolKind::Function,
                    "/a.cpp",
                    "ns_a",
                    "",
                    Language::Cpp,
                ),
                sym_full("C1", SymbolKind::Class, "/a.cpp", "ns_a", "", Language::Cpp),
                sym_full(
                    "g1",
                    SymbolKind::Function,
                    "/a.cpp",
                    "ns_b",
                    "",
                    Language::Cpp,
                ),
            ],
            vec![],
        ));

        let summary = g.symbol_summary(None);
        assert_eq!(summary.len(), 2);
        let ns_a = &summary["ns_a"];
        assert_eq!(ns_a[&SymbolKind::Function], 2);
        assert_eq!(ns_a[&SymbolKind::Class], 1);
        let ns_b = &summary["ns_b"];
        assert_eq!(ns_b[&SymbolKind::Function], 1);
    }

    #[test]
    fn symbol_summary_file_scoped() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym_full(
                    "a1",
                    SymbolKind::Function,
                    "/a.cpp",
                    "ns",
                    "",
                    Language::Cpp,
                ),
                sym_full(
                    "a2",
                    SymbolKind::Function,
                    "/a.cpp",
                    "ns",
                    "",
                    Language::Cpp,
                ),
            ],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym_full(
                "b1",
                SymbolKind::Function,
                "/b.cpp",
                "ns",
                "",
                Language::Cpp,
            )],
            vec![],
        ));

        let scoped = g.symbol_summary(Some(&PathBuf::from("/a.cpp")));
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped["ns"][&SymbolKind::Function], 2);

        // Whole-graph view picks up both files.
        let full = g.symbol_summary(None);
        assert_eq!(full["ns"][&SymbolKind::Function], 3);
    }
}
