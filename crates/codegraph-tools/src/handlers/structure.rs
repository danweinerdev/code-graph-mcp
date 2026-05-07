//! Structural-analysis handlers: `detect_cycles`, `get_orphans`,
//! `get_class_hierarchy`, `get_coupling`, `generate_diagram`.
//!
//! Mirrors the Go reference at `internal/tools/structure.go` for shape
//! and JSON output. Error wording follows the Phase 3.4 carry-forward
//! principle: Rust idioms (e.g. listing valid values inline) over rote
//! Go parity. Specific divergences are documented inline so future
//! readers understand which strings are deliberate.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use codegraph_core::{symbol_id, SymbolKind};
use codegraph_graph::{DiagramEdge, Graph};
use parking_lot::RwLock;
use rmcp::model::{CallToolResult, Content};

use super::{
    parse_kind, suggest_symbols, symbol_to_result, tool_error, tool_success_json, Page,
    SymbolResult,
};

// ----- detect_cycles -----

/// `detect_cycles` body. No params. Returns the SCCs (size > 1) of the
/// include graph as a JSON array of arrays of file path strings. An
/// empty result serializes as `[]` (never `null`) — the Vec wrapper
/// guarantees this without extra coercion.
pub fn detect_cycles(graph: &RwLock<Graph>) -> CallToolResult {
    let cycles: Vec<Vec<PathBuf>> = graph.read().detect_cycles();
    // Convert PathBuf -> String for stable JSON output. PathBuf serializes
    // through serde as `String` on Unix, but going through to_string_lossy
    // makes the conversion explicit and is robust on platforms whose
    // OsStr is not UTF-8 (Windows).
    let stringified: Vec<Vec<String>> = cycles
        .into_iter()
        .map(|cycle| {
            cycle
                .into_iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect()
        })
        .collect();
    tool_success_json(&stringified)
}

// ----- get_orphans -----

/// `get_orphans` body. `kind = None` defaults to callables (Function and
/// Method). `kind = Some("class")` etc. parses through [`parse_kind`].
/// Unknown kind strings return `"invalid kind: <kind>"` in line with
/// `search_symbols`.
///
/// Output is the shared [`Page`]`<`[`SymbolResult`]`>` envelope — the full
/// match set is collected from `Graph::orphans`, sorted by `symbol_id`
/// ascending for stable pagination across calls, then sliced by the
/// resolved offset/limit. `total` reports the pre-pagination match count
/// so clients can render "page X of Y" UIs.
///
/// Defaults: `limit = 20`, `offset = 0`, `brief = true`. `limit = 0`
/// means "use the default" (mirrors `search_symbols`); `limit` is
/// silently clamped at 1000. `offset >= total` returns an empty `results`
/// page with the correct `total`.
pub fn get_orphans(
    graph: &RwLock<Graph>,
    kind: Option<&str>,
    limit: Option<u32>,
    offset: Option<u32>,
    brief: Option<bool>,
) -> CallToolResult {
    let parsed_kind: Option<SymbolKind> =
        match kind.and_then(|s| if s.is_empty() { None } else { Some(s) }) {
            None => None,
            Some(s) => match parse_kind(s) {
                Some(k) => Some(k),
                None => return tool_error(format!("invalid kind: {s}")),
            },
        };

    // Resolve defaults: zero-or-missing limit -> 20; clamp at 1000.
    let resolved_limit = limit.filter(|&n| n != 0).unwrap_or(20).min(1000);
    let resolved_offset = offset.unwrap_or(0);
    let resolved_brief = brief.unwrap_or(true);

    let mut matches = graph.read().orphans(parsed_kind);
    let total = matches.len() as u32;

    // Sort by symbol_id ascending so page 1 + page 2 partition the result
    // deterministically across calls. Graph::orphans walks a HashMap and
    // returns symbols in non-deterministic order; symbol_id is unique by
    // construction, so this canonicalizes the sequence without needing
    // tie-break rules.
    matches.sort_by_key(symbol_id);

    // Bounds-safe slice: skip+take never panics on out-of-range offsets,
    // unlike direct indexing.
    let results: Vec<SymbolResult> = matches
        .iter()
        .skip(resolved_offset as usize)
        .take(resolved_limit as usize)
        .map(|s| symbol_to_result(s, resolved_brief))
        .collect();

    let response = Page::<SymbolResult> {
        results,
        total,
        offset: resolved_offset,
        limit: resolved_limit,
    };
    tool_success_json(&response)
}

// ----- get_class_hierarchy -----

/// `get_class_hierarchy` body. Required `class` string; optional `depth`
/// (default 1). Unknown class produces a did-you-mean message filtered
/// to class-like kinds (`Class`, `Struct`, `Interface`, `Trait`).
///
/// The did-you-mean wording mirrors the symbol_detail / callers
/// patterns in 3.4: `class not found: "<name>". Did you mean: a, b, c?`
/// when suggestions exist; otherwise just `class not found: "<name>"`.
pub fn get_class_hierarchy(
    graph: &RwLock<Graph>,
    class: &str,
    depth: Option<u32>,
) -> CallToolResult {
    if class.is_empty() {
        return tool_error("'class' is required");
    }

    let depth = depth.filter(|&d| d > 0).unwrap_or(1);

    let g = graph.read();
    if let Some(h) = g.class_hierarchy(class, depth) {
        return tool_success_json(&h);
    }
    let class_like = suggest_class_symbols(&g, class, 5);
    drop(g);

    if class_like.is_empty() {
        tool_error(format!("class not found: {class:?}"))
    } else {
        let suggestions = class_like.join(", ");
        tool_error(format!(
            "class not found: {class:?}. Did you mean: {suggestions}?"
        ))
    }
}

/// Did-you-mean helper for class-like lookups. Filters the candidate pool
/// to `{Class, Struct, Interface, Trait}` so a Function named "FooBar"
/// never appears as a suggestion for `class_hierarchy("Foo")`. Deliberately
/// does NOT reuse `suggest_symbols` from `mod.rs` because that helper is
/// kind-agnostic.
fn suggest_class_symbols(graph: &Graph, name: &str, limit: usize) -> Vec<String> {
    graph
        .search_symbols(name, None)
        .into_iter()
        .filter(|s| {
            matches!(
                s.kind,
                SymbolKind::Class | SymbolKind::Struct | SymbolKind::Interface | SymbolKind::Trait
            )
        })
        .take(limit)
        .map(|s| s.name)
        .collect()
}

// ----- get_coupling -----

/// `get_coupling` body. Required `file` string; optional `direction` in
/// `{outgoing(default), incoming, both}`.
///
/// Unknown direction returns
/// `"invalid direction: <direction>. Expected one of: outgoing, incoming, both"`
/// — this is a deliberate divergence from the Go wording
/// `"'direction' must be 'incoming', 'outgoing', or 'both'"`. The Rust
/// form matches the `invalid kind: <kind>` and `invalid format: <fmt>`
/// shapes used elsewhere in the handler suite, and includes the bad
/// value verbatim so users can self-correct.
pub fn get_coupling(graph: &RwLock<Graph>, file: &str, direction: Option<&str>) -> CallToolResult {
    if file.is_empty() {
        return tool_error("'file' is required");
    }

    let direction = direction.unwrap_or("");
    let direction = if direction.is_empty() {
        "outgoing"
    } else {
        direction
    };

    let path = Path::new(file);
    let counts: HashMap<PathBuf, u32> = match direction {
        "outgoing" => graph.read().coupling(path),
        "incoming" => graph.read().incoming_coupling(path),
        "both" => {
            let g = graph.read();
            let outgoing = g.coupling(path);
            let incoming = g.incoming_coupling(path);
            drop(g);
            let mut merged: HashMap<PathBuf, u32> =
                HashMap::with_capacity(outgoing.len() + incoming.len());
            for (k, v) in outgoing {
                merged.insert(k, v);
            }
            for (k, v) in incoming {
                *merged.entry(k).or_insert(0) += v;
            }
            merged
        }
        other => {
            return tool_error(format!(
                "invalid direction: {other}. Expected one of: outgoing, incoming, both"
            ));
        }
    };

    // Stringify keys for stable JSON output (PathBuf serializes through
    // OsStr, which on Windows can be a non-UTF-8 surrogate). Mirrors
    // the same pattern used in `detect_cycles`.
    let stringified: HashMap<String, u32> = counts
        .into_iter()
        .map(|(k, v)| (k.to_string_lossy().into_owned(), v))
        .collect();
    tool_success_json(&stringified)
}

// ----- generate_diagram -----

/// Inputs to [`generate_diagram`]. Bundled into a struct so the handler
/// signature stays under clippy's `too_many_arguments` threshold without
/// reaching for an `allow` attribute (same pattern as `SearchSymbolsInput`).
#[derive(Debug, Default)]
pub struct GenerateDiagramInput<'a> {
    pub symbol: Option<&'a str>,
    pub file: Option<&'a str>,
    pub class: Option<&'a str>,
    pub depth: Option<u32>,
    pub max_nodes: Option<u32>,
    pub format: Option<&'a str>,
    pub styled: bool,
}

/// `generate_diagram` body. Dispatches on the exclusive parameter
/// (`symbol` | `file` | `class`) to the matching `Graph::diagram_*`
/// method, then formats the result as either JSON edges or a Mermaid
/// flowchart.
///
/// **Direction**: hardcoded to `"TD"` for all three diagram types. The
/// Go reference uses `"BT"` for inheritance and `"TD"` otherwise; the
/// Rust port unifies on `"TD"` per the task brief. This is a Rust-idiom
/// divergence — having a single direction makes diagrams visually
/// consistent regardless of which view a user requested. The snapshot
/// suite in 3.7 will lock this in.
///
/// **Exactly-one-of**: when 0 or >1 of `symbol`/`file`/`class` are set,
/// returns an error. The Go reference accepted multiple parameters and
/// silently picked one by precedence (class > symbol > file); the Rust
/// port rejects ambiguous calls so silent precedence ambiguity can't
/// produce surprising results.
///
/// Empty edges in `edges` format serialize as `[]` (never `null`) —
/// `DiagramResult::edges` is a `Vec`, not `Option`, so this falls out
/// of the type system.
pub fn generate_diagram(graph: &RwLock<Graph>, input: GenerateDiagramInput<'_>) -> CallToolResult {
    // Exactly-one-of validation. Empty strings count as absent so a
    // client passing `{"symbol": ""}` doesn't pass the check.
    let symbol = input.symbol.filter(|s| !s.is_empty());
    let file = input.file.filter(|s| !s.is_empty());
    let class = input.class.filter(|s| !s.is_empty());
    let count =
        usize::from(symbol.is_some()) + usize::from(file.is_some()) + usize::from(class.is_some());
    if count != 1 {
        return tool_error("exactly one of 'symbol', 'file', or 'class' is required");
    }

    let depth = input.depth.filter(|&d| d > 0).unwrap_or(1);
    let max_nodes = input.max_nodes.filter(|&m| m > 0).unwrap_or(30);
    let format = input.format.unwrap_or("");
    let format = if format.is_empty() { "edges" } else { format };

    // Validate format up front so an invalid format with valid dispatch
    // params still produces the format error (not a not-found from the
    // graph lookup).
    if format != "edges" && format != "mermaid" {
        return tool_error(format!(
            "invalid format: {format}. Expected 'edges' or 'mermaid'"
        ));
    }

    let g = graph.read();
    let dr_opt = if let Some(id) = symbol {
        g.diagram_call_graph(id, depth, max_nodes)
    } else if let Some(path) = file {
        g.diagram_file_graph(Path::new(path), depth, max_nodes)
    } else if let Some(name) = class {
        g.diagram_inheritance(name, depth, max_nodes)
    } else {
        // Unreachable: the exactly-one-of check above guarantees one is
        // Some. `unreachable!()` documents the invariant; if a future
        // edit weakens the check, the panic surfaces in tests.
        unreachable!("exactly-one-of validation guarantees one branch is taken");
    };

    let dr = match dr_opt {
        Some(d) => d,
        None => {
            // Did-you-mean for symbol/class on miss; bare not-found
            // for file (no useful suggestion source for filenames).
            if let Some(id) = symbol {
                let suggestions = suggest_symbols(&g, id, 5);
                drop(g);
                return if suggestions.is_empty() {
                    tool_error(format!("symbol not found: {id:?}"))
                } else {
                    tool_error(format!(
                        "symbol not found: {id:?}. Did you mean: {suggestions}?"
                    ))
                };
            }
            if let Some(name) = class {
                let class_like = suggest_class_symbols(&g, name, 5);
                drop(g);
                return if class_like.is_empty() {
                    tool_error(format!("class not found: {name:?}"))
                } else {
                    let suggestions = class_like.join(", ");
                    tool_error(format!(
                        "class not found: {name:?}. Did you mean: {suggestions}?"
                    ))
                };
            }
            // file branch: no did-you-mean.
            let path = file.expect("exactly-one-of guarantees file is Some on this branch");
            drop(g);
            return tool_error(format!("file not found: {path:?}"));
        }
    };
    drop(g);

    match format {
        "edges" => {
            // DiagramResult.edges is already Vec<DiagramEdge>; serialize directly.
            let edges: &Vec<DiagramEdge> = &dr.edges;
            tool_success_json(edges)
        }
        "mermaid" => {
            // Hardcode "TD" — see fn-level doc comment for rationale.
            let rendered = dr.render_mermaid("TD", input.styled);
            CallToolResult::success(vec![Content::text(rendered)])
        }
        _ => unreachable!("format validation rejects everything else above"),
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{body_text, page_parts};
    use super::*;
    use codegraph_core::{Edge, EdgeKind, FileGraph, Language, Symbol, SymbolKind};

    fn sym(name: &str, kind: SymbolKind, file: &str) -> Symbol {
        sym_full(name, kind, file, "")
    }

    fn sym_full(name: &str, kind: SymbolKind, file: &str, parent: &str) -> Symbol {
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

    fn call_edge(from: &str, to: &str, file: &str) -> Edge {
        Edge {
            from: from.to_string(),
            to: to.to_string(),
            kind: EdgeKind::Calls,
            file: file.to_string(),
            line: 1,
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

    fn inherit_edge(from: &str, to: &str, file: &str) -> Edge {
        Edge {
            from: from.to_string(),
            to: to.to_string(),
            kind: EdgeKind::Inherits,
            file: file.to_string(),
            line: 0,
        }
    }

    fn locked(g: Graph) -> RwLock<Graph> {
        RwLock::new(g)
    }

    // --- detect_cycles ---

    #[test]
    fn detect_cycles_empty_graph_returns_empty_array() {
        let g = locked(Graph::new());
        let r = detect_cycles(&g);
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        // Empty Vec must serialize as `[]`, never `null`.
        assert_eq!(body_text(&r), "[]");
    }

    #[test]
    fn detect_cycles_acyclic_graph_returns_empty_array() {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/a.h".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![include_edge("/a.h", "/b.h")],
        });
        g.merge_file_graph(FileGraph {
            path: "/b.h".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![],
        });
        let g = locked(g);
        let r = detect_cycles(&g);
        assert_eq!(body_text(&r), "[]");
    }

    #[test]
    fn detect_cycles_two_node_cycle_returns_array_of_paths() {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/a.h".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![include_edge("/a.h", "/b.h")],
        });
        g.merge_file_graph(FileGraph {
            path: "/b.h".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![include_edge("/b.h", "/a.h")],
        });
        let g = locked(g);
        let r = detect_cycles(&g);
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1, "exactly one cycle");
        let cycle = arr[0].as_array().unwrap();
        assert_eq!(cycle.len(), 2);
        let mut names: Vec<&str> = cycle.iter().map(|v| v.as_str().unwrap()).collect();
        names.sort();
        assert_eq!(names, vec!["/a.h", "/b.h"]);
    }

    // --- get_orphans ---

    fn graph_with_orphans() -> Graph {
        // foo calls bar; baz is uncalled (orphan); cls is a class with no callers.
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("foo", SymbolKind::Function, "/x.cpp"),
                sym("bar", SymbolKind::Function, "/x.cpp"),
                sym("baz", SymbolKind::Function, "/x.cpp"),
                sym("cls", SymbolKind::Class, "/x.cpp"),
            ],
            edges: vec![call_edge("/x.cpp:foo", "/x.cpp:bar", "/x.cpp")],
        });
        g
    }

    #[test]
    fn orphans_default_returns_callables() {
        let g = locked(graph_with_orphans());
        let r = get_orphans(&g, None, None, None, None);
        let (arr, total, offset, limit) = page_parts(&r);
        // foo and baz have no callers; bar is called by foo. cls is a Class
        // and is excluded by the default callable-only filter.
        let names: Vec<&str> = arr.iter().map(|e| e["name"].as_str().unwrap()).collect();
        assert_eq!(arr.len(), 2, "got {names:?}");
        assert!(names.contains(&"foo"));
        assert!(names.contains(&"baz"));
        assert!(!names.contains(&"bar"));
        assert!(!names.contains(&"cls"));
        assert_eq!(total, 2);
        assert_eq!(offset, 0);
        assert_eq!(limit, 20);
    }

    #[test]
    fn orphans_kind_class_returns_only_classes() {
        let g = locked(graph_with_orphans());
        let r = get_orphans(&g, Some("class"), None, None, None);
        let (arr, total, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], serde_json::json!("cls"));
        assert_eq!(arr[0]["kind"], serde_json::json!("class"));
        assert_eq!(total, 1);
    }

    #[test]
    fn orphans_invalid_kind_errors() {
        let g = locked(Graph::new());
        let r = get_orphans(&g, Some("widget"), None, None, None);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "invalid kind: widget");
    }

    #[test]
    fn orphans_empty_graph_returns_empty_envelope() {
        let g = locked(Graph::new());
        let r = get_orphans(&g, None, None, None, None);
        let (arr, total, offset, limit) = page_parts(&r);
        assert!(arr.is_empty());
        assert_eq!(total, 0);
        assert_eq!(offset, 0);
        assert_eq!(limit, 20);
    }

    #[test]
    fn orphans_empty_string_kind_treated_as_default() {
        // A client passing `kind=""` should behave the same as omitting
        // kind — Go's `req.GetArguments()["kind"].(string)` ignores empty
        // strings via the `&& k != ""` check.
        let g = locked(graph_with_orphans());
        let r = get_orphans(&g, Some(""), None, None, None);
        let (arr, _, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 2, "empty kind => default callables-only");
    }

    #[test]
    fn orphans_brief_mode_omits_signature() {
        // Output is brief by default — assert signature is dropped from
        // the serialized form even though our test fixture has a non-empty
        // signature on each symbol.
        let g = locked(graph_with_orphans());
        let r = get_orphans(&g, None, None, None, None);
        let (arr, _, _, _) = page_parts(&r);
        for entry in arr {
            assert!(
                entry.get("signature").is_none(),
                "brief output must omit signature: {entry:?}",
            );
        }
    }

    // --- Phase 2 pagination invariants ------------------------------------

    /// Build a graph with exactly `n` orphan functions named `func_000`,
    /// `func_001`, ..., zero-padded to 3 digits so the natural sort order
    /// (`symbol_id` ascending) is predictable for assertions. All symbols
    /// live in `/big.cpp` so the symbol_id format is `[/big.cpp:func_000`,
    /// `/big.cpp:func_001`, ...]`.
    fn graph_with_n_orphan_functions(n: usize) -> Graph {
        let mut g = Graph::new();
        let mut symbols: Vec<Symbol> = Vec::with_capacity(n);
        for i in 0..n {
            symbols.push(sym(
                &format!("func_{i:03}"),
                SymbolKind::Function,
                "/big.cpp",
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
    fn orphans_default_limit_is_20() {
        // 25 orphans: default limit (20) returns the first 20; total = 25.
        let g = locked(graph_with_n_orphan_functions(25));
        let r = get_orphans(&g, None, None, None, None);
        let (arr, total, offset, limit) = page_parts(&r);
        assert_eq!(arr.len(), 20);
        assert_eq!(total, 25);
        assert_eq!(offset, 0);
        assert_eq!(limit, 20);
    }

    #[test]
    fn orphans_page_1_and_page_2_cover_full_set() {
        // 30 orphans: page 1 (offset=0, limit=20) ∪ page 2 (offset=20, limit=20)
        // covers all 30 with no overlap.
        let g = locked(graph_with_n_orphan_functions(30));

        let p1 = get_orphans(&g, None, Some(20), Some(0), None);
        let (a1, t1, _, _) = page_parts(&p1);
        let p2 = get_orphans(&g, None, Some(20), Some(20), None);
        let (a2, t2, _, _) = page_parts(&p2);

        assert_eq!(a1.len(), 20);
        assert_eq!(a2.len(), 10);
        assert_eq!(t1, 30);
        assert_eq!(t2, 30);

        // Union covers all 30, no duplicates.
        let mut ids: Vec<String> = a1
            .iter()
            .chain(a2.iter())
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 30, "page1 ∪ page2 must cover all 30 with no dup");
    }

    #[test]
    fn orphans_total_is_pre_pagination_count() {
        // Same fixture, three different pages — total is identical across all.
        let g = locked(graph_with_n_orphan_functions(30));
        let r1 = get_orphans(&g, None, Some(20), Some(0), None);
        let r2 = get_orphans(&g, None, Some(20), Some(20), None);
        let r3 = get_orphans(&g, None, Some(5), Some(10), None);
        let (_, t1, _, _) = page_parts(&r1);
        let (_, t2, _, _) = page_parts(&r2);
        let (_, t3, _, _) = page_parts(&r3);
        assert_eq!(t1, 30);
        assert_eq!(t2, 30);
        assert_eq!(t3, 30);
    }

    #[test]
    fn orphans_limit_clamps_at_1000() {
        // limit = 999_999 silently clamps to 1000; the response echoes the
        // clamped value so the agent sees what was actually used. The
        // 5-item fixture also verifies all 5 results return — confirming
        // take(1000) doesn't accidentally drop entries on a small set.
        let g = locked(graph_with_n_orphan_functions(5));
        let r = get_orphans(&g, None, Some(999_999), None, None);
        let (arr, _, _, limit) = page_parts(&r);
        assert_eq!(limit, 1000);
        assert_eq!(arr.len(), 5);
    }

    #[test]
    fn orphans_zero_limit_uses_default() {
        // limit = 0 is treated as "unset"; resolves to default 20.
        let g = locked(graph_with_n_orphan_functions(5));
        let r = get_orphans(&g, None, Some(0), None, None);
        let (_, _, _, limit) = page_parts(&r);
        assert_eq!(limit, 20);
    }

    #[test]
    fn orphans_offset_beyond_total_returns_empty() {
        // offset >= total returns empty results with the correct total.
        let g = locked(graph_with_orphans());
        let r = get_orphans(&g, None, None, Some(999), None);
        let (arr, total, offset, limit) = page_parts(&r);
        assert!(arr.is_empty());
        assert_eq!(total, 2);
        assert_eq!(offset, 999);
        assert_eq!(limit, 20);
    }

    #[test]
    fn orphans_kind_filter_combined_with_pagination() {
        // Mixed-kind fixture: 12 class orphans + 5 function orphans. With
        // kind="class" and limit=10, we get 10 class entries (all "class"
        // kind) and total=12.
        let mut g = Graph::new();
        let mut symbols: Vec<Symbol> = Vec::new();
        for i in 0..12 {
            symbols.push(sym(&format!("Class_{i:03}"), SymbolKind::Class, "/m.cpp"));
        }
        for i in 0..5 {
            symbols.push(sym(&format!("func_{i:03}"), SymbolKind::Function, "/m.cpp"));
        }
        g.merge_file_graph(FileGraph {
            path: "/m.cpp".to_string(),
            language: Language::Cpp,
            symbols,
            edges: vec![],
        });
        let g = locked(g);
        let r = get_orphans(&g, Some("class"), Some(10), None, None);
        let (arr, total, _, _) = page_parts(&r);
        assert_eq!(arr.len(), 10);
        assert_eq!(total, 12);
        for entry in &arr {
            assert_eq!(entry["kind"], serde_json::json!("class"));
        }
    }

    #[test]
    fn orphans_brief_false_includes_signature() {
        // brief=false surfaces signature/column/end_line on each row.
        let g = locked(graph_with_orphans());
        let r = get_orphans(&g, None, None, None, Some(false));
        let (arr, _, _, _) = page_parts(&r);
        assert!(!arr.is_empty());
        for entry in &arr {
            assert!(
                entry.get("signature").is_some(),
                "brief=false must include signature: {entry:?}",
            );
        }
    }

    // --- get_class_hierarchy ---

    fn class_graph() -> Graph {
        // Base <- Mid <- Leaf chain.
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/cls.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("Base", SymbolKind::Class, "/cls.cpp"),
                sym("Mid", SymbolKind::Class, "/cls.cpp"),
                sym("Leaf", SymbolKind::Class, "/cls.cpp"),
                sym(
                    "looks_like_a_class_but_isnt",
                    SymbolKind::Function,
                    "/cls.cpp",
                ),
            ],
            edges: vec![
                inherit_edge("Mid", "Base", "/cls.cpp"),
                inherit_edge("Leaf", "Mid", "/cls.cpp"),
            ],
        });
        g
    }

    #[test]
    fn class_hierarchy_missing_class_param_errors() {
        let g = locked(Graph::new());
        let r = get_class_hierarchy(&g, "", None);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'class' is required");
    }

    #[test]
    fn class_hierarchy_returns_node_tree() {
        let g = locked(class_graph());
        let r = get_class_hierarchy(&g, "Mid", Some(1));
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        assert_eq!(parsed["name"], serde_json::json!("Mid"));
        let bases = parsed["bases"].as_array().unwrap();
        assert_eq!(bases.len(), 1);
        assert_eq!(bases[0]["name"], serde_json::json!("Base"));
        let derived = parsed["derived"].as_array().unwrap();
        assert_eq!(derived.len(), 1);
        assert_eq!(derived[0]["name"], serde_json::json!("Leaf"));
    }

    #[test]
    fn class_hierarchy_unknown_with_no_suggestions() {
        let g = locked(Graph::new());
        let r = get_class_hierarchy(&g, "Nope", None);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "class not found: \"Nope\"");
    }

    #[test]
    fn class_hierarchy_unknown_with_suggestions_filters_to_class_like() {
        // "B" is a substring of "Base" (Class) and of nothing else. The
        // function `looks_like_a_class_but_isnt` does not contain "B".
        let g = locked(class_graph());
        let r = get_class_hierarchy(&g, "B", None);
        assert_eq!(r.is_error, Some(true));
        let text = body_text(&r);
        assert!(text.starts_with("class not found: \"B\""), "got: {text}");
        assert!(text.contains("Base"), "got: {text}");
        assert!(text.contains("Did you mean: "), "got: {text}");
    }

    #[test]
    fn class_hierarchy_function_kind_not_suggested() {
        // "looks_like_a_class_but_isnt" has SymbolKind::Function. A query
        // that matches it via substring should NOT receive a function as
        // a "class did you mean" suggestion. (Confirmed via separate text
        // assertion to make the divergence from suggest_symbols visible.)
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym(
                "looks_like_a_class_but_isnt",
                SymbolKind::Function,
                "/x.cpp",
            )],
            edges: vec![],
        });
        let g = locked(g);
        let r = get_class_hierarchy(&g, "looks", None);
        assert_eq!(r.is_error, Some(true));
        let text = body_text(&r);
        // No class-like candidates → bare not-found.
        assert_eq!(text, "class not found: \"looks\"");
    }

    #[test]
    fn class_hierarchy_depth_zero_normalized_to_one() {
        // A None depth and a Some(0) both become 1.
        let g = locked(class_graph());
        let with_zero = get_class_hierarchy(&g, "Mid", Some(0));
        let with_none = get_class_hierarchy(&g, "Mid", None);
        assert_eq!(body_text(&with_zero), body_text(&with_none));
    }

    // --- get_coupling ---

    fn coupling_graph() -> Graph {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/a.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("caller", SymbolKind::Function, "/a.cpp")],
            edges: vec![
                call_edge("/a.cpp:caller", "/b.cpp:target", "/a.cpp"),
                include_edge("/a.cpp", "/b.cpp"),
            ],
        });
        g.merge_file_graph(FileGraph {
            path: "/b.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("target", SymbolKind::Function, "/b.cpp")],
            edges: vec![],
        });
        g
    }

    #[test]
    fn coupling_missing_file_param_errors() {
        let g = locked(Graph::new());
        let r = get_coupling(&g, "", None);
        assert_eq!(r.is_error, Some(true));
        assert_eq!(body_text(&r), "'file' is required");
    }

    #[test]
    fn coupling_outgoing_default() {
        let g = locked(coupling_graph());
        let r = get_coupling(&g, "/a.cpp", None);
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let obj = parsed.as_object().unwrap();
        // 1 call + 1 include into /b.cpp.
        assert_eq!(obj["/b.cpp"], serde_json::json!(2));
    }

    #[test]
    fn coupling_incoming_returns_callers_and_includers() {
        let g = locked(coupling_graph());
        let r = get_coupling(&g, "/b.cpp", Some("incoming"));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let obj = parsed.as_object().unwrap();
        assert_eq!(obj["/a.cpp"], serde_json::json!(2));
    }

    #[test]
    fn coupling_both_merges_outgoing_and_incoming() {
        // Set up a graph where /a.cpp has 1 outgoing call to /b.cpp and
        // /c.cpp includes /a.cpp incoming. "both" must surface both keys.
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/a.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("caller", SymbolKind::Function, "/a.cpp")],
            edges: vec![call_edge("/a.cpp:caller", "/b.cpp:target", "/a.cpp")],
        });
        g.merge_file_graph(FileGraph {
            path: "/b.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("target", SymbolKind::Function, "/b.cpp")],
            edges: vec![],
        });
        g.merge_file_graph(FileGraph {
            path: "/c.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![include_edge("/c.cpp", "/a.cpp")],
        });
        let g = locked(g);
        let r = get_coupling(&g, "/a.cpp", Some("both"));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let obj = parsed.as_object().unwrap();
        // /b.cpp from outgoing (1 call), /c.cpp from incoming (1 include).
        assert_eq!(obj["/b.cpp"], serde_json::json!(1));
        assert_eq!(obj["/c.cpp"], serde_json::json!(1));
    }

    #[test]
    fn coupling_invalid_direction_errors() {
        let g = locked(Graph::new());
        let r = get_coupling(&g, "/a.cpp", Some("sideways"));
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            body_text(&r),
            "invalid direction: sideways. Expected one of: outgoing, incoming, both"
        );
    }

    #[test]
    fn coupling_unknown_file_returns_empty_object() {
        let g = locked(Graph::new());
        let r = get_coupling(&g, "/never.cpp", None);
        assert_eq!(body_text(&r), "{}");
    }

    // --- generate_diagram ---

    fn diagram_graph() -> Graph {
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("a", SymbolKind::Function, "/x.cpp"),
                sym("b", SymbolKind::Function, "/x.cpp"),
            ],
            edges: vec![call_edge("/x.cpp:a", "/x.cpp:b", "/x.cpp")],
        });
        g.merge_file_graph(FileGraph {
            path: "/y.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![],
            edges: vec![include_edge("/y.cpp", "/x.cpp")],
        });
        g.merge_file_graph(FileGraph {
            path: "/cls.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![
                sym("Base", SymbolKind::Class, "/cls.cpp"),
                sym("Derived", SymbolKind::Class, "/cls.cpp"),
            ],
            edges: vec![inherit_edge("Derived", "Base", "/cls.cpp")],
        });
        g
    }

    #[test]
    fn diagram_no_param_errors() {
        let g = locked(Graph::new());
        let r = generate_diagram(&g, GenerateDiagramInput::default());
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            body_text(&r),
            "exactly one of 'symbol', 'file', or 'class' is required"
        );
    }

    #[test]
    fn diagram_two_params_errors() {
        let g = locked(Graph::new());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("/x.cpp:a"),
                file: Some("/x.cpp"),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            body_text(&r),
            "exactly one of 'symbol', 'file', or 'class' is required"
        );
    }

    #[test]
    fn diagram_three_params_errors() {
        let g = locked(Graph::new());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("a"),
                file: Some("/x.cpp"),
                class: Some("Base"),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(r.is_error, Some(true));
    }

    #[test]
    fn diagram_empty_strings_treated_as_absent() {
        // Three empty strings count as 0 set parameters.
        let g = locked(Graph::new());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some(""),
                file: Some(""),
                class: Some(""),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            body_text(&r),
            "exactly one of 'symbol', 'file', or 'class' is required"
        );
    }

    #[test]
    fn diagram_symbol_edges_format() {
        let g = locked(diagram_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("/x.cpp:a"),
                ..GenerateDiagramInput::default()
            },
        );
        assert!(r.is_error.is_none() || r.is_error == Some(false));
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["from"], serde_json::json!("a"));
        assert_eq!(arr[0]["to"], serde_json::json!("b"));
        assert_eq!(arr[0]["label"], serde_json::json!("calls"));
    }

    #[test]
    fn diagram_file_edges_format() {
        let g = locked(diagram_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                file: Some("/x.cpp"),
                ..GenerateDiagramInput::default()
            },
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let arr = parsed.as_array().unwrap();
        // /y.cpp -> /x.cpp via include, found via reverse-include scan.
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["label"], serde_json::json!("includes"));
    }

    #[test]
    fn diagram_class_edges_format() {
        let g = locked(diagram_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                class: Some("Base"),
                ..GenerateDiagramInput::default()
            },
        );
        let parsed: serde_json::Value = serde_json::from_str(&body_text(&r)).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["from"], serde_json::json!("Derived"));
        assert_eq!(arr[0]["to"], serde_json::json!("Base"));
        assert_eq!(arr[0]["label"], serde_json::json!("inherits"));
    }

    #[test]
    fn diagram_mermaid_format() {
        let g = locked(diagram_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("/x.cpp:a"),
                format: Some("mermaid"),
                ..GenerateDiagramInput::default()
            },
        );
        let text = body_text(&r);
        assert!(text.starts_with("graph TD\n"), "got: {text}");
        assert!(text.contains("calls"), "must include label: {text}");
    }

    #[test]
    fn diagram_mermaid_styled_marks_center() {
        let g = locked(diagram_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("/x.cpp:a"),
                format: Some("mermaid"),
                styled: true,
                ..GenerateDiagramInput::default()
            },
        );
        let text = body_text(&r);
        assert!(text.contains(":::center"), "styled must tag center: {text}");
        assert!(text.contains("classDef center"), "got: {text}");
    }

    #[test]
    fn diagram_invalid_format_errors() {
        let g = locked(diagram_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("/x.cpp:a"),
                format: Some("svg"),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(r.is_error, Some(true));
        assert_eq!(
            body_text(&r),
            "invalid format: svg. Expected 'edges' or 'mermaid'"
        );
    }

    #[test]
    fn diagram_unknown_symbol_did_you_mean() {
        let g = locked(diagram_graph());
        // "a" is a substring of `/x.cpp:a` — should suggest.
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                symbol: Some("a"),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(r.is_error, Some(true));
        let text = body_text(&r);
        assert!(text.starts_with("symbol not found: \"a\""), "got: {text}");
        assert!(text.contains("Did you mean: "), "got: {text}");
    }

    #[test]
    fn diagram_unknown_file_no_did_you_mean() {
        let g = locked(diagram_graph());
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                file: Some("/never.cpp"),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(r.is_error, Some(true));
        // No did-you-mean for files.
        assert_eq!(body_text(&r), "file not found: \"/never.cpp\"");
    }

    #[test]
    fn diagram_unknown_class_with_suggestion() {
        let g = locked(diagram_graph());
        // "B" → "Base" (Class).
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                class: Some("B"),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(r.is_error, Some(true));
        let text = body_text(&r);
        assert!(text.starts_with("class not found: \"B\""), "got: {text}");
        assert!(text.contains("Base"), "got: {text}");
    }

    #[test]
    fn diagram_empty_edges_serializes_as_array() {
        // Class with no inheritance edges → empty Vec<DiagramEdge> → "[]".
        let mut g = Graph::new();
        g.merge_file_graph(FileGraph {
            path: "/x.cpp".to_string(),
            language: Language::Cpp,
            symbols: vec![sym("Solo", SymbolKind::Class, "/x.cpp")],
            edges: vec![],
        });
        let g = locked(g);
        let r = generate_diagram(
            &g,
            GenerateDiagramInput {
                class: Some("Solo"),
                ..GenerateDiagramInput::default()
            },
        );
        assert_eq!(body_text(&r), "[]");
    }
}
