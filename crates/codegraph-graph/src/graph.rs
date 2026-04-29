//! Graph storage and the merge / remove / clear mutators.
//!
//! This module mirrors the Go reference at `internal/graph/graph.go` lines
//! 1–175 (`Graph`, `Node`, `EdgeEntry`, `New`, `MergeFileGraph`, `RemoveFile`,
//! `removeFileUnsafe`, `Clear`). The Rust port adds a [`FileEntry`] that
//! records the source [`Language`] alongside the file's symbol IDs so the
//! Phase-3 cache v2 format can persist the language without re-deriving it
//! from the file extension. Locking is **not** introduced here — Task 2.6
//! wraps [`Graph`] behind a `parking_lot::RwLock`.
//!
//! Keys for the file-scoped maps (`files`, `includes`) are `PathBuf` rather
//! than `String` so callers do not have to launder `Path` ↔ `String`
//! conversions at every boundary. Symbol IDs remain `String` (aliased as
//! [`SymbolId`] in `codegraph-core`) because they are arbitrary identifiers,
//! not filesystem paths.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use codegraph_core::{symbol_id, EdgeKind, FileGraph, Language, Symbol, SymbolId};

/// In-memory directed graph of code symbols.
///
/// Storage layout matches the Go reference exactly:
/// - `nodes`: symbol id → [`Node`]
/// - `adj` / `radj`: symbol id → outgoing / incoming `Calls` and `Inherits`
///   edges (the Go binary keeps both directions for cheap callers/callees)
/// - `files`: file path → [`FileEntry`] (language + owned symbol IDs)
/// - `includes`: file path → included file paths (kept separate from
///   `adj`/`radj` because include edges are file-to-file, not symbol-to-symbol)
#[derive(Debug, Default)]
pub struct Graph {
    pub(crate) nodes: HashMap<SymbolId, Node>,
    pub(crate) adj: HashMap<SymbolId, Vec<EdgeEntry>>,
    pub(crate) radj: HashMap<SymbolId, Vec<EdgeEntry>>,
    pub(crate) files: HashMap<PathBuf, FileEntry>,
    pub(crate) includes: HashMap<PathBuf, Vec<PathBuf>>,
}

/// Wrapper around a [`Symbol`] stored in the graph. Mirrors Go's `Node`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Node {
    pub symbol: Symbol,
}

/// One directed edge in either the forward (`adj`) or reverse (`radj`)
/// adjacency list. Mirrors Go's `EdgeEntry`. The `target` is the *other end*
/// of the edge from the map key's perspective: in `adj[from]` it is the
/// destination; in `radj[to]` it is the origin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EdgeEntry {
    pub target: SymbolId,
    pub kind: EdgeKind,
    pub file: PathBuf,
    pub line: u32,
}

/// Per-file metadata recorded at merge time. The Go reference stores only
/// `map[string][]string` (path → symbol IDs); the Rust port also captures
/// the source [`Language`] so cache v2 (Phase 3) can persist it without
/// re-deriving from the path extension.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileEntry {
    pub language: Language,
    pub symbol_ids: Vec<SymbolId>,
}

/// Quick storage-size summary returned by [`Graph::stats`]. The `edges`
/// count includes both adjacency entries (calls + inherits) and include
/// edges, matching the Go binary's `Stats()` semantics.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GraphStats {
    pub nodes: u32,
    pub edges: u32,
    pub files: u32,
}

impl Graph {
    /// Construct an empty graph with all maps initialized. Never panics and
    /// never allocates beyond `HashMap`'s zero-capacity default.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace all symbols and edges from a parsed [`FileGraph`].
    ///
    /// If the path is already known, its previous contents are removed first
    /// (via `remove_file_unsafe`) so re-merging is naturally
    /// idempotent — the post-state depends only on the input `fg`, not on
    /// whether the path was previously merged.
    ///
    /// Edges are routed by kind:
    /// - `Calls` and `Inherits` → both `adj[from]` and `radj[to]`
    /// - `Includes` → `includes[from]` only (file-to-file, not symbol-to-symbol)
    pub fn merge_file_graph(&mut self, fg: FileGraph) {
        let path = PathBuf::from(&fg.path);

        // Remove stale data if file was previously indexed.
        if self.files.contains_key(&path) {
            self.remove_file_unsafe(&path);
        }

        // Add symbols as nodes.
        let mut symbol_ids: Vec<SymbolId> = Vec::with_capacity(fg.symbols.len());
        for symbol in fg.symbols {
            let id = symbol_id(&symbol);
            self.nodes.insert(id.clone(), Node { symbol });
            symbol_ids.push(id);
        }
        self.files.insert(
            path.clone(),
            FileEntry {
                language: fg.language,
                symbol_ids,
            },
        );

        // Add edges, routing by kind.
        for edge in fg.edges {
            let edge_file = PathBuf::from(&edge.file);
            match edge.kind {
                EdgeKind::Calls | EdgeKind::Inherits => {
                    self.adj
                        .entry(edge.from.clone())
                        .or_default()
                        .push(EdgeEntry {
                            target: edge.to.clone(),
                            kind: edge.kind,
                            file: edge_file.clone(),
                            line: edge.line,
                        });
                    self.radj.entry(edge.to).or_default().push(EdgeEntry {
                        target: edge.from,
                        kind: edge.kind,
                        file: edge_file,
                        line: edge.line,
                    });
                }
                EdgeKind::Includes => {
                    self.includes
                        .entry(PathBuf::from(&edge.from))
                        .or_default()
                        .push(PathBuf::from(&edge.to));
                }
                // `EdgeKind` is `#[non_exhaustive]`; future variants are silently
                // ignored here until the routing rule is extended.
                _ => {}
            }
        }
    }

    /// Remove all symbols and edges originating from the given file.
    ///
    /// Cleanup covers all five storage maps:
    /// - `nodes`: every symbol whose ID appears in `files[path].symbol_ids`
    /// - `adj` and `radj`: every entry whose `file` field equals `path`,
    ///   purging keys whose vec becomes empty
    /// - `includes[path]`: removed
    /// - `files[path]`: removed
    ///
    /// Unknown paths are a no-op.
    pub fn remove_file(&mut self, path: &Path) {
        self.remove_file_unsafe(path);
    }

    // "unsafe" here means "caller must hold the write lock once Task 2.6 wraps
    // Graph in parking_lot::RwLock". No Rust `unsafe` code is involved.
    fn remove_file_unsafe(&mut self, path: &Path) {
        // Remove nodes for this file's symbols.
        if let Some(entry) = self.files.get(path) {
            for id in &entry.symbol_ids {
                self.nodes.remove(id);
            }
        }

        // Remove adj/radj entries sourced from this file. Keys whose vec
        // becomes empty are dropped so `adj.len()` reflects active sources.
        Self::retain_edges_not_from(&mut self.adj, path);
        Self::retain_edges_not_from(&mut self.radj, path);

        self.includes.remove(path);
        self.files.remove(path);
    }

    /// Filter edge map entries: drop edges whose `file` equals `path`, and
    /// drop keys whose retained vec is empty.
    fn retain_edges_not_from(map: &mut HashMap<SymbolId, Vec<EdgeEntry>>, path: &Path) {
        map.retain(|_, entries| {
            entries.retain(|e| e.file != path);
            !entries.is_empty()
        });
    }

    /// Reset the graph to empty. All five maps are cleared.
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.adj.clear();
        self.radj.clear();
        self.files.clear();
        self.includes.clear();
    }

    /// Storage-size summary. `edges` sums adjacency entries and include
    /// edges, matching Go's `Stats()` (which counts each include once and
    /// each call/inherit once via the forward `adj` map only — the reverse
    /// `radj` is *not* double-counted).
    pub fn stats(&self) -> GraphStats {
        let adj_edges: usize = self.adj.values().map(Vec::len).sum();
        let include_edges: usize = self.includes.values().map(Vec::len).sum();
        GraphStats {
            nodes: self.nodes.len() as u32,
            edges: (adj_edges + include_edges) as u32,
            files: self.files.len() as u32,
        }
    }

    // ----- Phase 2.1 read accessors used only by the in-module tests.
    // The public query surface arrives in Tasks 2.2–2.5, which will add
    // proper APIs (`file_symbols`, `symbol_detail`, etc.) on top of these
    // private maps. Gated to `cfg(test)` so the dead_code lint stays clean
    // until then.
    #[cfg(test)]
    fn nodes(&self) -> &HashMap<SymbolId, Node> {
        &self.nodes
    }

    #[cfg(test)]
    fn adj(&self) -> &HashMap<SymbolId, Vec<EdgeEntry>> {
        &self.adj
    }

    #[cfg(test)]
    fn radj(&self) -> &HashMap<SymbolId, Vec<EdgeEntry>> {
        &self.radj
    }

    #[cfg(test)]
    fn files(&self) -> &HashMap<PathBuf, FileEntry> {
        &self.files
    }

    #[cfg(test)]
    fn includes(&self) -> &HashMap<PathBuf, Vec<PathBuf>> {
        &self.includes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::{call_edge, include_edge, inherit_edge, make_fg, sym};
    use codegraph_core::SymbolKind;

    #[test]
    fn merge_one_file_adds_nodes_and_edges() {
        let mut g = Graph::new();
        let fg = make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("foo", SymbolKind::Function, "/a.cpp"),
                sym("bar", SymbolKind::Function, "/a.cpp"),
            ],
            vec![
                call_edge("/a.cpp:foo", "/a.cpp:bar", "/a.cpp", 1),
                include_edge("/a.cpp", "/utils.h", "/a.cpp"),
            ],
        );

        g.merge_file_graph(fg);

        let stats = g.stats();
        assert_eq!(stats.nodes, 2);
        assert_eq!(stats.edges, 2, "1 call + 1 include = 2 edges");
        assert_eq!(stats.files, 1);

        assert!(g.nodes().contains_key("/a.cpp:foo"));
        assert!(g.nodes().contains_key("/a.cpp:bar"));

        // Call edge: forward in adj, reverse in radj.
        assert_eq!(g.adj()["/a.cpp:foo"][0].target, "/a.cpp:bar");
        assert_eq!(g.adj()["/a.cpp:foo"][0].kind, EdgeKind::Calls);
        assert_eq!(g.radj()["/a.cpp:bar"][0].target, "/a.cpp:foo");
        assert_eq!(g.radj()["/a.cpp:bar"][0].kind, EdgeKind::Calls);

        // Include edge: in includes, NOT in adj/radj.
        let key = PathBuf::from("/a.cpp");
        assert_eq!(g.includes()[&key], vec![PathBuf::from("/utils.h")]);

        // Files map records the language.
        assert!(g.files().contains_key(&key));
        assert_eq!(g.files()[&key].language, Language::Cpp);
    }

    #[test]
    fn merge_two_files_aggregates() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("foo", SymbolKind::Function, "/a.cpp")],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("bar", SymbolKind::Function, "/b.cpp")],
            vec![],
        ));

        let stats = g.stats();
        assert_eq!(stats.nodes, 2);
        assert_eq!(stats.files, 2);
        assert_eq!(stats.edges, 0);
        assert!(g.nodes().contains_key("/a.cpp:foo"));
        assert!(g.nodes().contains_key("/b.cpp:bar"));
    }

    #[test]
    fn re_merge_same_path_replaces() {
        let mut g = Graph::new();

        // Initial merge: 2 symbols.
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("foo", SymbolKind::Function, "/a.cpp"),
                sym("bar", SymbolKind::Function, "/a.cpp"),
            ],
            vec![],
        ));
        assert_eq!(g.stats().nodes, 2);

        // Re-merge with a single different symbol — old ones must be gone.
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("baz", SymbolKind::Function, "/a.cpp")],
            vec![],
        ));
        let stats = g.stats();
        assert_eq!(stats.nodes, 1);
        assert_eq!(stats.files, 1);
        assert!(!g.nodes().contains_key("/a.cpp:foo"));
        assert!(!g.nodes().contains_key("/a.cpp:bar"));
        assert!(g.nodes().contains_key("/a.cpp:baz"));

        // Idempotency: re-merging the same shape twice yields the same final state.
        let again = make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("baz", SymbolKind::Function, "/a.cpp")],
            vec![],
        );
        g.merge_file_graph(again.clone());
        g.merge_file_graph(again);
        let stats2 = g.stats();
        assert_eq!(stats, stats2);
        assert_eq!(stats2.nodes, 1);
    }

    #[test]
    fn re_merge_replaces_edges_not_just_nodes() {
        // The realistic incremental-index scenario: a file is edited so its
        // call targets change. Re-merging must drop stale edges, not just
        // stale nodes.
        let mut g = Graph::new();

        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("a", SymbolKind::Function, "/a.cpp"),
                sym("b", SymbolKind::Function, "/a.cpp"),
                sym("c", SymbolKind::Function, "/a.cpp"),
            ],
            vec![call_edge("/a.cpp:a", "/a.cpp:b", "/a.cpp", 1)],
        ));
        assert_eq!(g.adj()["/a.cpp:a"][0].target, "/a.cpp:b");
        assert!(g.radj().contains_key("/a.cpp:b"));

        // Re-merge: same nodes but `a` now calls `c` instead of `b`.
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("a", SymbolKind::Function, "/a.cpp"),
                sym("b", SymbolKind::Function, "/a.cpp"),
                sym("c", SymbolKind::Function, "/a.cpp"),
            ],
            vec![call_edge("/a.cpp:a", "/a.cpp:c", "/a.cpp", 1)],
        ));

        // Stale edge gone, new edge present, no doubling.
        assert_eq!(g.adj()["/a.cpp:a"].len(), 1);
        assert_eq!(g.adj()["/a.cpp:a"][0].target, "/a.cpp:c");
        assert!(
            !g.radj().contains_key("/a.cpp:b"),
            "stale reverse-edge key for old target must be dropped"
        );
        assert!(g.radj().contains_key("/a.cpp:c"));
        assert_eq!(g.stats().edges, 1);
    }

    #[test]
    fn remove_file_cleans_all_storage() {
        let mut g = Graph::new();

        // /a.cpp has a symbol that calls a symbol in /b.cpp, plus an include.
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("foo", SymbolKind::Function, "/a.cpp")],
            vec![
                call_edge("/a.cpp:foo", "/b.cpp:bar", "/a.cpp", 1),
                include_edge("/a.cpp", "/utils.h", "/a.cpp"),
            ],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("bar", SymbolKind::Function, "/b.cpp")],
            vec![call_edge("/b.cpp:bar", "/b.cpp:helper", "/b.cpp", 1)],
        ));

        let path_a = PathBuf::from("/a.cpp");
        g.remove_file(&path_a);

        // Node from /a.cpp gone; node from /b.cpp remains.
        assert!(!g.nodes().contains_key("/a.cpp:foo"));
        assert!(g.nodes().contains_key("/b.cpp:bar"));

        // adj/radj entries with file == /a.cpp gone. The radj key
        // "/b.cpp:bar" was populated only by the /a.cpp call, so the whole
        // key is gone — the edge originating from /b.cpp survives.
        assert!(!g.adj().contains_key("/a.cpp:foo"));
        assert!(!g.radj().contains_key("/b.cpp:bar"));
        // Edge originating from /b.cpp (file=/b.cpp) is preserved.
        assert!(g.adj().contains_key("/b.cpp:bar"));
        assert_eq!(g.adj()["/b.cpp:bar"][0].target, "/b.cpp:helper");

        // Includes for /a.cpp gone; files entry for /a.cpp gone.
        assert!(!g.includes().contains_key(&path_a));
        assert!(!g.files().contains_key(&path_a));
        assert!(g.files().contains_key(&PathBuf::from("/b.cpp")));
    }

    #[test]
    fn clear_resets_to_empty() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("foo", SymbolKind::Function, "/a.cpp")],
            vec![include_edge("/a.cpp", "/utils.h", "/a.cpp")],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("bar", SymbolKind::Function, "/b.cpp")],
            vec![],
        ));
        assert_ne!(
            g.stats(),
            GraphStats {
                nodes: 0,
                edges: 0,
                files: 0
            }
        );

        g.clear();

        assert_eq!(
            g.stats(),
            GraphStats {
                nodes: 0,
                edges: 0,
                files: 0,
            }
        );
        assert!(g.nodes().is_empty());
        assert!(g.adj().is_empty());
        assert!(g.radj().is_empty());
        assert!(g.files().is_empty());
        assert!(g.includes().is_empty());
    }

    #[test]
    fn merge_routes_inherits_edges_to_adj_radj() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("Base", SymbolKind::Class, "/a.cpp"),
                sym("Derived", SymbolKind::Class, "/a.cpp"),
            ],
            // Inherits edges use bare derived/base names per the Phase 1 quirk.
            vec![inherit_edge("Derived", "Base", "/a.cpp")],
        ));

        let adj = g.adj();
        let radj = g.radj();

        let derived_out = adj.get("Derived").expect("Derived has adj entry");
        assert_eq!(derived_out.len(), 1);
        assert_eq!(derived_out[0].target, "Base");
        assert_eq!(derived_out[0].kind, EdgeKind::Inherits);

        let base_in = radj.get("Base").expect("Base has radj entry");
        assert_eq!(base_in.len(), 1);
        assert_eq!(base_in[0].target, "Derived");
        assert_eq!(base_in[0].kind, EdgeKind::Inherits);
    }

    #[test]
    fn merge_routes_includes_only_to_includes_map() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![],
            vec![include_edge("/a.cpp", "/utils.h", "/a.cpp")],
        ));

        let key = PathBuf::from("/a.cpp");
        assert_eq!(g.includes()[&key], vec![PathBuf::from("/utils.h")]);
        // Must NOT leak into adj/radj.
        assert!(g.adj().is_empty());
        assert!(g.radj().is_empty());
    }

    #[test]
    fn file_entry_records_language() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("foo", SymbolKind::Function, "/a.cpp")],
            vec![],
        ));
        let entry = g.files().get(&PathBuf::from("/a.cpp")).unwrap();
        assert_eq!(entry.language, Language::Cpp);
        assert_eq!(entry.symbol_ids, vec!["/a.cpp:foo".to_string()]);
    }

    #[test]
    fn stats_after_re_merge() {
        let mut g = Graph::new();
        let fg = make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("foo", SymbolKind::Function, "/a.cpp"),
                sym("bar", SymbolKind::Function, "/a.cpp"),
                sym("baz", SymbolKind::Function, "/a.cpp"),
            ],
            vec![
                call_edge("/a.cpp:foo", "/a.cpp:bar", "/a.cpp", 1),
                include_edge("/a.cpp", "/utils.h", "/a.cpp"),
            ],
        );

        g.merge_file_graph(fg.clone());
        let first = g.stats();
        assert_eq!(first.nodes, 3);
        assert_eq!(first.edges, 2);
        assert_eq!(first.files, 1);

        // Re-merge the SAME FileGraph — counts must NOT double.
        g.merge_file_graph(fg);
        let second = g.stats();
        assert_eq!(first, second);
    }
}
