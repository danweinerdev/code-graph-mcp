//! Symbol-level read queries: per-file listing, single-symbol lookup,
//! filtered/paginated search, and the namespace-grouped summary.
//!
//! Mirrors the Go reference at `internal/graph/graph.go` lines 177–320
//! (`FileSymbols`, `SymbolDetail`, `Search`, `SearchSymbols`, `SymbolSummary`)
//! with one new feature: the optional `language` filter on [`SearchParams`].
//! Phase 1 added `Language` to every [`Symbol`]; this is its first consumer.
//!
//! All methods take `&self` — querying never mutates state. Pagination
//! correctness (stable order under repeat queries, `total` reported before
//! slicing) follows the Go semantics exactly so existing MCP clients see
//! identical envelopes.

use std::cmp::min;
use std::collections::HashMap;
use std::path::Path;

use codegraph_core::{symbol_id, Language, Symbol, SymbolKind};
use regex::Regex;

use crate::Graph;

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
    /// Exact-match language filter. `None` keeps all languages. **New in
    /// Phase 2.2** — Go has no equivalent because the Go binary only ships a
    /// C++ parser.
    pub language: Option<Language>,
    /// Page size. `0` is treated as the default (20).
    pub limit: u32,
    /// Skip the first N matches. Bounded to `total` so out-of-range offsets
    /// return an empty page rather than panicking.
    pub offset: u32,
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
    pub fn search(&self, mut params: SearchParams) -> SearchResult {
        if params.limit == 0 {
            params.limit = 20;
        }

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

        let mut matches: Vec<Symbol> = Vec::new();
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

            matches.push(s.clone());
        }

        // Sort by SymbolID for stable pagination. `sort_by_key` is stable in
        // Rust's std, mirroring Go's `sort.Slice` (which is *not* stable in
        // Go but the SymbolID comparator produces a total order on unique
        // IDs, so stability is moot for the sort key itself).
        matches.sort_by_key(symbol_id);

        let total = matches.len() as u32;
        let start = min(params.offset as usize, matches.len());
        let end = min(start + params.limit as usize, matches.len());

        SearchResult {
            symbols: matches[start..end].to_vec(),
            total,
        }
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
    use codegraph_core::{Edge, FileGraph};
    use std::path::PathBuf;

    fn sym_full(
        name: &str,
        kind: SymbolKind,
        file: &str,
        namespace: &str,
        parent: &str,
        language: Language,
    ) -> Symbol {
        Symbol {
            name: name.to_string(),
            kind,
            file: file.to_string(),
            line: 1,
            column: 0,
            end_line: 1,
            signature: String::new(),
            namespace: namespace.to_string(),
            parent: parent.to_string(),
            language,
        }
    }

    fn sym(name: &str, kind: SymbolKind, file: &str) -> Symbol {
        sym_full(name, kind, file, "", "", Language::Cpp)
    }

    fn make_fg(
        path: &str,
        language: Language,
        symbols: Vec<Symbol>,
        edges: Vec<Edge>,
    ) -> FileGraph {
        FileGraph {
            path: path.to_string(),
            language,
            symbols,
            edges,
        }
    }

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
