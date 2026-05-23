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

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use code_graph_core::{EdgeKind, Symbol, SymbolId, SymbolKind};

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
    pub fn callers(&self, id: &str, depth: u32) -> Vec<CallChain> {
        self.bfs(id, depth, &self.radj, EdgeKind::Calls)
    }

    /// Symbols called by `id`, up to `depth` hops away. BFS over the
    /// forward adjacency list filtered by `EdgeKind::Calls`. `depth = 0`
    /// is normalized to 1 (see [`Graph::callers`]).
    pub fn callees(&self, id: &str, depth: u32) -> Vec<CallChain> {
        self.bfs(id, depth, &self.adj, EdgeKind::Calls)
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
            Language::Cpp,
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

        let chain = g.callers("/x.cpp:b", 5);
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

        let chain = g.callees("/x.rs:A", 1);
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

        let chain = g.callees("/x.rs:Entry", 3);
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

        let chain = g.callees("/x.rs:F", 2);
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

        let chain = g.callers("/x.rs:S", 1);
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
}
