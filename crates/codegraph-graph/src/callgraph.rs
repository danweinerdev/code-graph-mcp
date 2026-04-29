//! Call-graph traversal: BFS over `Calls` edges in either direction
//! (`callers` over `radj`, `callees` over `adj`), plus the related
//! `orphans` and `file_dependencies` helpers.
//!
//! Mirrors the Go reference at `internal/graph/graph.go` lines 322–427
//! (`Callers`, `Callees`, `bfs`, `FileDependencies`, `Orphans`). The Go
//! shape uses `int` for line and depth; the Rust port uses `u32` since
//! both are non-negative by construction.
//!
//! Locking is not handled in this module. Task 2.6 wraps [`Graph`] in
//! `parking_lot::RwLock`; until then these methods take `&self` and rely
//! on the caller for synchronization.
//!
//! Class-hierarchy traversal and cycle detection live in their own
//! submodule (`algorithms.rs` from Task 2.4) — this module deliberately
//! stays focused on the call-graph surface.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use codegraph_core::{EdgeKind, Symbol, SymbolId, SymbolKind};

use crate::{EdgeEntry, Graph};

/// One hop on a call chain returned by [`Graph::callers`] / [`Graph::callees`].
///
/// `symbol_id` identifies the visited node; `file` and `line` carry the
/// edge's call site (matching Go's `EdgeEntry.File` / `Line`); `depth` is
/// the BFS distance from the start node (1 = direct caller/callee).
///
/// JSON tags match the Go shape exactly (`symbol_id`, `file`, `line`,
/// `depth`) — derived from the snake_case Rust field names without
/// needing `rename_all`.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CallChain {
    pub symbol_id: SymbolId,
    pub file: PathBuf,
    pub line: u32,
    pub depth: u32,
}

impl Graph {
    /// Symbols that call `id`, up to `depth` hops away. BFS over the
    /// reverse adjacency list filtered by `EdgeKind::Calls`. `depth = 0`
    /// is normalized to 1 to match the Go behavior — an agent passing
    /// `0` would otherwise get an empty result, which is confusing.
    pub fn callers(&self, id: &str, depth: u32) -> Vec<CallChain> {
        self.bfs(id, depth, &self.radj, EdgeKind::Calls)
    }

    /// Symbols called by `id`, up to `depth` hops away. BFS over the
    /// forward adjacency list filtered by `EdgeKind::Calls`. `depth = 0`
    /// is normalized to 1 (see [`Graph::callers`]).
    pub fn callees(&self, id: &str, depth: u32) -> Vec<CallChain> {
        self.bfs(id, depth, &self.adj, EdgeKind::Calls)
    }

    /// Internal BFS shared by `callers` and `callees`. The caller picks
    /// the adjacency map (`adj` for forward, `radj` for reverse) and the
    /// edge kind to follow. Each node is visited at most once via the
    /// `visited` set, so cycles can never produce an infinite loop.
    ///
    /// The `start` node is pre-inserted into `visited` so it never
    /// appears in the result, even if the graph contains a self-loop or
    /// a cycle that would otherwise revisit it.
    fn bfs(
        &self,
        start: &str,
        depth: u32,
        adjacency: &HashMap<SymbolId, Vec<EdgeEntry>>,
        kind: EdgeKind,
    ) -> Vec<CallChain> {
        let depth = if depth == 0 { 1 } else { depth };

        let mut visited: HashSet<SymbolId> = HashSet::new();
        visited.insert(start.to_string());

        let mut queue: VecDeque<(SymbolId, u32)> = VecDeque::new();
        queue.push_back((start.to_string(), 0));

        let mut result: Vec<CallChain> = Vec::new();

        while let Some((curr_id, curr_depth)) = queue.pop_front() {
            if curr_depth >= depth {
                continue;
            }

            let Some(entries) = adjacency.get(&curr_id) else {
                continue;
            };
            for entry in entries {
                if entry.kind != kind {
                    continue;
                }
                if visited.contains(&entry.target) {
                    continue;
                }
                visited.insert(entry.target.clone());
                let new_depth = curr_depth + 1;
                result.push(CallChain {
                    symbol_id: entry.target.clone(),
                    file: entry.file.clone(),
                    line: entry.line,
                    depth: new_depth,
                });
                queue.push_back((entry.target.clone(), new_depth));
            }
        }

        result
    }

    /// Symbols with no incoming `Calls` edges.
    ///
    /// `kind = None` (the default) returns only callables — functions
    /// and methods. `kind = Some(k)` filters strictly by the requested
    /// kind, which lets callers ask for orphan classes / structs / etc.
    /// directly. `SymbolKind` is `#[non_exhaustive]`, so the default
    /// branch enumerates the two callable variants explicitly rather
    /// than relying on a fall-through.
    pub fn orphans(&self, kind: Option<SymbolKind>) -> Vec<Symbol> {
        let mut result: Vec<Symbol> = Vec::new();
        for (id, node) in &self.nodes {
            match kind {
                None => match node.symbol.kind {
                    SymbolKind::Function | SymbolKind::Method => {}
                    _ => continue,
                },
                Some(k) => {
                    if node.symbol.kind != k {
                        continue;
                    }
                }
            }

            let has_caller = self
                .radj
                .get(id)
                .is_some_and(|entries| entries.iter().any(|e| e.kind == EdgeKind::Calls));
            if !has_caller {
                result.push(node.symbol.clone());
            }
        }
        result
    }

    /// Files included by `path` (`#include`-style edges). Returns an
    /// empty `Vec` for unknown paths so JSON serializes as `[]`, never
    /// `null`. The returned `Vec` is a clone — callers may mutate it
    /// without affecting the graph.
    pub fn file_dependencies(&self, path: &Path) -> Vec<PathBuf> {
        match self.includes.get(path) {
            Some(deps) => deps.clone(),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codegraph_core::{Edge, FileGraph, Language};

    fn sym(name: &str, kind: SymbolKind, file: &str) -> Symbol {
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

    fn call_edge(from: &str, to: &str, file: &str, line: u32) -> Edge {
        Edge {
            from: from.to_string(),
            to: to.to_string(),
            kind: EdgeKind::Calls,
            file: file.to_string(),
            line,
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

    fn include_edge(from: &str, to: &str, file: &str) -> Edge {
        Edge {
            from: from.to_string(),
            to: to.to_string(),
            kind: EdgeKind::Includes,
            file: file.to_string(),
            line: 0,
        }
    }

    fn make_fg(path: &str, symbols: Vec<Symbol>, edges: Vec<Edge>) -> FileGraph {
        FileGraph {
            path: path.to_string(),
            language: Language::Cpp,
            symbols,
            edges,
        }
    }

    /// Linear chain `a -> b -> c -> d` all in `/x.cpp`.
    fn linear_chain() -> Graph {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            vec![
                sym("a", SymbolKind::Function, "/x.cpp"),
                sym("b", SymbolKind::Function, "/x.cpp"),
                sym("c", SymbolKind::Function, "/x.cpp"),
                sym("d", SymbolKind::Function, "/x.cpp"),
            ],
            vec![
                call_edge("/x.cpp:a", "/x.cpp:b", "/x.cpp", 10),
                call_edge("/x.cpp:b", "/x.cpp:c", "/x.cpp", 20),
                call_edge("/x.cpp:c", "/x.cpp:d", "/x.cpp", 30),
            ],
        ));
        g
    }

    fn ids(chain: &[CallChain]) -> Vec<String> {
        let mut v: Vec<String> = chain.iter().map(|c| c.symbol_id.clone()).collect();
        v.sort();
        v
    }

    // --- callers / callees on a linear chain ---

    #[test]
    fn callers_linear_chain() {
        let g = linear_chain();

        let one = g.callers("/x.cpp:d", 1);
        assert_eq!(ids(&one), vec!["/x.cpp:c".to_string()]);

        let two = g.callers("/x.cpp:d", 2);
        assert_eq!(
            ids(&two),
            vec!["/x.cpp:b".to_string(), "/x.cpp:c".to_string()],
        );

        let three = g.callers("/x.cpp:d", 3);
        assert_eq!(
            ids(&three),
            vec![
                "/x.cpp:a".to_string(),
                "/x.cpp:b".to_string(),
                "/x.cpp:c".to_string(),
            ],
        );
    }

    #[test]
    fn callees_linear_chain() {
        let g = linear_chain();

        let one = g.callees("/x.cpp:a", 1);
        assert_eq!(ids(&one), vec!["/x.cpp:b".to_string()]);

        let three = g.callees("/x.cpp:a", 3);
        assert_eq!(
            ids(&three),
            vec![
                "/x.cpp:b".to_string(),
                "/x.cpp:c".to_string(),
                "/x.cpp:d".to_string(),
            ],
        );
    }

    // --- diamond ---

    #[test]
    fn bfs_handles_diamond() {
        // a -> b, a -> c, b -> d, c -> d. d must be visited exactly once.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            vec![
                sym("a", SymbolKind::Function, "/x.cpp"),
                sym("b", SymbolKind::Function, "/x.cpp"),
                sym("c", SymbolKind::Function, "/x.cpp"),
                sym("d", SymbolKind::Function, "/x.cpp"),
            ],
            vec![
                call_edge("/x.cpp:a", "/x.cpp:b", "/x.cpp", 1),
                call_edge("/x.cpp:a", "/x.cpp:c", "/x.cpp", 2),
                call_edge("/x.cpp:b", "/x.cpp:d", "/x.cpp", 3),
                call_edge("/x.cpp:c", "/x.cpp:d", "/x.cpp", 4),
            ],
        ));

        let chain = g.callees("/x.cpp:a", 2);
        assert_eq!(chain.len(), 3, "d visited only once: {chain:?}");
        assert_eq!(
            ids(&chain),
            vec![
                "/x.cpp:b".to_string(),
                "/x.cpp:c".to_string(),
                "/x.cpp:d".to_string(),
            ],
        );
    }

    // --- cycles ---

    #[test]
    fn bfs_does_not_loop_on_cycle() {
        // a -> b -> c -> a (cycle of 3).
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            vec![
                sym("a", SymbolKind::Function, "/x.cpp"),
                sym("b", SymbolKind::Function, "/x.cpp"),
                sym("c", SymbolKind::Function, "/x.cpp"),
            ],
            vec![
                call_edge("/x.cpp:a", "/x.cpp:b", "/x.cpp", 1),
                call_edge("/x.cpp:b", "/x.cpp:c", "/x.cpp", 2),
                call_edge("/x.cpp:c", "/x.cpp:a", "/x.cpp", 3),
            ],
        ));

        // depth=10 is far higher than the cycle length; if the BFS
        // looped, this would never return.
        let chain = g.callees("/x.cpp:a", 10);
        assert_eq!(chain.len(), 2, "exactly b and c, never a again: {chain:?}");
        assert_eq!(
            ids(&chain),
            vec!["/x.cpp:b".to_string(), "/x.cpp:c".to_string()],
        );
    }

    // --- depth normalization ---

    #[test]
    fn bfs_depth_zero_normalized_to_one() {
        let g = linear_chain();
        let zero = g.callees("/x.cpp:a", 0);
        let one = g.callees("/x.cpp:a", 1);
        assert_eq!(zero, one, "depth=0 must behave like depth=1");
        assert_eq!(zero.len(), 1);
        assert_eq!(zero[0].symbol_id, "/x.cpp:b");
    }

    // --- unknown start node ---

    #[test]
    fn bfs_unknown_symbol_returns_empty() {
        let g = linear_chain();
        assert!(g.callers("nonexistent", 5).is_empty());
        assert!(g.callees("nonexistent", 5).is_empty());
    }

    // --- CallChain payload ---

    #[test]
    fn call_chain_carries_file_and_line() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            vec![
                sym("a", SymbolKind::Function, "/x.cpp"),
                sym("b", SymbolKind::Function, "/x.cpp"),
            ],
            vec![call_edge("/x.cpp:a", "/x.cpp:b", "/x.cpp", 42)],
        ));

        let chain = g.callees("/x.cpp:a", 1);
        assert_eq!(chain.len(), 1);
        let hop = &chain[0];
        assert_eq!(hop.symbol_id, "/x.cpp:b");
        assert_eq!(hop.file, PathBuf::from("/x.cpp"));
        assert_eq!(hop.line, 42);
        assert_eq!(hop.depth, 1);
    }

    // --- BFS only follows the requested edge kind ---

    #[test]
    fn callers_only_traverses_calls_kind() {
        // radj["b"] gets BOTH a Calls entry (from `a Calls b`) and an
        // Inherits entry (from `Derived Inherits b`). callers() must
        // follow only the Calls edge.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            vec![
                sym("a", SymbolKind::Function, "/x.cpp"),
                sym("b", SymbolKind::Function, "/x.cpp"),
                sym("Derived", SymbolKind::Class, "/x.cpp"),
            ],
            vec![
                call_edge("/x.cpp:a", "/x.cpp:b", "/x.cpp", 1),
                // Inherits edges in this codebase use bare names (Phase
                // 1 quirk preserved). Source = "Derived", target = "b".
                inherit_edge("Derived", "/x.cpp:b", "/x.cpp"),
            ],
        ));

        let chain = g.callers("/x.cpp:b", 5);
        let names = ids(&chain);
        assert_eq!(
            names,
            vec!["/x.cpp:a".to_string()],
            "only the Calls source is reported; Inherits source is filtered out"
        );
    }

    // --- orphans ---

    #[test]
    fn orphans_default_returns_only_callables() {
        // unused Function (no callers), MyClass with no callers, caller
        // Function that calls used. Default mode: only `unused` is an
        // orphan — `caller` has no callers either, but it's still a
        // callable; `MyClass` is filtered out by the default kind rule.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            vec![
                sym("unused", SymbolKind::Function, "/x.cpp"),
                sym("MyClass", SymbolKind::Class, "/x.cpp"),
                sym("caller", SymbolKind::Function, "/x.cpp"),
                sym("used", SymbolKind::Function, "/x.cpp"),
            ],
            vec![call_edge("/x.cpp:caller", "/x.cpp:used", "/x.cpp", 1)],
        ));

        let mut names: Vec<String> = g.orphans(None).into_iter().map(|s| s.name).collect();
        names.sort();
        // `unused` and `caller` both have no callers and both are
        // functions; `used` has a caller; `MyClass` is filtered out.
        assert_eq!(names, vec!["caller".to_string(), "unused".to_string()]);
    }

    #[test]
    fn orphans_with_kind_filter() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            vec![
                sym("unused", SymbolKind::Function, "/x.cpp"),
                sym("MyClass", SymbolKind::Class, "/x.cpp"),
                sym("OtherClass", SymbolKind::Class, "/x.cpp"),
            ],
            vec![],
        ));

        let mut names: Vec<String> = g
            .orphans(Some(SymbolKind::Class))
            .into_iter()
            .map(|s| s.name)
            .collect();
        names.sort();
        assert_eq!(names, vec!["MyClass".to_string(), "OtherClass".to_string()]);
    }

    #[test]
    fn orphans_excludes_called_symbols() {
        // `used` has an incoming Calls edge in radj — must NOT appear.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            vec![
                sym("caller", SymbolKind::Function, "/x.cpp"),
                sym("used", SymbolKind::Function, "/x.cpp"),
            ],
            vec![call_edge("/x.cpp:caller", "/x.cpp:used", "/x.cpp", 1)],
        ));

        let names: Vec<String> = g.orphans(None).into_iter().map(|s| s.name).collect();
        assert!(
            !names.contains(&"used".to_string()),
            "called symbol must not be an orphan: {names:?}"
        );
        // `caller` itself has no callers and so is reported.
        assert_eq!(names, vec!["caller".to_string()]);
    }

    // --- file_dependencies ---

    #[test]
    fn file_dependencies_known_path() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            vec![],
            vec![
                include_edge("/a.cpp", "/utils.h", "/a.cpp"),
                include_edge("/a.cpp", "/types.h", "/a.cpp"),
            ],
        ));

        let deps = g.file_dependencies(&PathBuf::from("/a.cpp"));
        assert_eq!(
            deps,
            vec![PathBuf::from("/utils.h"), PathBuf::from("/types.h")],
        );
    }

    #[test]
    fn file_dependencies_unknown_path_returns_empty() {
        let g = Graph::new();
        let deps = g.file_dependencies(&PathBuf::from("/never-merged.cpp"));
        // Vec, never None — JSON must serialize as `[]`, not `null`.
        assert!(deps.is_empty());
    }
}
