//! Cycle detection (iterative Tarjan SCC) and the diamond-safe class
//! hierarchy traversal.
//!
//! Mirrors the Go reference at `internal/graph/algorithms.go`
//! (`DetectCycles`, `tarjanSCC`) and `internal/graph/graph.go` lines 429–486
//! (`ClassHierarchy`, `buildHierarchy`). The Go binary uses a recursive
//! Tarjan; this Rust port uses an **iterative** Tarjan with an explicit
//! `Vec` worklist so deep include graphs cannot overflow the call stack.
//! Class-hierarchy traversal stays recursive — class graphs are realistically
//! a few dozen levels deep at most, so the stack-safety concern is specific
//! to file-include cycles rather than inheritance trees.
//!
//! The key correctness property carried forward from the Go regression test
//! (`Designs/LLMOptimization/notes/01-Implementation.md`) is that the class
//! hierarchy uses a **per-DFS-path** visited set — not a global one — so
//! diamond inheritance fully expands a shared ancestor under both arms.
//! See [`Graph::class_hierarchy`] and the `class_hierarchy_diamond_4_level_fixture`
//! test below.
//!
//! Locking is not handled in this module. Task 2.6 wraps [`Graph`] in
//! `parking_lot::RwLock`; until then these methods take `&self` and rely
//! on the caller for synchronization.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use codegraph_core::{EdgeKind, SymbolKind};

use crate::Graph;

/// One node in a class-inheritance tree returned by
/// [`Graph::class_hierarchy`].
///
/// `bases` are the symbols this class inherits from (walked via the
/// forward `Inherits` edges out of the queried name). `derived` are the
/// symbols that inherit from this class (walked via the reverse
/// `Inherits` edges into the queried name). Both fields are skipped from
/// JSON output when empty so leaf nodes serialize as just `{ "name": ... }`,
/// matching the Go shape's `omitempty` tags.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HierarchyNode {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bases: Vec<HierarchyNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived: Vec<HierarchyNode>,
}

/// One frame on the iterative-Tarjan worklist.
///
/// `Enter` is the first visit to a node — initialize indices, push onto
/// the SCC stack, then schedule a `Process` frame to walk neighbors.
/// `Process(node, parent, neighbor_idx)` is a continuation that resumes
/// neighbor iteration at `neighbor_idx`. When `neighbor_idx` reaches the
/// end of the neighbor list, we finalize the node (root check + SCC
/// emission) and propagate `lowlinks[node]` into the parent (if any) —
/// this is the step the recursive form expresses as
/// `lowlinks[v] = min(lowlinks[v], lowlinks[w])` after each recursive call.
enum Step {
    Enter(PathBuf, Option<PathBuf>),
    Process(PathBuf, Option<PathBuf>, usize),
}

impl Graph {
    /// Strongly connected components of size > 1 in the file include
    /// graph. Each SCC is a `Vec<PathBuf>` of files that mutually
    /// `#include` each other (directly or transitively).
    ///
    /// Implementation note: this is an **iterative** Tarjan SCC. The
    /// recursive form (matching the Go reference) overflows on
    /// pathological include chains; the iterative version uses an
    /// explicit `Vec<Step>` worklist so the only depth bound is the
    /// heap size. Single-node SCCs (every node is trivially its own
    /// SCC) are filtered out — only true cycles are returned.
    pub fn detect_cycles(&self) -> Vec<Vec<PathBuf>> {
        // Collect every file that participates in the include graph,
        // both as a source and as a target. Mirrors the Go reference's
        // `allFiles` map.
        let mut all_files: HashSet<PathBuf> = HashSet::new();
        for (from, tos) in &self.includes {
            all_files.insert(from.clone());
            for to in tos {
                all_files.insert(to.clone());
            }
        }

        tarjan_scc(&all_files, &self.includes)
    }

    /// Inheritance tree rooted at `name`.
    ///
    /// Returns `None` if no symbol with the given name exists in
    /// the graph with a class-like kind (`Class`, `Struct`, `Interface`,
    /// or `Trait`). The Go reference checks only `Class`/`Struct`; this
    /// Rust port widens the filter to include `Interface` and `Trait` so
    /// Rust traits and Go interfaces (Phase 1's added kinds) resolve as
    /// hierarchy roots without a separate API.
    ///
    /// `depth = 0` is normalized to `1` to match the Go behavior — the
    /// Go binary uses `if depth <= 0 { depth = 1 }` so an agent passing
    /// `0` gets the same result as `1` rather than an empty tree.
    ///
    /// **Diamond inheritance**: the DFS uses a *per-path* `on_path` set
    /// rather than a global visited set. This is essential — when a
    /// shared ancestor is reached via two different paths (e.g.
    /// `Derived → MixinA → Base` and `Derived → MixinB → Base`), each
    /// arm must fully expand `Base` independently. A global visited set
    /// would short-circuit the second visit and silently truncate the
    /// hierarchy. See the `class_hierarchy_diamond_4_level_fixture`
    /// test for the regression fixture.
    pub fn class_hierarchy(&self, name: &str, depth: u32) -> Option<HierarchyNode> {
        // Verify the class exists with a class-like kind.
        let exists = self.nodes.values().any(|node| {
            node.symbol.name == name
                && matches!(
                    node.symbol.kind,
                    SymbolKind::Class
                        | SymbolKind::Struct
                        | SymbolKind::Interface
                        | SymbolKind::Trait
                )
        });
        if !exists {
            return None;
        }

        let depth = if depth == 0 { 1 } else { depth };

        let mut on_path: HashSet<String> = HashSet::new();
        Some(self.build_hierarchy(name, depth, &mut on_path))
    }

    /// Recursive helper for [`Graph::class_hierarchy`].
    ///
    /// Recursion is acceptable here (unlike the iterative-Tarjan
    /// requirement) because class hierarchies are realistically a few
    /// dozen levels deep at worst — the stack-safety concern only
    /// applies to file-include cycles which can chain across thousands
    /// of headers. The plan only requires Tarjan to be iterative.
    ///
    /// `on_path` is mutated in lockstep with the recursion: the name is
    /// inserted before recursing into children and removed after both
    /// the bases and derived loops complete. This is the diamond fix —
    /// siblings can each fully expand the same ancestor because the set
    /// only carries the *current path*, not every previously seen node.
    fn build_hierarchy(
        &self,
        name: &str,
        depth: u32,
        on_path: &mut HashSet<String>,
    ) -> HierarchyNode {
        // Cycle base case: this name is on the current DFS path. Emit a
        // bare leaf so the caller sees the name without recursing
        // forever. Matches Go's `if onPath[name] { return &HierarchyNode{Name: name} }`.
        if on_path.contains(name) {
            return HierarchyNode {
                name: name.to_string(),
                bases: Vec::new(),
                derived: Vec::new(),
            };
        }

        let mut node = HierarchyNode {
            name: name.to_string(),
            bases: Vec::new(),
            derived: Vec::new(),
        };

        if depth == 0 {
            return node;
        }

        on_path.insert(name.to_string());

        // Panic-safe `on_path` cleanup: matches the Go reference's
        // `defer delete(onPath, name)` semantic. If either recursive
        // loop below panics, `PopGuard::drop` still runs and removes
        // `name`, so a sibling DFS path on the unwound stack doesn't
        // see a stale entry. The workspace uses `panic = unwind`
        // (default), so this RAII guard is the way to guarantee
        // unconditional cleanup without `unsafe`.
        struct PopGuard<'a> {
            set: &'a mut HashSet<String>,
            name: String,
        }
        impl Drop for PopGuard<'_> {
            fn drop(&mut self) {
                self.set.remove(&self.name);
            }
        }
        let guard = PopGuard {
            set: on_path,
            name: name.to_string(),
        };

        // Targets are collected up front so the immutable borrow on
        // `self.adj` / `self.radj` ends before each recursive call; the
        // recursion only needs `&self`, but cloning the names releases
        // the slice borrow and lets the loop pass `guard.set` (auto-
        // reborrowed as `&mut HashSet<String>`) through the call.
        if let Some(entries) = self.adj.get(name) {
            let bases: Vec<String> = entries
                .iter()
                .filter(|e| e.kind == EdgeKind::Inherits)
                .map(|e| e.target.clone())
                .collect();
            for target in bases {
                node.bases
                    .push(self.build_hierarchy(&target, depth - 1, guard.set));
            }
        }

        if let Some(entries) = self.radj.get(name) {
            let derived: Vec<String> = entries
                .iter()
                .filter(|e| e.kind == EdgeKind::Inherits)
                .map(|e| e.target.clone())
                .collect();
            for target in derived {
                node.derived
                    .push(self.build_hierarchy(&target, depth - 1, guard.set));
            }
        }

        // `guard` drops here, removing `name` from `on_path`. Drop also
        // runs along the panic unwind path if either recursion above
        // panicked, which is the whole point of the guard struct.
        drop(guard);
        node
    }
}

/// Iterative Tarjan SCC over a directed graph keyed by `PathBuf`.
///
/// The recursive textbook form (and the Go reference) does:
///
/// ```text
/// strongconnect(v):
///   indices[v] = lowlinks[v] = index++
///   push v on SCC stack
///   for w in adj[v]:
///     if w not visited:
///       strongconnect(w)
///       lowlinks[v] = min(lowlinks[v], lowlinks[w])  // <-- post-recursion fixup
///     elif w on stack:
///       lowlinks[v] = min(lowlinks[v], indices[w])
///   if lowlinks[v] == indices[v]:
///     pop SCC down to v
/// ```
///
/// To make this iterative we replace the recursion with a `Vec<Step>`
/// worklist. The non-trivial bit is the *post-recursion fixup* — when
/// processing of a child `w` finishes, the parent `v`'s `lowlinks[v]`
/// must absorb `lowlinks[w]`. We track each frame's parent explicitly
/// (`Option<PathBuf>` in [`Step::Process`]) so when a node finalizes we
/// know which lowlink to update.
///
/// The frame ordering invariant: for each node we push exactly one
/// `Enter`. The `Enter` handler immediately pushes a corresponding
/// `Process(_, parent, 0)`. Each `Process(_, parent, idx)` either:
/// 1. has more neighbors to walk → push `Process(_, parent, idx+1)`
///    *first*, then either push `Enter(neighbor, Some(self))` to recurse
///    or update lowlink in-place if neighbor is on the SCC stack;
/// 2. is exhausted → finalize (root check + SCC pop) and propagate the
///    completed lowlink into the parent's lowlink (if any).
///
/// Because the `Process` continuation is pushed before the child's
/// `Enter`, the child's full processing happens between the two times
/// the parent's `Process` frame is popped — exactly mirroring the
/// recursion order.
fn tarjan_scc(nodes: &HashSet<PathBuf>, adj: &HashMap<PathBuf, Vec<PathBuf>>) -> Vec<Vec<PathBuf>> {
    let mut index_counter: i64 = 0;
    let mut scc_stack: Vec<PathBuf> = Vec::new();
    let mut on_stack: HashSet<PathBuf> = HashSet::new();
    let mut indices: HashMap<PathBuf, i64> = HashMap::new();
    let mut lowlinks: HashMap<PathBuf, i64> = HashMap::new();
    let mut result: Vec<Vec<PathBuf>> = Vec::new();

    // Iteration order over a HashSet is unspecified; for deterministic
    // test output we sort. The output ordering of SCCs is incidental
    // (tests assert on contents, not order), but stable iteration makes
    // debugging easier and is cheap.
    let mut start_order: Vec<&PathBuf> = nodes.iter().collect();
    start_order.sort();

    for start in start_order {
        if indices.contains_key(start) {
            continue;
        }

        let mut work: Vec<Step> = vec![Step::Enter(start.clone(), None)];

        while let Some(step) = work.pop() {
            match step {
                Step::Enter(v, parent) => {
                    // First visit: assign DFS index/lowlink and push onto
                    // the SCC stack.
                    indices.insert(v.clone(), index_counter);
                    lowlinks.insert(v.clone(), index_counter);
                    index_counter += 1;
                    scc_stack.push(v.clone());
                    on_stack.insert(v.clone());
                    work.push(Step::Process(v, parent, 0));
                }
                Step::Process(v, parent, neighbor_idx) => {
                    let neighbors: &[PathBuf] = adj.get(&v).map(Vec::as_slice).unwrap_or(&[]);

                    if neighbor_idx < neighbors.len() {
                        let w = neighbors[neighbor_idx].clone();
                        // Schedule the next iteration of v's neighbor loop.
                        // This MUST be pushed before the recurse-into-w
                        // step, so that when w's processing completes the
                        // worklist resumes v at neighbor_idx + 1.
                        work.push(Step::Process(v.clone(), parent.clone(), neighbor_idx + 1));

                        if !indices.contains_key(&w) {
                            // Recurse: process w fully before resuming v.
                            work.push(Step::Enter(w, Some(v)));
                        } else if on_stack.contains(&w) {
                            // Back-edge to a node on the current DFS
                            // stack: update v's lowlink to indices[w].
                            // (This is the `lowlinks[v] = min(lowlinks[v],
                            // indices[w])` branch of recursive Tarjan.)
                            let w_index = indices[&w];
                            let v_low = lowlinks[&v];
                            if w_index < v_low {
                                lowlinks.insert(v, w_index);
                            }
                        }
                        // Else: w is visited but not on the stack — it
                        // belongs to a different (already-emitted) SCC.
                        // No lowlink update needed.
                    } else {
                        // All neighbors of v processed. If v is a root
                        // node (lowlinks[v] == indices[v]), pop the SCC.
                        if lowlinks[&v] == indices[&v] {
                            let mut scc: Vec<PathBuf> = Vec::new();
                            loop {
                                let w = scc_stack
                                    .pop()
                                    .expect("scc_stack non-empty while popping SCC");
                                on_stack.remove(&w);
                                let is_root = w == v;
                                scc.push(w);
                                if is_root {
                                    break;
                                }
                            }
                            // Only true cycles (size > 1) are reported —
                            // a node that is its own trivial SCC is not
                            // a cycle.
                            if scc.len() > 1 {
                                result.push(scc);
                            }
                        }

                        // Propagate v's lowlink up to its parent. This
                        // is the iterative analog of the recursive
                        // `lowlinks[parent] = min(lowlinks[parent], lowlinks[v])`
                        // post-recursion fixup.
                        if let Some(p) = parent {
                            let v_low = lowlinks[&v];
                            let p_low = lowlinks[&p];
                            if v_low < p_low {
                                lowlinks.insert(p, v_low);
                            }
                        }
                    }
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::{include_edge, inherit_edge, make_fg, sym};
    use codegraph_core::{Edge, Language};

    /// Build a graph whose include map exactly mirrors `edges`. Each
    /// edge must be `(from_file, to_file)`. We synthesize one FileGraph
    /// per source file so `merge_file_graph` populates `self.includes`.
    fn graph_from_includes(edges: &[(&str, &str)]) -> Graph {
        let mut g = Graph::new();
        // Group edges by source file so each source path produces exactly
        // one FileGraph (re-merging the same path would otherwise wipe
        // the previous include batch).
        let mut by_source: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for (from, to) in edges {
            by_source
                .entry((*from).to_string())
                .or_default()
                .push(((*from).to_string(), (*to).to_string()));
        }
        for (from, edges_for_from) in by_source {
            // Pass `from` as the file argument to match the existing
            // `algorithms.rs` semantic where each include edge is
            // attributed to its source file.
            let edge_objs: Vec<Edge> = edges_for_from
                .iter()
                .map(|(f, t)| include_edge(f, t, f))
                .collect();
            g.merge_file_graph(make_fg(&from, Language::Cpp, vec![], edge_objs));
        }
        g
    }

    fn scc_contents(scc: &[PathBuf]) -> Vec<String> {
        let mut v: Vec<String> = scc
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    // --- iterative Tarjan SCC ---

    #[test]
    fn tarjan_acyclic_graph() {
        // a -> b -> c, a -> d. No cycles; no SCCs of size > 1.
        let g = graph_from_includes(&[("/a", "/b"), ("/b", "/c"), ("/a", "/d")]);
        let cycles = g.detect_cycles();
        assert!(
            cycles.is_empty(),
            "acyclic graph reports no cycles: {cycles:?}"
        );
    }

    #[test]
    fn tarjan_two_node_cycle() {
        // a -> b -> a. One SCC of size 2.
        let g = graph_from_includes(&[("/a", "/b"), ("/b", "/a")]);
        let cycles = g.detect_cycles();
        assert_eq!(cycles.len(), 1);
        assert_eq!(
            scc_contents(&cycles[0]),
            vec!["/a".to_string(), "/b".to_string()]
        );
    }

    #[test]
    fn tarjan_three_node_cycle() {
        // a -> b -> c -> a. One SCC of size 3.
        let g = graph_from_includes(&[("/a", "/b"), ("/b", "/c"), ("/c", "/a")]);
        let cycles = g.detect_cycles();
        assert_eq!(cycles.len(), 1);
        assert_eq!(
            scc_contents(&cycles[0]),
            vec!["/a".to_string(), "/b".to_string(), "/c".to_string()],
        );
    }

    #[test]
    fn tarjan_mixed_graph() {
        // 5 nodes: a -> b -> c -> a (3-cycle); d -> e (acyclic chain).
        // Exactly one SCC of size 3 expected.
        let g = graph_from_includes(&[("/a", "/b"), ("/b", "/c"), ("/c", "/a"), ("/d", "/e")]);
        let cycles = g.detect_cycles();
        assert_eq!(cycles.len(), 1, "mixed graph: only the 3-cycle is reported");
        assert_eq!(cycles[0].len(), 3);
        assert_eq!(
            scc_contents(&cycles[0]),
            vec!["/a".to_string(), "/b".to_string(), "/c".to_string()],
        );
    }

    #[test]
    fn tarjan_self_loop_not_reported() {
        // a -> a. Trivially an SCC of size 1; size > 1 filter drops it.
        let g = graph_from_includes(&[("/a", "/a")]);
        let cycles = g.detect_cycles();
        assert!(
            cycles.is_empty(),
            "single-node self-loop is a size-1 SCC and is not a cycle: {cycles:?}",
        );
    }

    #[test]
    fn tarjan_two_separate_cycles() {
        // Two disjoint 2-cycles: {a, b} and {c, d}.
        let g = graph_from_includes(&[("/a", "/b"), ("/b", "/a"), ("/c", "/d"), ("/d", "/c")]);
        let cycles = g.detect_cycles();
        assert_eq!(cycles.len(), 2);
        // Both SCCs should have exactly 2 members.
        for scc in &cycles {
            assert_eq!(scc.len(), 2);
        }
        // Collect both SCCs' contents and check both expected pairs are present.
        let mut all: Vec<Vec<String>> = cycles.iter().map(|s| scc_contents(s)).collect();
        all.sort();
        assert_eq!(
            all,
            vec![
                vec!["/a".to_string(), "/b".to_string()],
                vec!["/c".to_string(), "/d".to_string()],
            ],
        );
    }

    /// Exercises the iterative Tarjan branch where a neighbor `w` is
    /// already visited but **not** on the SCC stack — i.e. it belongs
    /// to a different, already-emitted SCC. The recursive textbook form
    /// would update `lowlinks[v] = min(lowlinks[v], indices[w])` only
    /// when `w` is on the stack; the iterative port at lines 296–298
    /// of this file relies on the same predicate to avoid corrupting a
    /// finalized SCC. Without this test, the cross-SCC branch is dark
    /// code — flipping the `on_stack.contains` check would still pass
    /// every other Tarjan test in this module.
    ///
    /// Fixture:
    /// ```text
    ///   /a <-> /b   (cycle)
    ///   /c  ->  /a  (cross-SCC edge into the cycle)
    /// ```
    /// Expected: exactly one SCC of size 2 containing `{/a, /b}`. `/c`
    /// is acyclic — its size-1 SCC is filtered out by the `len > 1`
    /// guard.
    #[test]
    fn tarjan_cross_scc_edge_not_doubled() {
        let g = graph_from_includes(&[("/a", "/b"), ("/b", "/a"), ("/c", "/a")]);
        let cycles = g.detect_cycles();
        assert_eq!(
            cycles.len(),
            1,
            "cross-SCC edge from /c must not create a second cycle: {cycles:?}",
        );
        assert_eq!(cycles[0].len(), 2);
        assert_eq!(
            scc_contents(&cycles[0]),
            vec!["/a".to_string(), "/b".to_string()],
        );
    }

    // --- class_hierarchy: lookup and kind filter ---

    #[test]
    fn class_hierarchy_unknown_returns_none() {
        let g = Graph::new();
        assert!(g.class_hierarchy("Foo", 1).is_none());
    }

    #[test]
    fn class_hierarchy_struct_kind_works() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("MyStruct", SymbolKind::Struct, "/a.cpp")],
            vec![],
        ));
        let result = g.class_hierarchy("MyStruct", 1);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "MyStruct");
    }

    #[test]
    fn class_hierarchy_widened_filter_trait() {
        // The Go reference checks only Class/Struct. The Rust port widens
        // to Class/Struct/Interface/Trait so Rust traits resolve as roots.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.rs",
            Language::Rust,
            vec![sym("MyTrait", SymbolKind::Trait, "/a.rs")],
            vec![],
        ));
        let result = g.class_hierarchy("MyTrait", 1);
        assert!(result.is_some(), "widened filter must accept Trait kind",);
        assert_eq!(result.unwrap().name, "MyTrait");
    }

    #[test]
    fn class_hierarchy_widened_filter_interface() {
        // Same as above but for Interface kind (Go interfaces / future
        // language support).
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.go",
            Language::Go,
            vec![sym("MyInterface", SymbolKind::Interface, "/a.go")],
            vec![],
        ));
        let result = g.class_hierarchy("MyInterface", 1);
        assert!(
            result.is_some(),
            "widened filter must accept Interface kind",
        );
        assert_eq!(result.unwrap().name, "MyInterface");
    }

    #[test]
    fn class_hierarchy_function_kind_rejected() {
        // Sanity check: non-class-like kinds still return None.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("foo", SymbolKind::Function, "/a.cpp")],
            vec![],
        ));
        assert!(g.class_hierarchy("foo", 1).is_none());
    }

    // --- class_hierarchy: depth semantics ---

    #[test]
    fn class_hierarchy_depth_one_returns_direct_only() {
        // Chain Base <- Mid <- Leaf (Inherits edges flow Leaf -> Mid -> Base).
        // Querying Mid at depth=1 returns:
        //   bases: [Base]   (Base has empty bases — depth budget exhausted)
        //   derived: [Leaf] (Leaf has empty derived — same)
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("Base", SymbolKind::Class, "/a.cpp"),
                sym("Mid", SymbolKind::Class, "/a.cpp"),
                sym("Leaf", SymbolKind::Class, "/a.cpp"),
            ],
            vec![
                inherit_edge("Mid", "Base", "/a.cpp"),
                inherit_edge("Leaf", "Mid", "/a.cpp"),
            ],
        ));

        let result = g.class_hierarchy("Mid", 1).expect("Mid found");
        assert_eq!(result.name, "Mid");
        assert_eq!(result.bases.len(), 1);
        assert_eq!(result.bases[0].name, "Base");
        assert!(
            result.bases[0].bases.is_empty(),
            "depth budget exhausted: Base must not expand further",
        );
        assert_eq!(result.derived.len(), 1);
        assert_eq!(result.derived[0].name, "Leaf");
        assert!(
            result.derived[0].derived.is_empty(),
            "depth budget exhausted: Leaf must not expand further",
        );
    }

    #[test]
    fn class_hierarchy_depth_zero_normalized_to_one() {
        // depth=0 must behave identically to depth=1 (matches the Go
        // `if depth <= 0 { depth = 1 }` normalization).
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("Base", SymbolKind::Class, "/a.cpp"),
                sym("Derived", SymbolKind::Class, "/a.cpp"),
            ],
            vec![inherit_edge("Derived", "Base", "/a.cpp")],
        ));
        let zero = g.class_hierarchy("Derived", 0).expect("Derived found");
        let one = g.class_hierarchy("Derived", 1).expect("Derived found");
        assert_eq!(zero, one);
    }

    // --- class_hierarchy: 4-level diamond regression ---

    /// THE regression test for the diamond-inheritance bug fixed in the
    /// LLMOptimization phase. With a global-visited DFS, the second
    /// arm's visit to `Base` would short-circuit to an empty leaf,
    /// silently truncating the hierarchy. The 3-class diamond at
    /// depth=2 is too shallow to expose the bug because the shared node
    /// bottoms out as a leaf either way; a 4-level chain at depth=3 is
    /// the minimal fixture that makes the truncation visible.
    ///
    /// Fixture (Inherits flows child -> parent):
    /// ```text
    ///   Root
    ///    ^
    ///   Base
    ///    ^
    ///   ├── MixinA
    ///   └── MixinB
    ///        ^
    ///       Derived
    ///        ^
    ///       Leaf
    /// ```
    /// Inherits edges: Base->Root, MixinA->Base, MixinB->Base,
    /// Derived->MixinA, Derived->MixinB, Leaf->Derived.
    ///
    /// Query: `class_hierarchy("Derived", 3)`.
    /// Expected:
    /// - bases = [MixinA, MixinB]
    /// - both MixinA and MixinB have bases = [Base]
    /// - **both copies of Base have non-empty bases containing Root**
    ///
    /// If the per-DFS-path tracking is reverted to a global visited
    /// set, the second arm's Base would have empty bases — the
    /// assertions below would catch it.
    #[test]
    fn class_hierarchy_diamond_4_level_fixture() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("Root", SymbolKind::Class, "/a.cpp"),
                sym("Base", SymbolKind::Class, "/a.cpp"),
                sym("MixinA", SymbolKind::Class, "/a.cpp"),
                sym("MixinB", SymbolKind::Class, "/a.cpp"),
                sym("Derived", SymbolKind::Class, "/a.cpp"),
                sym("Leaf", SymbolKind::Class, "/a.cpp"),
            ],
            vec![
                inherit_edge("Base", "Root", "/a.cpp"),
                inherit_edge("MixinA", "Base", "/a.cpp"),
                inherit_edge("MixinB", "Base", "/a.cpp"),
                inherit_edge("Derived", "MixinA", "/a.cpp"),
                inherit_edge("Derived", "MixinB", "/a.cpp"),
                inherit_edge("Leaf", "Derived", "/a.cpp"),
            ],
        ));

        let result = g.class_hierarchy("Derived", 3).expect("Derived found");
        assert_eq!(result.name, "Derived");
        assert_eq!(result.bases.len(), 2, "Derived inherits from both mixins");

        // Sort bases by name so the test isn't order-sensitive.
        let mut bases = result.bases.clone();
        bases.sort_by(|a, b| a.name.cmp(&b.name));
        let mixin_a = &bases[0];
        let mixin_b = &bases[1];
        assert_eq!(mixin_a.name, "MixinA");
        assert_eq!(mixin_b.name, "MixinB");

        // Both mixins must have bases = [Base].
        assert_eq!(mixin_a.bases.len(), 1, "MixinA -> Base");
        assert_eq!(mixin_a.bases[0].name, "Base");
        assert_eq!(mixin_b.bases.len(), 1, "MixinB -> Base");
        assert_eq!(mixin_b.bases[0].name, "Base");

        // **The regression assertion**: both Base copies must be fully
        // expanded. With a global-visited bug the second copy would have
        // empty bases.
        assert!(
            !mixin_a.bases[0].bases.is_empty(),
            "diamond bug regression: Base under MixinA must expand to Root",
        );
        assert!(
            !mixin_b.bases[0].bases.is_empty(),
            "diamond bug regression: Base under MixinB must expand to Root",
        );
        assert_eq!(mixin_a.bases[0].bases[0].name, "Root");
        assert_eq!(mixin_b.bases[0].bases[0].name, "Root");

        // Sanity: the derived side reports Leaf.
        assert_eq!(result.derived.len(), 1);
        assert_eq!(result.derived[0].name, "Leaf");
    }
}
