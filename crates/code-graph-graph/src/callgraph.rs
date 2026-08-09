//! Call-graph traversal: BFS over `Calls` edges in either direction
//! (`callers` over `radj`, `callees` over `adj`), plus the related
//! `orphans` and `file_dependencies` helpers.
//!
//! Mirrors the Go reference at `internal/graph/graph.go` lines 322–427
//! (`Callers`, `Callees`, `bfs`, `FileDependencies`, `Orphans`). The Go
//! shape uses `int` for line and depth; the Rust port uses `u32` since
//! both are non-negative by construction.
//!
//! Locking is not handled in this module: these methods take `&self`
//! and rely on the caller for synchronization. The server-side
//! [`Graph`] is wrapped in `parking_lot::RwLock` (re-exported from
//! `code_graph_graph::RwLock`); query handlers take a read lock
//! around the call.
//!
//! Class-hierarchy traversal and cycle detection live in their own
//! submodule (`algorithms.rs`) — this module deliberately stays
//! focused on the call-graph surface.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use code_graph_core::{Confidence, EdgeKind, Symbol, SymbolId, SymbolKind};

use crate::{EdgeEntry, Graph, IncludeEntry};

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
    ///
    /// `min_confidence` filters edges by their resolver confidence
    /// (see [`Confidence`]): `None` admits every edge,
    /// `Some(Confidence::Resolved)` drops `Heuristic` (multi-candidate)
    /// edges at BFS time so the resulting chain only contains hops the
    /// resolver was sure about. The threshold is applied at each hop, so
    /// a depth-2 walk via a Heuristic intermediate is pruned entirely
    /// (the intermediate's own depth-1 row never enters `visited`).
    /// Passing `Some(Confidence::Heuristic)` is a no-op (every
    /// confidence value satisfies it).
    pub fn callers(
        &self,
        id: &str,
        depth: u32,
        min_confidence: Option<Confidence>,
    ) -> Vec<CallChain> {
        self.bfs(id, depth, &self.radj, EdgeKind::Calls, min_confidence)
    }

    /// Symbols called by `id`, up to `depth` hops away. BFS over the
    /// forward adjacency list filtered by `EdgeKind::Calls`. `depth = 0`
    /// is normalized to 1 (see [`Graph::callers`]). `min_confidence` has
    /// the same semantics as on [`Graph::callers`].
    pub fn callees(
        &self,
        id: &str,
        depth: u32,
        min_confidence: Option<Confidence>,
    ) -> Vec<CallChain> {
        self.bfs(id, depth, &self.adj, EdgeKind::Calls, min_confidence)
    }

    /// Methods that override `id`. One-hop only: returns the direct
    /// overrides recorded as `EdgeKind::Overrides` edges in the
    /// reverse adjacency list. Each result is a `(symbol_id, file,
    /// line)` triple of the OVERRIDING method.
    ///
    /// Override edges live in `radj` keyed by the BASE method's
    /// symbol_id (the edge's `to`); the override method is the edge's
    /// `from`, which surfaces here as the `target` field of the
    /// `EdgeEntry` (the reverse-adj convention).
    ///
    /// Unlike `callers`/`callees` this is NOT a transitive BFS — the
    /// override relationship is a single-step "this method overrides
    /// that method" by language semantics; chasing it transitively
    /// would conflate inheritance depth with override depth in
    /// confusing ways. A caller wanting "every method that
    /// transitively reaches an override of X" can compose
    /// `find_overrides` with `callers`.
    pub fn find_overrides(&self, id: &str) -> Vec<CallChain> {
        let mut out = Vec::new();
        if let Some(entries) = self.radj.get(id) {
            for entry in entries {
                if entry.kind == EdgeKind::Overrides && self.is_resolved_node(&entry.target) {
                    out.push(CallChain {
                        symbol_id: entry.target.clone(),
                        file: entry.file.clone(),
                        line: entry.line,
                        depth: 1,
                    });
                }
            }
        }
        out
    }

    /// Internal BFS shared by `callers` and `callees`. The caller picks
    /// the adjacency map (`adj` for forward, `radj` for reverse) and the
    /// edge kind to follow. Each node is visited at most once via the
    /// `visited` set, so cycles can never produce an infinite loop.
    ///
    /// The `start` node is pre-inserted into `visited` so it never
    /// appears in the result, even if the graph contains a self-loop or
    /// a cycle that would otherwise revisit it.
    ///
    /// Hops whose target is not a resolved node ([`Graph::is_resolved_node`])
    /// are skipped entirely: they neither emit a `CallChain` nor enter
    /// `visited`. This brings `get_callers`/`get_callees` to parity with
    /// `generate_diagram`'s edge filter (both gate on `nodes.contains_key`)
    /// and — critically — keeps unresolved tokens out of the visited set,
    /// so two resolved callers that both happen to reach the same bare
    /// token (e.g. `Ok`, `printf`) don't cross-poison each other's
    /// depth-`>= 2` traversal of resolved neighbors. A callable symbol
    /// whose only callees are unresolved produces an empty result vec —
    /// that is the natural "no resolved hops" case, indistinguishable
    /// from "callable with no callees at all", and the handler renders
    /// both as the empty `Page<CallChain>` envelope.
    fn bfs(
        &self,
        start: &str,
        depth: u32,
        adjacency: &HashMap<SymbolId, Vec<EdgeEntry>>,
        kind: EdgeKind,
        min_confidence: Option<Confidence>,
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
                // Confidence filter (Phase 3 of #27). Heuristic edges
                // are dropped at the same point as unresolved targets,
                // so a multi-candidate intermediate at depth 1 NEVER
                // enters `visited` — and `get_callers`/`get_callees`
                // with `min_confidence=resolved` returns chains that
                // are end-to-end Resolved. The threshold is `==` not
                // `<=` because `Confidence` has no Ord impl: only
                // `Some(Resolved)` and `None` are useful (Heuristic as
                // a threshold passes everything).
                if min_confidence == Some(Confidence::Resolved)
                    && entry.confidence != Confidence::Resolved
                {
                    continue;
                }
                // Resolved-only filter (design Decision 7). Unresolved
                // targets (raw callee tokens the resolver couldn't bind)
                // are dropped BEFORE the `visited` insert so they don't
                // suppress legitimate later visits of resolved neighbors
                // along a different path. The predicate is the same
                // `nodes.contains_key` check `mermaid_label` applies for
                // diagram edges, so `get_callers`/`get_callees` and
                // `generate_diagram` agree on what counts as a real hop.
                if !self.is_resolved_node(&entry.target) {
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

    /// Same as [`orphans`] but restricted to symbols whose file is at
    /// or under `subtree_prefix`. Walks
    /// [`PathTrie::iter_subtree`](code_graph_path_trie::PathTrie::iter_subtree)
    /// over `self.files`, so the cost is `O(symbols-under-prefix)` and
    /// independent of how big the rest of the graph is.
    ///
    /// **Phase E payoff site.** Pre-Phase E the same query would have
    /// scanned `self.nodes` (millions of entries on UE/LLVM-scale)
    /// and filtered by `Symbol.file.starts_with(prefix)` per
    /// candidate. The trie's subtree iter gives bounded work, which
    /// matters once `subtree_prefix` narrows the search to a single
    /// crate or directory.
    ///
    /// [`orphans`]: Graph::orphans
    pub fn orphans_under(&self, subtree_prefix: &Path, kind: Option<SymbolKind>) -> Vec<Symbol> {
        let mut result: Vec<Symbol> = Vec::new();
        for (_path, entry) in self.files.iter_subtree(subtree_prefix) {
            for sid in &entry.symbol_ids {
                let Some(node) = self.nodes.get(sid) else {
                    continue;
                };
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
                    .get(sid)
                    .is_some_and(|entries| entries.iter().any(|e| e.kind == EdgeKind::Calls));
                if !has_caller {
                    result.push(node.symbol.clone());
                }
            }
        }
        result
    }

    /// Files included by `path` (`#include`-style edges), each paired with
    /// the source line of the include directive. Returns an empty `Vec`
    /// for unknown paths so JSON serializes as `[]`, never `null`. The
    /// returned `Vec` is a clone — callers may mutate it without affecting
    /// the graph.
    pub fn file_dependencies(&self, path: &Path) -> Vec<IncludeEntry> {
        match self.includes.get(path) {
            Some(deps) => deps.clone(),
            None => Vec::new(),
        }
    }

    /// Shortest call-path from `from` to `to`, Dijkstra over a lexicographic
    /// `(hops, heuristic_hops)` cost (Design `GraphQueries` Decision 3).
    ///
    /// Traversal semantics are deliberately identical to the private [`bfs`]
    /// this module shares with [`Graph::callers`]/[`Graph::callees`]:
    /// forward adjacency (`self.adj`) only, [`EdgeKind::Calls`] only,
    /// targets failing [`Graph::is_resolved_node`] are skipped before they
    /// would enter the frontier, and `min_confidence == Some(Confidence::Resolved)`
    /// drops non-`Resolved` edges at each hop (`Confidence` has no `Ord`, so
    /// this is a strict-equality filter, not a threshold).
    ///
    /// Cost is packed as a single `u64`: `((hops as u64) << 32) | (heuristic_hops as u64)`.
    /// Hops is the primary key, so the returned path is always genuinely
    /// shortest; the heuristic-hop count only breaks ties among equally-short
    /// paths, preferring the all-resolved (or more-resolved) one. **The widen
    /// to `u64` happens before the shift** — `hops` never leaves `u32` before
    /// the cast, so `hops << 32` (a full-bit-width shift on a `u32`, which
    /// panics in debug and evaluates to `0` in release) never occurs.
    ///
    /// The heap key is `(cost, symbol_id)`, so equal-cost frontier nodes pop
    /// in `symbol_id`-ascending order — the traversal, and therefore the
    /// result, is deterministic across runs and process restarts (`HashMap`
    /// iteration order never enters the decision).
    ///
    /// `node_cap` bounds the number of nodes popped off the heap
    /// (`nodes_examined`). Reaching the cap before finding `to` returns
    /// `(None, nodes_examined, true)` — a partial path is never returned;
    /// `cap_reached` is the sole discriminator between "no path exists" and
    /// "the search was cut short".
    ///
    /// `from == to` is a trivial success: a single-hop chain containing only
    /// the source, `heuristic_hops = 0`, independent of `node_cap`.
    ///
    /// An unknown `from` or `to` returns `(None, 0, false)` — the caller
    /// (the `find_path` handler) is responsible for turning that into a
    /// tool error naming which endpoint failed.
    ///
    /// Returns `(path, nodes_examined, cap_reached)`. When `path` is
    /// `Some(_)`, its own [`PathResult::nodes_examined`] and
    /// [`PathResult::cap_reached`] fields mirror the second and third tuple
    /// elements exactly — the tuple form lets the caller read
    /// `nodes_examined`/`cap_reached` uniformly regardless of whether a path
    /// was found.
    ///
    /// [`bfs`]: Graph::bfs
    pub fn shortest_path(
        &self,
        from: &str,
        to: &str,
        node_cap: u32,
        min_confidence: Option<Confidence>,
    ) -> (Option<PathResult>, u32, bool) {
        if !self.nodes.contains_key(from) || !self.nodes.contains_key(to) {
            return (None, 0, false);
        }

        if from == to {
            let hop = self.path_hop_for_source(from);
            let result = PathResult {
                hops: vec![hop],
                heuristic_hops: 0,
            };
            return (Some(result), 1, false);
        }

        let mut best_cost: HashMap<SymbolId, u64> = HashMap::new();
        let mut incoming: HashMap<SymbolId, IncomingRecord> = HashMap::new();
        let mut heap: BinaryHeap<Reverse<(u64, SymbolId)>> = BinaryHeap::new();

        best_cost.insert(from.to_string(), 0);
        heap.push(Reverse((0u64, from.to_string())));

        let mut nodes_examined: u32 = 0;

        while let Some(Reverse((cost, curr_id))) = heap.pop() {
            // Stale heap entry: a better cost for this node was already
            // relaxed and popped. Skip without counting it toward
            // `nodes_examined` — it was never genuinely "examined".
            if best_cost.get(&curr_id) != Some(&cost) {
                continue;
            }

            nodes_examined += 1;

            if curr_id == to {
                let hops = chain_from_incoming(self, from, &curr_id, &incoming);
                let heuristic_hops = (cost & 0xFFFF_FFFF) as u32;
                let result = PathResult {
                    hops,
                    heuristic_hops,
                };
                return (Some(result), nodes_examined, false);
            }

            // Cap check AFTER the target check: a target found exactly on
            // the cap-th examined node is still a success. Only a node_cap
            // exhausted WITHOUT finding `to` is cap exhaustion.
            if nodes_examined >= node_cap {
                return (None, nodes_examined, true);
            }

            let hops = (cost >> 32) as u32;
            let heuristic = (cost & 0xFFFF_FFFF) as u32;

            let Some(entries) = self.adj.get(&curr_id) else {
                continue;
            };
            for entry in entries {
                if entry.kind != EdgeKind::Calls {
                    continue;
                }
                if min_confidence == Some(Confidence::Resolved)
                    && entry.confidence != Confidence::Resolved
                {
                    continue;
                }
                if !self.is_resolved_node(&entry.target) {
                    continue;
                }

                let new_hops = hops.saturating_add(1);
                let new_heuristic =
                    heuristic.saturating_add(u32::from(entry.confidence == Confidence::Heuristic));
                let new_cost = ((new_hops as u64) << 32) | (new_heuristic as u64);

                let is_better = match best_cost.get(&entry.target) {
                    None => true,
                    Some(&existing) => new_cost < existing,
                };
                if is_better {
                    best_cost.insert(entry.target.clone(), new_cost);
                    incoming.insert(
                        entry.target.clone(),
                        IncomingRecord {
                            parent: curr_id.clone(),
                            file: entry.file.clone(),
                            line: entry.line,
                            confidence: entry.confidence,
                        },
                    );
                    heap.push(Reverse((new_cost, entry.target.clone())));
                }
            }
        }

        (None, nodes_examined, false)
    }

    /// Build the `PathHop` for a node with no `Incoming` record — the
    /// source node of a path, where `entered_by` is `None` because no edge
    /// was traversed to reach it.
    fn path_hop_for_source(&self, id: &str) -> PathHop {
        let (file, line) = match self.nodes.get(id) {
            Some(node) => (PathBuf::from(&node.symbol.file), node.symbol.line),
            None => (PathBuf::new(), 0),
        };
        PathHop {
            symbol_id: id.to_string(),
            file,
            line,
            entered_by: None,
        }
    }
}

/// Walk `incoming` parent pointers from `target` back to `source`,
/// collecting one [`PathHop`] per node, then reverse so `hops[0]` is the
/// source and the last element is `target`. Free function (rather than a
/// `&self` method) so it can borrow `graph` immutably alongside the
/// `incoming` map built during the Dijkstra relaxation loop above.
fn chain_from_incoming(
    graph: &Graph,
    source: &str,
    target: &str,
    incoming: &HashMap<SymbolId, IncomingRecord>,
) -> Vec<PathHop> {
    let mut chain: Vec<PathHop> = Vec::new();
    let mut cursor: SymbolId = target.to_string();
    loop {
        if cursor == source {
            chain.push(graph.path_hop_for_source(&cursor));
            break;
        }
        let inc = incoming
            .get(&cursor)
            .expect("every non-source node on a reconstructed path has a parent pointer");
        chain.push(PathHop {
            symbol_id: cursor.clone(),
            file: inc.file.clone(),
            line: inc.line,
            entered_by: Some(inc.confidence),
        });
        cursor = inc.parent.clone();
    }
    chain.reverse();
    chain
}

/// Parent-pointer record for [`chain_from_incoming`]. Named at module scope
/// (rather than nested inside `shortest_path`) so it can appear in
/// `chain_from_incoming`'s signature.
struct IncomingRecord {
    parent: SymbolId,
    file: PathBuf,
    line: u32,
    confidence: Confidence,
}

/// One hop on a path returned by [`Graph::shortest_path`].
///
/// `symbol_id` identifies the visited node. `file`/`line` carry the call
/// site of the edge that reached this hop (same convention as
/// [`CallChain::file`]/[`CallChain::line`]) — `entered_by` is `None` only
/// for `hops[0]` (the source, reached by no edge) and
/// `Some(confidence)` for every subsequent hop, naming the resolver
/// confidence of the edge that was traversed to reach it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PathHop {
    pub symbol_id: SymbolId,
    pub file: PathBuf,
    pub line: u32,
    pub entered_by: Option<Confidence>,
}

/// Result of a successful [`Graph::shortest_path`] call.
///
/// `hops[0]` is the source symbol, `hops[hops.len() - 1]` is the target;
/// every adjacent pair is a real `Calls` edge. `heuristic_hops` is the
/// count of edges on the path whose `Confidence` was `Heuristic` — the
/// lexicographic-cost tie-break, surfaced here so a client can tell an
/// all-resolved path from one that had to fall back. `nodes_examined` and
/// `cap_reached` mirror the second/third elements of `shortest_path`'s
/// return tuple for this successful case (`cap_reached` is always `false`
/// here — reaching the cap is a distinct, path-less outcome).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PathResult {
    pub hops: Vec<PathHop>,
    pub heuristic_hops: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::{call_edge, inherit_edge, make_fg, sym};
    use code_graph_core::{Confidence, Edge, Language};

    /// Linear chain `a -> b -> c -> d` all in `/x.cpp`.
    fn linear_chain() -> Graph {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            Language::Cpp,
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

        let one = g.callers("/x.cpp:d", 1, None);
        assert_eq!(ids(&one), vec!["/x.cpp:c".to_string()]);

        let two = g.callers("/x.cpp:d", 2, None);
        assert_eq!(
            ids(&two),
            vec!["/x.cpp:b".to_string(), "/x.cpp:c".to_string()],
        );

        let three = g.callers("/x.cpp:d", 3, None);
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

        let one = g.callees("/x.cpp:a", 1, None);
        assert_eq!(ids(&one), vec!["/x.cpp:b".to_string()]);

        let three = g.callees("/x.cpp:a", 3, None);
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
            Language::Cpp,
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

        let chain = g.callees("/x.cpp:a", 2, None);
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
            Language::Cpp,
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
        let chain = g.callees("/x.cpp:a", 10, None);
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
        let zero = g.callees("/x.cpp:a", 0, None);
        let one = g.callees("/x.cpp:a", 1, None);
        assert_eq!(zero, one, "depth=0 must behave like depth=1");
        assert_eq!(zero.len(), 1);
        assert_eq!(zero[0].symbol_id, "/x.cpp:b");
    }

    // --- unknown start node ---

    #[test]
    fn bfs_unknown_symbol_returns_empty() {
        let g = linear_chain();
        assert!(g.callers("nonexistent", 5, None).is_empty());
        assert!(g.callees("nonexistent", 5, None).is_empty());
    }

    // --- CallChain payload ---

    #[test]
    fn call_chain_carries_file_and_line() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            Language::Cpp,
            vec![
                sym("a", SymbolKind::Function, "/x.cpp"),
                sym("b", SymbolKind::Function, "/x.cpp"),
            ],
            vec![call_edge("/x.cpp:a", "/x.cpp:b", "/x.cpp", 42)],
        ));

        let chain = g.callees("/x.cpp:a", 1, None);
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
            Language::Cpp,
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

        let chain = g.callers("/x.cpp:b", 5, None);
        let names = ids(&chain);
        assert_eq!(
            names,
            vec!["/x.cpp:a".to_string()],
            "only the Calls source is reported; Inherits source is filtered out"
        );
    }

    // --- resolved-only filter (design Decision 7) ---

    #[test]
    fn callees_filters_unresolved_targets() {
        // `A` has Calls edges to a project symbol (`B`, present in
        // `nodes`) and three bare unresolved tokens (`Ok`, `info`,
        // `to_string` — NOT present in `nodes`). The unresolved targets
        // must not appear in the result; only `B` survives.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.rs",
            Language::Rust,
            vec![
                sym("A", SymbolKind::Function, "/x.rs"),
                sym("B", SymbolKind::Function, "/x.rs"),
            ],
            vec![
                call_edge("/x.rs:A", "/x.rs:B", "/x.rs", 1),
                call_edge("/x.rs:A", "Ok", "/x.rs", 2),
                call_edge("/x.rs:A", "info", "/x.rs", 3),
                call_edge("/x.rs:A", "to_string", "/x.rs", 4),
            ],
        ));

        let chain = g.callees("/x.rs:A", 1, None);
        assert_eq!(
            ids(&chain),
            vec!["/x.rs:B".to_string()],
            "only the resolved project symbol survives: {chain:?}",
        );
        let raw_ids: Vec<&str> = chain.iter().map(|c| c.symbol_id.as_str()).collect();
        for tok in ["Ok", "info", "to_string"] {
            assert!(
                !raw_ids.contains(&tok),
                "unresolved token {tok:?} must not appear in callees: {raw_ids:?}",
            );
        }
    }

    #[test]
    fn callees_unresolved_token_does_not_pollute_visited_at_depth_2() {
        // Two-arm fixture exercising the depth->=2 visited-pollution
        // failure mode. The point: if the filter ran in the handler
        // (post-BFS) rather than in `Graph::bfs`, the BFS would still
        // insert the unresolved token `Ok` into `visited` on the first
        // arm. The second arm — which legitimately reaches resolved
        // descendants by way of a DIFFERENT path — would then have its
        // `visited`-membership checks falsely satisfied for the shared
        // token, distorting depth attribution for the resolved
        // neighbors reached after it. Filtering inside `bfs` keeps `Ok`
        // out of `visited` entirely, so both arms walk their resolved
        // sub-trees faithfully.
        //
        // Edges from the start `Entry`:
        //     Entry -> Ok           (unresolved; must NOT enter visited)
        //     Entry -> B            (resolved)
        //     B     -> C            (resolved)
        //     Entry -> D            (resolved)
        //     D     -> Ok           (unresolved; reached by a 2nd arm)
        //     D     -> C            (resolved; second arm to C — would
        //                            be suppressed if `Ok` had polluted
        //                            visited and we did handler-level
        //                            filtering)
        //
        // Depth=3 walk from `Entry` must yield {B, C, D} as resolved
        // descendants. `Ok` must be absent. `C` is reached exactly once
        // (it is a true diamond apex), but the visit must succeed —
        // proving that the unresolved-token detour didn't trip the
        // dedup guard for a resolved node downstream.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.rs",
            Language::Rust,
            vec![
                sym("Entry", SymbolKind::Function, "/x.rs"),
                sym("B", SymbolKind::Function, "/x.rs"),
                sym("C", SymbolKind::Function, "/x.rs"),
                sym("D", SymbolKind::Function, "/x.rs"),
            ],
            vec![
                call_edge("/x.rs:Entry", "Ok", "/x.rs", 1),
                call_edge("/x.rs:Entry", "/x.rs:B", "/x.rs", 2),
                call_edge("/x.rs:B", "/x.rs:C", "/x.rs", 3),
                call_edge("/x.rs:Entry", "/x.rs:D", "/x.rs", 4),
                call_edge("/x.rs:D", "Ok", "/x.rs", 5),
                call_edge("/x.rs:D", "/x.rs:C", "/x.rs", 6),
            ],
        ));

        let chain = g.callees("/x.rs:Entry", 3, None);
        let resolved_ids = ids(&chain);
        assert_eq!(
            resolved_ids,
            vec![
                "/x.rs:B".to_string(),
                "/x.rs:C".to_string(),
                "/x.rs:D".to_string(),
            ],
            "all resolved descendants reached; no Ok: {chain:?}",
        );
        // C must be present exactly once (BFS dedup on resolved IDs).
        let c_count = chain.iter().filter(|c| c.symbol_id == "/x.rs:C").count();
        assert_eq!(c_count, 1, "C reached exactly once: {chain:?}");

        // Per-hop depth assertions. The commit message that introduced
        // the BFS-side filter (5c92c0a) named depth-attribution
        // distortion as the failure mode — a visited-set polluted by an
        // unresolved token would short-circuit a later legitimate visit
        // and either drop the resolved neighbor entirely (the identity
        // assertion above already pins this) OR record it at the wrong
        // hop count. The depth checks below pin the latter explicitly,
        // so a regression that smuggles `Ok` back into `visited` is
        // caught even if some other change masks the identity-set
        // failure.
        let depth_of =
            |id: &str| -> Option<u32> { chain.iter().find(|c| c.symbol_id == id).map(|c| c.depth) };
        assert_eq!(
            depth_of("/x.rs:B"),
            Some(1),
            "B is a direct callee of Entry: depth 1 expected: {chain:?}",
        );
        assert_eq!(
            depth_of("/x.rs:D"),
            Some(1),
            "D is a direct callee of Entry: depth 1 expected: {chain:?}",
        );
        // C is reachable from BOTH first-level arms; whichever wins the
        // visited-insert race (HashMap iteration order over adj[Entry]'s
        // Vec is deterministic insertion-order but the post-fix BFS pops
        // (B,1) and (D,1) before (C,?), so C is always reached AT depth
        // 2. We do NOT pin the parent (deduped by `visited`); we only
        // pin the depth attribution, which is the discriminator.
        assert_eq!(
            depth_of("/x.rs:C"),
            Some(2),
            "C is reached one hop past a first-level resolved arm: \
             depth 2 expected: {chain:?}",
        );
    }

    #[test]
    fn callees_all_unresolved_returns_empty_chain_set() {
        // `F` calls only unresolved tokens — every callee is a raw
        // identifier that doesn't bind to a Symbol in `nodes`. The BFS
        // returns an empty Vec; the handler then surfaces the empty
        // `Page<CallChain>` envelope (the existing "callable with no
        // callees" path), NOT a tool error.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.rs",
            Language::Rust,
            vec![sym("F", SymbolKind::Function, "/x.rs")],
            vec![
                call_edge("/x.rs:F", "Ok", "/x.rs", 1),
                call_edge("/x.rs:F", "Err", "/x.rs", 2),
                call_edge("/x.rs:F", "info", "/x.rs", 3),
            ],
        ));

        let chain = g.callees("/x.rs:F", 2, None);
        assert!(
            chain.is_empty(),
            "every callee is unresolved -> empty BFS result: {chain:?}",
        );
    }

    #[test]
    fn callers_filters_unresolved_targets() {
        // Symmetric to `callees_filters_unresolved_targets` on the
        // callers side. `S` has reverse-adjacency entries for two
        // resolved callers (`R1`, `R2`) and one unresolved bare token
        // (`some_macro` — a raw caller-side identifier whose definition
        // isn't in `nodes`, e.g. a macro invocation captured as a call
        // edge from a phantom source). `Graph::callers("S")` must
        // return only the two resolved chains.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.rs",
            Language::Rust,
            vec![
                sym("S", SymbolKind::Function, "/x.rs"),
                sym("R1", SymbolKind::Function, "/x.rs"),
                sym("R2", SymbolKind::Function, "/x.rs"),
            ],
            vec![
                call_edge("/x.rs:R1", "/x.rs:S", "/x.rs", 1),
                call_edge("/x.rs:R2", "/x.rs:S", "/x.rs", 2),
                call_edge("some_macro", "/x.rs:S", "/x.rs", 3),
            ],
        ));

        let chain = g.callers("/x.rs:S", 1, None);
        assert_eq!(
            ids(&chain),
            vec!["/x.rs:R1".to_string(), "/x.rs:R2".to_string()],
            "only resolved callers survive: {chain:?}",
        );
        let raw_ids: Vec<&str> = chain.iter().map(|c| c.symbol_id.as_str()).collect();
        assert!(
            !raw_ids.contains(&"some_macro"),
            "unresolved caller token must not appear: {raw_ids:?}",
        );
    }

    // --- orphans ---

    #[test]
    fn orphans_default_returns_only_callables() {
        // Default mode returns all callables with no incoming Calls edges.
        // Both `unused` and `caller` qualify (Functions, no callers).
        // `used` is excluded — it has an incoming Call from `caller`.
        // `MyClass` is excluded by the default kind filter (callables only).
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            Language::Cpp,
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
            Language::Cpp,
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
            Language::Cpp,
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
        // Build the include edges inline with distinctive, non-zero
        // source lines (the `include_edge` fixture hardcodes `line: 0`,
        // which would make a `line` assertion vacuous). This pins that
        // `file_dependencies` reports the include directive's real source
        // line, not a defaulted placeholder.
        let include = |to: &str, line: u32| Edge {
            from: "/a.cpp".to_string(),
            to: to.to_string(),
            kind: EdgeKind::Includes,
            file: "/a.cpp".to_string(),
            line,
            confidence: Confidence::Resolved,
        };
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![],
            vec![include("/utils.h", 4), include("/types.h", 9)],
        ));

        let deps = g.file_dependencies(&PathBuf::from("/a.cpp"));
        // Assert BOTH path AND line for every entry: a regression that
        // dropped the line back to 0 would fail here.
        assert_eq!(
            deps,
            vec![
                IncludeEntry {
                    path: PathBuf::from("/utils.h"),
                    line: 4,
                },
                IncludeEntry {
                    path: PathBuf::from("/types.h"),
                    line: 9,
                },
            ],
        );
    }

    #[test]
    fn file_dependencies_unknown_path_returns_empty() {
        let g = Graph::new();
        let deps = g.file_dependencies(&PathBuf::from("/never-merged.cpp"));
        // Vec, never None — JSON must serialize as `[]`, not `null`.
        assert!(deps.is_empty());
    }

    // --- shortest_path (1.2) ------------------------------------------

    fn heuristic_edge(from: &str, to: &str, file: &str, line: u32) -> Edge {
        Edge {
            from: from.to_string(),
            to: to.to_string(),
            kind: EdgeKind::Calls,
            file: file.to_string(),
            line,
            confidence: Confidence::Heuristic,
        }
    }

    fn hop_ids(result: &PathResult) -> Vec<String> {
        result.hops.iter().map(|h| h.symbol_id.clone()).collect()
    }

    #[test]
    fn shortest_path_connected_pair_returns_real_edge_chain() {
        // a -> b -> c -> d, plus a distractor a -> x (dead end).
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            Language::Cpp,
            vec![
                sym("a", SymbolKind::Function, "/x.cpp"),
                sym("b", SymbolKind::Function, "/x.cpp"),
                sym("c", SymbolKind::Function, "/x.cpp"),
                sym("d", SymbolKind::Function, "/x.cpp"),
                sym("x", SymbolKind::Function, "/x.cpp"),
            ],
            vec![
                call_edge("/x.cpp:a", "/x.cpp:b", "/x.cpp", 1),
                call_edge("/x.cpp:b", "/x.cpp:c", "/x.cpp", 2),
                call_edge("/x.cpp:c", "/x.cpp:d", "/x.cpp", 3),
                call_edge("/x.cpp:a", "/x.cpp:x", "/x.cpp", 4),
            ],
        ));

        let (result, nodes_examined, cap_reached) =
            g.shortest_path("/x.cpp:a", "/x.cpp:d", 1000, None);
        assert!(!cap_reached);
        assert!(nodes_examined > 0);
        let result = result.expect("a connected pair must find a path");
        let ids = hop_ids(&result);
        assert_eq!(
            ids,
            vec![
                "/x.cpp:a".to_string(),
                "/x.cpp:b".to_string(),
                "/x.cpp:c".to_string(),
                "/x.cpp:d".to_string(),
            ]
        );
        assert_eq!(result.hops.first().unwrap().symbol_id, "/x.cpp:a");
        assert_eq!(result.hops.last().unwrap().symbol_id, "/x.cpp:d");
        assert!(
            result.hops[0].entered_by.is_none(),
            "source has no incoming edge"
        );
        for hop in &result.hops[1..] {
            assert_eq!(hop.entered_by, Some(Confidence::Resolved));
        }

        // Every adjacent pair is a real edge: check via the forward
        // adjacency list directly.
        for window in result.hops.windows(2) {
            let (from, to) = (&window[0].symbol_id, &window[1].symbol_id);
            let has_edge = g
                .adj
                .get(from)
                .is_some_and(|entries| entries.iter().any(|e| &e.target == to));
            assert!(has_edge, "{from} -> {to} must be a real edge");
        }
    }

    #[test]
    fn shortest_path_unconnected_pair_returns_none() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            Language::Cpp,
            vec![
                sym("a", SymbolKind::Function, "/x.cpp"),
                sym("b", SymbolKind::Function, "/x.cpp"),
            ],
            vec![],
        ));

        let (result, _nodes_examined, cap_reached) =
            g.shortest_path("/x.cpp:a", "/x.cpp:b", 1000, None);
        assert!(result.is_none());
        assert!(!cap_reached, "no path exists, not a cap exhaustion");
    }

    #[test]
    fn shortest_path_equal_hop_prefers_all_resolved() {
        // Two 2-hop paths from `start` to `target`:
        //   start -> r1 -> target   (both edges Resolved)
        //   start -> h1 -> target   (both edges Heuristic)
        // Equal hop count (2); only the all-resolved path may win.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            Language::Cpp,
            vec![
                sym("start", SymbolKind::Function, "/x.cpp"),
                sym("r1", SymbolKind::Function, "/x.cpp"),
                sym("h1", SymbolKind::Function, "/x.cpp"),
                sym("target", SymbolKind::Function, "/x.cpp"),
            ],
            vec![
                call_edge("/x.cpp:start", "/x.cpp:r1", "/x.cpp", 1),
                call_edge("/x.cpp:r1", "/x.cpp:target", "/x.cpp", 2),
                heuristic_edge("/x.cpp:start", "/x.cpp:h1", "/x.cpp", 3),
                heuristic_edge("/x.cpp:h1", "/x.cpp:target", "/x.cpp", 4),
            ],
        ));

        let (result, _nodes_examined, cap_reached) =
            g.shortest_path("/x.cpp:start", "/x.cpp:target", 1000, None);
        assert!(!cap_reached);
        let result = result.expect("path must exist");
        assert_eq!(
            hop_ids(&result),
            vec![
                "/x.cpp:start".to_string(),
                "/x.cpp:r1".to_string(),
                "/x.cpp:target".to_string(),
            ],
            "the all-resolved path must win over the equal-hop heuristic path"
        );
        assert_eq!(result.heuristic_hops, 0);
    }

    #[test]
    fn shortest_path_node_cap_smaller_than_graph_returns_cap_reached() {
        // Linear chain long enough that a cap of 2 cannot possibly reach
        // the target.
        let mut g = Graph::new();
        let mut symbols = Vec::new();
        let mut edges = Vec::new();
        for i in 0..10 {
            symbols.push(sym(&format!("n{i}"), SymbolKind::Function, "/x.cpp"));
            if i > 0 {
                edges.push(call_edge(
                    &format!("/x.cpp:n{}", i - 1),
                    &format!("/x.cpp:n{i}"),
                    "/x.cpp",
                    i as u32,
                ));
            }
        }
        g.merge_file_graph(make_fg("/x.cpp", Language::Cpp, symbols, edges));

        let (result, nodes_examined, cap_reached) =
            g.shortest_path("/x.cpp:n0", "/x.cpp:n9", 2, None);
        assert!(result.is_none(), "must never return a partial path");
        assert!(cap_reached, "cap must be reported as reached");
        assert_eq!(nodes_examined, 2);
    }

    #[test]
    fn shortest_path_source_equals_target_is_single_hop_zero_distance() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            Language::Cpp,
            vec![sym("a", SymbolKind::Function, "/x.cpp")],
            vec![],
        ));

        let (result, nodes_examined, cap_reached) =
            g.shortest_path("/x.cpp:a", "/x.cpp:a", 1000, None);
        assert!(!cap_reached);
        assert_eq!(nodes_examined, 1);
        let result = result.expect("from == to must succeed");
        assert_eq!(result.hops.len(), 1);
        assert_eq!(result.hops[0].symbol_id, "/x.cpp:a");
        assert_eq!(result.hops[0].entered_by, None);
        assert_eq!(result.heuristic_hops, 0);
    }

    #[test]
    fn shortest_path_unknown_endpoint_returns_none_zero_false() {
        let g = linear_chain();
        let (result, nodes_examined, cap_reached) =
            g.shortest_path("nonexistent", "/x.cpp:a", 1000, None);
        assert!(result.is_none());
        assert_eq!(nodes_examined, 0);
        assert!(!cap_reached);

        let (result2, nodes_examined2, cap_reached2) =
            g.shortest_path("/x.cpp:a", "nonexistent", 1000, None);
        assert!(result2.is_none());
        assert_eq!(nodes_examined2, 0);
        assert!(!cap_reached2);
    }

    #[test]
    fn shortest_path_respects_min_confidence_resolved() {
        // start -> h1 -> target is the ONLY path, and it's Heuristic. With
        // min_confidence = Resolved, the Heuristic hop must be pruned
        // before it enters the frontier, so no path is found.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            Language::Cpp,
            vec![
                sym("start", SymbolKind::Function, "/x.cpp"),
                sym("h1", SymbolKind::Function, "/x.cpp"),
                sym("target", SymbolKind::Function, "/x.cpp"),
            ],
            vec![
                heuristic_edge("/x.cpp:start", "/x.cpp:h1", "/x.cpp", 1),
                heuristic_edge("/x.cpp:h1", "/x.cpp:target", "/x.cpp", 2),
            ],
        ));

        let (result, _nodes_examined, _cap_reached) = g.shortest_path(
            "/x.cpp:start",
            "/x.cpp:target",
            1000,
            Some(Confidence::Resolved),
        );
        assert!(
            result.is_none(),
            "the only path is Heuristic; must be filtered"
        );
    }
}
