//! Graph storage and the merge / remove / clear mutators.
//!
//! This module mirrors the Go reference at `internal/graph/graph.go` lines
//! 1–175 (`Graph`, `Node`, `EdgeEntry`, `New`, `MergeFileGraph`, `RemoveFile`,
//! `removeFileUnsafe`, `Clear`). The Rust port adds a [`FileEntry`] that
//! records the source [`Language`] alongside the file's symbol IDs so the
//! cache v2 format can persist the language without re-deriving it from the
//! file extension. Locking is **not** introduced here — the server-side
//! [`Graph`] is wrapped behind a `parking_lot::RwLock` at the call site.
//!
//! Keys for the file-scoped maps (`files`, `includes`) are `PathBuf` rather
//! than `String` so callers do not have to launder `Path` ↔ `String`
//! conversions at every boundary. Symbol IDs remain `String` (aliased as
//! [`SymbolId`] in `code-graph-core`) because they are arbitrary identifiers,
//! not filesystem paths.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use code_graph_core::{symbol_id, EdgeKind, FileGraph, Language, Symbol, SymbolId};
use serde::{Deserialize, Serialize};

/// In-memory directed graph of code symbols.
///
/// Storage layout matches the Go reference exactly:
/// - `nodes`: symbol id → [`Node`]
/// - `adj` / `radj`: symbol id → outgoing / incoming `Calls` and `Inherits`
///   edges (the Go binary keeps both directions for cheap callers/callees)
/// - `files`: file path → [`FileEntry`] (language + owned symbol IDs)
/// - `includes`: file path → [`IncludeEntry`] list (included file path +
///   source line; kept separate from `adj`/`radj` because include edges
///   are file-to-file, not symbol-to-symbol)
#[derive(Debug, Default)]
pub struct Graph {
    pub(crate) nodes: HashMap<SymbolId, Node>,
    pub(crate) adj: HashMap<SymbolId, Vec<EdgeEntry>>,
    pub(crate) radj: HashMap<SymbolId, Vec<EdgeEntry>>,
    pub(crate) files: HashMap<PathBuf, FileEntry>,
    pub(crate) includes: HashMap<PathBuf, Vec<IncludeEntry>>,
}

/// Wrapper around a [`Symbol`] stored in the graph. Mirrors Go's `Node`.
///
/// `Serialize`/`Deserialize` are derived so callers can round-trip a `Node`
/// directly when convenient. Cache v2 (`persist.rs`) does not use them — it
/// flattens to `HashMap<SymbolId, Symbol>` to match the Go cache shape — but
/// other persistence layers may.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub symbol: Symbol,
}

/// One directed edge in either the forward (`adj`) or reverse (`radj`)
/// adjacency list. Mirrors Go's `EdgeEntry`. The `target` is the *other end*
/// of the edge from the map key's perspective: in `adj[from]` it is the
/// destination; in `radj[to]` it is the origin.
///
/// `PathBuf`'s default serde impl serializes as a string on Unix (and a
/// best-effort string on Windows), which is what cache v2 expects.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EdgeEntry {
    pub target: SymbolId,
    pub kind: EdgeKind,
    pub file: PathBuf,
    pub line: u32,
}

/// Per-file metadata recorded at merge time. The Go reference stores only
/// `map[string][]string` (path → symbol IDs); the Rust port also captures
/// the source [`Language`] so the cache v2 format can persist it without
/// re-deriving from the path extension.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub language: Language,
    pub symbol_ids: Vec<SymbolId>,
}

/// One entry in a file's include list: the included file path plus the
/// source line of the `#include`-style directive that produced it.
///
/// The include map previously stored bare `PathBuf`s, discarding the line.
/// Carrying the line lets the dependency query report *where* in the
/// source each include was declared instead of just *that* it exists.
/// `path`/`line` mirror the wire-format field names directly (no
/// `rename_all` needed); serde derives match the sibling cached structs so
/// the on-disk cache shape stays a single-deserializer-compatible JSON.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IncludeEntry {
    pub path: PathBuf,
    pub line: u32,
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

    /// Whether `id` resolves to a node currently stored in this graph.
    ///
    /// The "resolved" predicate shared by call-graph BFS and diagram
    /// edge rendering: an edge target string that fails this check is a
    /// bare callee token the parser captured but the call resolver could
    /// not bind to a definition (external function, macro identifier,
    /// stdlib call like `Ok`/`Err`/`printf`/`to_string`, etc.). Such
    /// tokens must NOT enter call-graph BFS `visited` sets — their
    /// presence would distort depth attribution for resolved neighbors at
    /// depth >= 2 by short-circuiting later legitimate visits via false
    /// `visited` membership — and must not render as path-basename
    /// pseudo-nodes in diagrams. Both [`Graph::bfs`] (used by
    /// `callers`/`callees`) and `Graph::diagram_call_graph`'s BFS
    /// expansion pivot on this exact `nodes.contains_key` check before
    /// inserting into `visited`, so the two tools stay behaviorally
    /// consistent on what counts as a "real" callee. The diagram path
    /// additionally uses the same predicate inside `diagrams::mermaid_label`
    /// as a post-BFS defense-in-depth filter for `raw_edges` entries
    /// that bypassed the expansion-time guard (truncation tail).
    pub(crate) fn is_resolved_node(&self, id: &str) -> bool {
        self.nodes.contains_key(id)
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
                        .push(IncludeEntry {
                            path: PathBuf::from(&edge.to),
                            line: edge.line,
                        });
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

    // "unsafe" here means "caller must hold the write lock on the
    // `parking_lot::RwLock` that wraps the server-side Graph". No Rust
    // `unsafe` code is involved.
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

    /// Scrub every adjacency entry that points at a symbol in `removed_ids`.
    ///
    /// `remove_file_unsafe` only deletes edges whose `file` equals the
    /// removed path. That covers edges *originating from* the file, but
    /// leaves dangling cross-file edges that *target* a now-removed symbol:
    /// e.g. file `B`'s call edge `B:caller → A:old_fn` is stored with
    /// `file = B` and survives a `remove_file(A)`, even though `A:old_fn`
    /// is gone from `nodes`. The watch-mode reindex path uses this method,
    /// scoped to the symbol IDs that genuinely disappeared during a
    /// rename / delete, to keep `adj`/`radj` consistent with `nodes`.
    ///
    /// Cost: O(edges touching the removed IDs), not O(all edges). The
    /// `HashSet` lookup is O(1), and most reindexes have a removed-set of
    /// size 0 or 1 (a routine modify with no rename touches no IDs at all).
    ///
    /// Inbound re-resolution — rebinding `B:caller`'s call to a renamed
    /// `A:new_fn` — is intentionally **out of scope**: that requires
    /// re-parsing `B`, which the watch event for `A` does not warrant. The
    /// agent sees `B:caller` with no recorded callee instead of phantom
    /// data; a subsequent edit to `B` will re-resolve naturally.
    pub fn prune_dangling_edges(&mut self, removed_ids: &HashSet<SymbolId>) {
        if removed_ids.is_empty() {
            return;
        }
        Self::retain_edges_not_targeting(&mut self.adj, removed_ids);
        Self::retain_edges_not_targeting(&mut self.radj, removed_ids);
        // Also drop any radj keys for removed symbols: their incoming-edge
        // list belongs to a node that no longer exists. (The same-file
        // incoming entries were already cleaned by remove_file_unsafe; this
        // catches cross-file ones.)
        for id in removed_ids {
            self.radj.remove(id);
            self.adj.remove(id);
        }
    }

    /// Filter edge map entries: drop edges whose `target` is in `removed`,
    /// and drop keys whose retained vec is empty.
    fn retain_edges_not_targeting(
        map: &mut HashMap<SymbolId, Vec<EdgeEntry>>,
        removed: &HashSet<SymbolId>,
    ) {
        map.retain(|_, entries| {
            entries.retain(|e| !removed.contains(&e.target));
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

    /// Reconstruct a `Vec<FileGraph>` from internal storage with the
    /// `symbols` populated (in insertion order) and `edges` left empty.
    ///
    /// This is the cheap snapshot the watch-mode incremental reindex
    /// path needs: to call language-aware edge resolution
    /// (`code_graph_tools::indexer::resolve_all_edges`) on a single
    /// re-parsed file, the resolver builds a `(Language, name)`-keyed
    /// `SymbolIndex` plus a basename-keyed `FileIndex` over the *whole*
    /// graph. Both indexes look at `fg.symbols` and `fg.language` only —
    /// `fg.edges` is irrelevant to index construction — so this snapshot
    /// can leave `edges` empty and still produce a complete index.
    ///
    /// Cost: O(symbols) clones plus one `Vec<FileGraph>` allocation.
    /// Iteration order over `files` is HashMap-defined, but the resolver
    /// does not depend on order (it's an inverted index).
    pub fn file_graphs_snapshot(&self) -> Vec<FileGraph> {
        let mut out = Vec::with_capacity(self.files.len());
        for (path, entry) in &self.files {
            let mut symbols = Vec::with_capacity(entry.symbol_ids.len());
            for id in &entry.symbol_ids {
                if let Some(node) = self.nodes.get(id) {
                    symbols.push(node.symbol.clone());
                }
            }
            out.push(FileGraph {
                path: path.to_string_lossy().into_owned(),
                language: entry.language,
                symbols,
                edges: Vec::new(),
            });
        }
        out
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

    // ----- Read accessors used only by the in-module tests. The public
    // query surface (`file_symbols`, `symbol_detail`, etc.) is built on top
    // of these private maps. Gated to `cfg(test)` so the dead_code lint
    // stays clean.
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
    fn includes(&self) -> &HashMap<PathBuf, Vec<IncludeEntry>> {
        &self.includes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::{call_edge, include_edge, inherit_edge, make_fg, sym};
    use code_graph_core::{Edge, SymbolKind};

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
                // Inline (not the `line: 0` fixture) so the include's
                // source line is a distinctive non-zero value: this is
                // what proves `merge_file_graph` propagates `edge.line`
                // into the stored `IncludeEntry` rather than defaulting it.
                Edge {
                    from: "/a.cpp".to_string(),
                    to: "/utils.h".to_string(),
                    kind: EdgeKind::Includes,
                    file: "/a.cpp".to_string(),
                    line: 12,
                },
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

        // Include edge: in includes (with its source line preserved),
        // NOT in adj/radj.
        let key = PathBuf::from("/a.cpp");
        assert_eq!(
            g.includes()[&key],
            vec![IncludeEntry {
                path: PathBuf::from("/utils.h"),
                line: 12,
            }],
        );

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
            // Inherits edges use bare (non-generic) derived/base names.
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
        // This test pins the *routing* invariant; the `include_edge`
        // fixture carries `line: 0`, which flows through unchanged.
        assert_eq!(
            g.includes()[&key],
            vec![IncludeEntry {
                path: PathBuf::from("/utils.h"),
                line: 0,
            }],
        );
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
    fn file_graphs_snapshot_returns_one_entry_per_file_with_no_edges() {
        // The watch-mode snapshot helper must reconstruct one FileGraph per
        // stored file with the file's symbols (in insertion order) and an
        // empty `edges` Vec — edges are merged into adj/radj/includes at
        // merge time and aren't recoverable from internal storage in
        // FileGraph form, but the watch path only needs symbols+language
        // for index construction.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("foo", SymbolKind::Function, "/a.cpp"),
                sym("bar", SymbolKind::Function, "/a.cpp"),
            ],
            vec![call_edge("/a.cpp:foo", "/a.cpp:bar", "/a.cpp", 1)],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("baz", SymbolKind::Function, "/b.cpp")],
            vec![],
        ));

        let snapshot = g.file_graphs_snapshot();
        assert_eq!(snapshot.len(), 2, "one FileGraph per stored file");
        for fg in &snapshot {
            assert!(
                fg.edges.is_empty(),
                "snapshot leaves edges empty (merged into adj/radj/includes already)"
            );
            assert_eq!(fg.language, Language::Cpp);
        }
        // Find /a.cpp's snapshot — order is HashMap-defined.
        let a = snapshot
            .iter()
            .find(|fg| fg.path == "/a.cpp")
            .expect("/a.cpp present");
        assert_eq!(a.symbols.len(), 2);
        assert_eq!(a.symbols[0].name, "foo");
        assert_eq!(a.symbols[1].name, "bar");
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

    #[test]
    fn prune_dangling_edges_drops_cross_file_targets_to_removed_symbols() {
        // Watch-mode rename scenario: A defines old_fn; B calls old_fn.
        // After A is reindexed without old_fn, the cross-file edge from
        // B is left dangling (its `file` is B, not A). prune_dangling_edges
        // — given the truly-removed ID set — must scrub it.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("old_fn", SymbolKind::Function, "/a.cpp")],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("caller", SymbolKind::Function, "/b.cpp")],
            // file=B for the cross-file edge — that's the bug-trigger shape.
            vec![call_edge("/b.cpp:caller", "/a.cpp:old_fn", "/b.cpp", 7)],
        ));

        // Sanity: pre-prune, B's caller targets old_fn.
        assert_eq!(g.adj()["/b.cpp:caller"][0].target, "/a.cpp:old_fn");
        assert!(g.radj().contains_key("/a.cpp:old_fn"));

        // Simulate the "old_fn was truly removed" set the watch path computes.
        let mut removed = HashSet::new();
        removed.insert("/a.cpp:old_fn".to_string());
        g.prune_dangling_edges(&removed);

        // The dangling edge is gone, so `adj["/b.cpp:caller"]` either has
        // no entries left (key dropped) or no entry targeting old_fn.
        assert!(
            g.adj()
                .get("/b.cpp:caller")
                .is_none_or(|v| v.iter().all(|e| e.target != "/a.cpp:old_fn")),
            "dangling forward edge to removed symbol must be gone"
        );
        // radj key for old_fn fully cleared — the symbol no longer exists.
        assert!(!g.radj().contains_key("/a.cpp:old_fn"));
    }

    #[test]
    fn prune_dangling_edges_empty_set_is_noop() {
        // Routine reindex (no rename) should produce an empty removed set;
        // the method must be a true no-op so the watch hot path costs
        // nothing on the common case.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("foo", SymbolKind::Function, "/a.cpp"),
                sym("bar", SymbolKind::Function, "/a.cpp"),
            ],
            vec![call_edge("/a.cpp:foo", "/a.cpp:bar", "/a.cpp", 1)],
        ));
        let before = g.stats();
        g.prune_dangling_edges(&HashSet::new());
        assert_eq!(g.stats(), before);
        assert_eq!(g.adj()["/a.cpp:foo"][0].target, "/a.cpp:bar");
    }

    #[test]
    fn prune_dangling_edges_preserves_unrelated_edges() {
        // Only the entry whose `target ∈ removed_ids` should be scrubbed —
        // other entries on the same key, and entries to non-removed
        // symbols, must survive.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("old_fn", SymbolKind::Function, "/a.cpp"),
                sym("kept_fn", SymbolKind::Function, "/a.cpp"),
            ],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("caller", SymbolKind::Function, "/b.cpp")],
            vec![
                call_edge("/b.cpp:caller", "/a.cpp:old_fn", "/b.cpp", 1),
                call_edge("/b.cpp:caller", "/a.cpp:kept_fn", "/b.cpp", 2),
            ],
        ));

        let mut removed = HashSet::new();
        removed.insert("/a.cpp:old_fn".to_string());
        g.prune_dangling_edges(&removed);

        let entries = g.adj().get("/b.cpp:caller").expect("caller key kept");
        assert_eq!(entries.len(), 1, "exactly the unrelated edge survives");
        assert_eq!(entries[0].target, "/a.cpp:kept_fn");
        assert!(g.radj().contains_key("/a.cpp:kept_fn"));
        assert!(!g.radj().contains_key("/a.cpp:old_fn"));
    }
}
