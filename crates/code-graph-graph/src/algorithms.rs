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
//! The class hierarchy walk uses two visited sets in tandem: a
//! per-DFS-path `on_path` set distinguishes cycles (bare-leaf halt) from
//! diamonds, and a global `visited_unique` set drives both the
//! `max_nodes` budget and the diamond-dedupe ref-stub branch. The
//! check order is `on_path` first, `visited_unique` second — a name
//! reached on the current DFS path collapses to a cycle leaf even if
//! it has also been fully expanded elsewhere. See
//! [`Graph::class_hierarchy`] and the
//! `class_hierarchy_diamond_4_level_fixture` test below.
//!
//! Locking is not handled in this module. Task 2.6 wraps [`Graph`] in
//! `parking_lot::RwLock`; until then these methods take `&self` and rely
//! on the caller for synchronization.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use code_graph_core::{EdgeKind, SymbolKind};

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
///
/// `ref` distinguishes diamond-shared stubs from canonical nodes;
/// cycle-guard halts remain JSON-identical to natural leaves.
/// - `ref: None` (field omitted from JSON) on a *full* node = canonical
///   first-visit occurrence; its `bases`/`derived` (when present) are
///   the real subtree. A leaf with `ref: None` is the natural end of an
///   inheritance chain — nothing more to walk.
/// - `ref: Some(true)` (emitted as `"ref": true`) = stub indicating
///   "this name was already visited and fully expanded elsewhere in the
///   tree; consult the canonical occurrence". Stubs collapse diamond-
///   inheritance subtree duplications so the same ancestor isn't
///   re-serialized inline on every arm that reaches it.
/// - A bare leaf with empty `bases`/`derived` and `ref: None` reached
///   via the cycle base case (the queried name was already on the
///   current DFS path) is the cycle-guard halt. It is JSON-
///   indistinguishable from a natural-end leaf; the distinction lives
///   only in the walk's internal state. Clients walking the tree treat
///   both cases identically.
///
/// The raw-identifier `r#ref` is required because `ref` is a Rust
/// keyword. Serde strips the `r#` prefix automatically when serializing,
/// so the JSON field name is the unprefixed `"ref"` without needing
/// `#[serde(rename = "ref")]`.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HierarchyNode {
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bases: Vec<HierarchyNode>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub derived: Vec<HierarchyNode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<bool>,
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

    /// Inheritance tree rooted at `name`, with a global unique-name budget.
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
    /// `max_nodes` caps the total number of *unique class names* that
    /// can appear anywhere in the returned tree. A name is counted once
    /// in the budget no matter how many paths reach it (so a diamond
    /// where the shared ancestor is reachable via N arms costs 1 budget
    /// slot, not N). When the budget is exhausted, recursion stops
    /// adding new children to subsequent nodes — but already-recursed
    /// children remain in the tree, so the partial tree is well-formed.
    /// Callers that want the legacy "unbounded" behavior pass
    /// `max_nodes = u32::MAX`.
    ///
    /// On success returns `(root, total_nodes_seen, truncated)`:
    /// - `total_nodes_seen` is the number of unique names actually
    ///   walked (≤ `max_nodes`).
    /// - `truncated` is `true` when the budget cut at least one child
    ///   off the tree.
    ///
    /// **Diamond inheritance**: the DFS uses a *per-path* `on_path` set
    /// for cycle detection and a *global* `visited_unique` set for
    /// diamond deduplication. When a shared ancestor is reached via two
    /// different paths (e.g. `Derived → MixinA → Base` and
    /// `Derived → MixinB → Base`), the first arm fully expands `Base`
    /// and inserts the name into `visited_unique`; the second arm sees
    /// the name in `visited_unique` (but not in `on_path`) and emits a
    /// `{name, ref: true}` stub instead of re-expanding. Clients walking
    /// the tree maintain a `name -> node` map keyed on the first
    /// non-ref occurrence and treat `ref: true` as a pointer back to
    /// that canonical node. Cycles still emit bare leaves (`on_path`
    /// hit wins over `visited_unique` hit); the cycle leaf is JSON-
    /// indistinguishable from a natural-end leaf.
    ///
    /// The `max_nodes` budget consults `visited_unique` exclusively —
    /// each unique name costs one slot regardless of how many arms
    /// reach it. Ref-stubs do not charge additional slots (the slot
    /// was charged on first visit). See the
    /// `class_hierarchy_diamond_4_level_fixture` and
    /// `class_hierarchy_diamond_counts_unique_names` tests.
    pub fn class_hierarchy(
        &self,
        name: &str,
        depth: u32,
        max_nodes: u32,
    ) -> Option<(HierarchyNode, u32, bool)> {
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
        let mut visited_unique: HashSet<String> = HashSet::new();
        let mut truncated = false;
        // The root is treated identically to any other first-visit node:
        // `build_hierarchy` inserts it into both `on_path` and
        // `visited_unique` at the top of the call. The root therefore
        // costs one budget slot exactly like every other class name; if
        // `max_nodes == 0`, the recursive helper still inserts the root
        // but refuses to descend into any children (the per-child budget
        // gate sees `visited_unique.len() == 1 >= 0` and trips
        // `truncated`).
        let root = self.build_hierarchy(
            name,
            depth,
            &mut on_path,
            &mut visited_unique,
            max_nodes,
            &mut truncated,
        );
        let total = visited_unique.len() as u32;
        Some((root, total, truncated))
    }

    /// Recursive helper for [`Graph::class_hierarchy`].
    ///
    /// Recursion is acceptable here (unlike the iterative-Tarjan
    /// requirement) because class hierarchies are realistically a few
    /// dozen levels deep at worst — the stack-safety concern only
    /// applies to file-include cycles which can chain across thousands
    /// of headers. The plan only requires Tarjan to be iterative.
    ///
    /// The function opens with a three-way branch at the visit point.
    /// The order is load-bearing: `on_path` is checked BEFORE
    /// `visited_unique` so a self-cycle that is also on a different DFS
    /// path collapses to a cycle leaf (not a ref-stub). The three cases:
    ///
    /// 1. **Cycle** — `on_path.contains(name)` is true. Return a bare
    ///    leaf `{name}` with `r#ref: None`. This is the existing cycle
    ///    guard, JSON-indistinguishable from a natural-end leaf, and
    ///    matches the Go reference's `if onPath[name] { return &node }`.
    /// 2. **Diamond** — `visited_unique.contains(name)` is true and the
    ///    name is NOT on the current path. Return a ref-stub
    ///    `{name, ref: true}` and do NOT recurse. The canonical
    ///    occurrence (with full `bases`/`derived`) lives at the first
    ///    place this name was reached in pre-order; clients reconstruct
    ///    the full tree by keying a `name -> node` map on first
    ///    non-ref occurrences and treating ref-stubs as pointers.
    /// 3. **First visit** — neither set contains the name. Insert into
    ///    BOTH `on_path` and `visited_unique`, recurse to build the
    ///    subtree, and remove from `on_path` on the way back up.
    ///    `visited_unique` is never removed — it carries diamond-
    ///    dedupe state across sibling branches.
    ///
    /// Two visited sets are threaded through:
    /// - `on_path` (per-DFS-path) is mutated in lockstep with the
    ///   recursion: the name is inserted before recursing into children
    ///   and removed after both the bases and derived loops complete.
    /// - `visited_unique` (global) tracks every unique name walked
    ///   anywhere in the tree. It serves two purposes: gating the
    ///   `max_nodes` budget (one slot per unique name regardless of
    ///   visit count) and triggering the diamond ref-stub branch on
    ///   re-visits.
    ///
    /// The caller's per-child budget gate consults `visited_unique`
    /// before recursing: only first-visit children consume a slot;
    /// diamond re-visits are free. Ref-stub emission therefore preserves
    /// the existing `total_nodes_seen` contract — unique class names
    /// walked, diamond ancestor = 1 slot.
    fn build_hierarchy(
        &self,
        name: &str,
        depth: u32,
        on_path: &mut HashSet<String>,
        visited_unique: &mut HashSet<String>,
        max_nodes: u32,
        truncated: &mut bool,
    ) -> HierarchyNode {
        // (1) Cycle guard. Checked FIRST so a self-cycle on a different
        // DFS path collapses to a cycle leaf, not a ref-stub. Bare leaf
        // with `r#ref: None` matches Go's
        // `if onPath[name] { return &HierarchyNode{Name: name} }`.
        if on_path.contains(name) {
            return HierarchyNode {
                name: name.to_string(),
                bases: Vec::new(),
                derived: Vec::new(),
                r#ref: None,
            };
        }

        // (2) Diamond dedupe. The name has already been fully expanded
        // somewhere else in the tree (its canonical occurrence). Emit a
        // ref-stub so clients can rejoin the canonical subtree without
        // re-serializing it inline. No recursion — `visited_unique`
        // already counted this name's slot at first visit.
        if visited_unique.contains(name) {
            return HierarchyNode {
                name: name.to_string(),
                bases: Vec::new(),
                derived: Vec::new(),
                r#ref: Some(true),
            };
        }

        // (3) First visit. Charge the unique-name slot and enter the
        // DFS path. Both inserts happen here, in lockstep, so future
        // re-visits along the same path see `on_path` first (cycle
        // leaf) and re-visits across sibling paths see `visited_unique`
        // (ref-stub).
        visited_unique.insert(name.to_string());

        let mut node = HierarchyNode {
            name: name.to_string(),
            bases: Vec::new(),
            derived: Vec::new(),
            r#ref: None,
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
        // unconditional cleanup without `unsafe`. Note: only `on_path`
        // is rewound; `visited_unique` is intentionally global, so its
        // first-visit insert above persists across the unwind for the
        // diamond-dedupe invariant.
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
        //
        // The budget check happens *before* we recurse into a child:
        // - if the child's name is already in `visited_unique`, the
        //   recursion will emit a ref-stub (case 2 above) and is free
        //   — no budget cost for the diamond's shared ancestor.
        // - otherwise, refuse to descend when the budget is at the cap;
        //   set `truncated` and skip this child entirely so the budget
        //   check is monotone (once exceeded, it never tries to grow
        //   again on this branch). The actual `visited_unique` insert
        //   for the child happens inside the recursive call's first-
        //   visit branch.
        if let Some(entries) = self.adj.get(name) {
            let bases: Vec<String> = entries
                .iter()
                .filter(|e| e.kind == EdgeKind::Inherits)
                .map(|e| e.target.clone())
                .collect();
            for target in bases {
                if !visited_unique.contains(&target) && (visited_unique.len() as u32) >= max_nodes {
                    *truncated = true;
                    continue;
                }
                node.bases.push(self.build_hierarchy(
                    &target,
                    depth - 1,
                    guard.set,
                    visited_unique,
                    max_nodes,
                    truncated,
                ));
            }
        }

        if let Some(entries) = self.radj.get(name) {
            let derived: Vec<String> = entries
                .iter()
                .filter(|e| e.kind == EdgeKind::Inherits)
                .map(|e| e.target.clone())
                .collect();
            for target in derived {
                if !visited_unique.contains(&target) && (visited_unique.len() as u32) >= max_nodes {
                    *truncated = true;
                    continue;
                }
                node.derived.push(self.build_hierarchy(
                    &target,
                    depth - 1,
                    guard.set,
                    visited_unique,
                    max_nodes,
                    truncated,
                ));
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
    use code_graph_core::{Edge, Language};

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

    // --- HierarchyNode serialization ---

    /// Byte-identical-JSON regression: a leaf `HierarchyNode` with
    /// `r#ref: None` and empty `bases`/`derived` must serialize to
    /// EXACTLY `{"name":"X"}` — the same wire shape it had before the
    /// `ref` field was added in Task 2.1. This pins down two contracts
    /// simultaneously:
    /// 1. `#[serde(default, skip_serializing_if = "Option::is_none")]`
    ///    drops the field from the JSON output when `ref` is `None`, so
    ///    existing JSON consumers / stored snapshots see no shape change.
    /// 2. Serde strips the `r#` raw-identifier prefix automatically when
    ///    emitting JSON field names — no `#[serde(rename = "ref")]` is
    ///    needed. If a future serde change broke this assumption, the
    ///    test would surface as `{"name":"X","r#ref":...}` or similar.
    #[test]
    fn hierarchy_node_ref_none_serializes_without_ref_field() {
        let node = HierarchyNode {
            name: "X".to_string(),
            bases: Vec::new(),
            derived: Vec::new(),
            r#ref: None,
        };
        let json = serde_json::to_string(&node).expect("serialize HierarchyNode");
        assert_eq!(
            json, r#"{"name":"X"}"#,
            "leaf node with ref: None must serialize byte-identically to pre-Task-2.1 shape",
        );

        // Round-trip: JSON missing every optional field must deserialize
        // back to the same node. Pins the `#[serde(default)]` on `r#ref`
        // — without it, missing `"ref"` would refuse to deserialize and
        // any historical hierarchy JSON would break on load.
        let round_trip: HierarchyNode =
            serde_json::from_str(r#"{"name":"X"}"#).expect("deserialize HierarchyNode");
        assert_eq!(round_trip, node);
    }

    // --- class_hierarchy: lookup and kind filter ---

    #[test]
    fn class_hierarchy_unknown_returns_none() {
        let g = Graph::new();
        assert!(g.class_hierarchy("Foo", 1, u32::MAX).is_none());
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
        let result = g.class_hierarchy("MyStruct", 1, u32::MAX);
        assert!(result.is_some());
        let (root, total, truncated) = result.unwrap();
        assert_eq!(root.name, "MyStruct");
        assert_eq!(total, 1);
        assert!(!truncated);
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
        let result = g.class_hierarchy("MyTrait", 1, u32::MAX);
        assert!(result.is_some(), "widened filter must accept Trait kind",);
        assert_eq!(result.unwrap().0.name, "MyTrait");
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
        let result = g.class_hierarchy("MyInterface", 1, u32::MAX);
        assert!(
            result.is_some(),
            "widened filter must accept Interface kind",
        );
        assert_eq!(result.unwrap().0.name, "MyInterface");
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
        assert!(g.class_hierarchy("foo", 1, u32::MAX).is_none());
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

        let (result, _, _) = g.class_hierarchy("Mid", 1, u32::MAX).expect("Mid found");
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
        let (zero, _, _) = g
            .class_hierarchy("Derived", 0, u32::MAX)
            .expect("Derived found");
        let (one, _, _) = g
            .class_hierarchy("Derived", 1, u32::MAX)
            .expect("Derived found");
        assert_eq!(zero, one);
    }

    // --- class_hierarchy: 4-level diamond regression ---

    /// Regression pin for the diamond-shape walk. The historical bug
    /// (fixed in the LLMOptimization phase) was a global-visited DFS
    /// that silently truncated the second arm's `Base` to an empty
    /// leaf. The ref-stub walk introduced for diamond dedupe (this
    /// phase) replaces *re-expansion* with explicit `ref: true` stubs:
    /// the canonical expansion still happens once, but its name is
    /// reachable everywhere else via ref-stubs rather than missing.
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
    /// Pre-order DFS for `class_hierarchy("Derived", 3)`:
    ///   - Visit Derived.
    ///   - bases[0] = MixinA → bases[0] = Base → bases[0] = Root.
    ///   - Walking Base's derived (down-DFS) reaches MixinA (cycle
    ///     leaf — on_path) and MixinB (FIRST visit — full node, but at
    ///     depth=0 so empty bases/derived).
    ///   - Back at Derived, bases[1] = MixinB. MixinB is already in
    ///     `visited_unique` from the down-DFS walk above, so it emits
    ///     a `{name: "MixinB", ref: true}` ref-stub here.
    ///
    /// The discriminator assertions: MixinB at `result.bases[1]` is a
    /// ref-stub (empty bases, `ref: Some(true)`) AND the canonical
    /// MixinB inside `Base.derived` does NOT recurse to Base (which
    /// would be a cycle), so the diamond has exactly one fully-expanded
    /// path Derived → MixinA → Base → Root rather than two duplicate
    /// inline expansions.
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

        let (result, _, _) = g
            .class_hierarchy("Derived", 3, u32::MAX)
            .expect("Derived found");
        assert_eq!(result.name, "Derived");
        assert_eq!(result.bases.len(), 2, "Derived inherits from both mixins");

        // Sort bases by name so the test isn't order-sensitive.
        let mut bases = result.bases.clone();
        bases.sort_by(|a, b| a.name.cmp(&b.name));
        let mixin_a = &bases[0];
        let mixin_b = &bases[1];
        assert_eq!(mixin_a.name, "MixinA");
        assert_eq!(mixin_b.name, "MixinB");

        // MixinA is the canonical (first-visit) occurrence and has a
        // fully expanded Base subtree: Base → Root.
        assert_eq!(mixin_a.r#ref, None, "MixinA must be the canonical node");
        assert_eq!(mixin_a.bases.len(), 1, "MixinA -> Base");
        let base = &mixin_a.bases[0];
        assert_eq!(base.name, "Base");
        assert_eq!(base.r#ref, None, "Base under MixinA is canonical");
        assert_eq!(
            base.bases.len(),
            1,
            "Base must expand to Root via its canonical occurrence",
        );
        assert_eq!(base.bases[0].name, "Root");

        // MixinB at Derived.bases[1] is a ref-stub: MixinB was already
        // walked as one of Base.derived during the canonical Base
        // expansion above, so the second reach emits a stub rather than
        // re-expanding the Mixin subtree inline.
        assert_eq!(
            mixin_b.r#ref,
            Some(true),
            "MixinB on the second arm must be a ref-stub, not a full node",
        );
        assert!(
            mixin_b.bases.is_empty(),
            "ref-stub carries empty bases by definition; got {:?}",
            mixin_b.bases,
        );
        assert!(
            mixin_b.derived.is_empty(),
            "ref-stub carries empty derived by definition",
        );

        // Sanity: the derived side reports Leaf as a full node.
        assert_eq!(result.derived.len(), 1);
        assert_eq!(result.derived[0].name, "Leaf");
        assert_eq!(result.derived[0].r#ref, None);
    }

    // --- class_hierarchy: max_nodes budget ---

    /// Build a 12-class linear inheritance chain on the *derived* side:
    /// `C00 <- C01 <- ... <- C11`. Querying `class_hierarchy("C00", depth,
    /// max_nodes=10)` walks Root -> ... 11 unique names total in the
    /// derived direction, so a budget of 10 must truncate after the 10th
    /// unique node.
    ///
    /// Inherits edge direction: child → parent. So `C01 -> C00`,
    /// `C02 -> C01`, etc. — `radj["Cnn"]` then yields the derived
    /// children, walked by the down-DFS arm.
    fn linear_chain_graph(n: usize) -> Graph {
        let mut g = Graph::new();
        let mut symbols: Vec<code_graph_core::Symbol> = Vec::with_capacity(n);
        for i in 0..n {
            symbols.push(sym(&format!("C{i:02}"), SymbolKind::Class, "/chain.cpp"));
        }
        let mut edges: Vec<Edge> = Vec::with_capacity(n - 1);
        for i in 1..n {
            // child Ci inherits from parent Ci-1.
            edges.push(inherit_edge(
                &format!("C{i:02}"),
                &format!("C{:02}", i - 1),
                "/chain.cpp",
            ));
        }
        g.merge_file_graph(make_fg("/chain.cpp", Language::Cpp, symbols, edges));
        g
    }

    /// Walk a `HierarchyNode` tree and collect every distinct name
    /// reachable through `bases` and `derived`. Used by the budget tests
    /// to count the unique names actually present in the returned tree.
    fn collect_unique_names(node: &HierarchyNode, out: &mut HashSet<String>) {
        out.insert(node.name.clone());
        for b in &node.bases {
            collect_unique_names(b, out);
        }
        for d in &node.derived {
            collect_unique_names(d, out);
        }
    }

    /// Truncation regression: hierarchy with at least 11 unique classes,
    /// queried with `max_nodes = 10`. Must report `truncated = true`,
    /// `total_nodes_seen = 10`, and the tree must contain exactly 10
    /// unique names (the budget cap).
    #[test]
    fn class_hierarchy_max_nodes_truncates() {
        // 12-node chain so the visit count exceeds the 10-node budget.
        let g = linear_chain_graph(12);
        // Use a generous depth so the DFS would reach every node if
        // unbounded — the budget is the truncation mechanism, not depth.
        let (root, total, truncated) = g.class_hierarchy("C00", 50, 10).expect("C00 found");
        assert!(
            truncated,
            "12-node chain with max_nodes=10 must truncate, got truncated=false"
        );
        assert_eq!(total, 10, "total_nodes_seen must equal the budget cap");
        let mut names: HashSet<String> = HashSet::new();
        collect_unique_names(&root, &mut names);
        assert_eq!(
            names.len(),
            10,
            "tree must contain exactly 10 unique names; got {}: {:?}",
            names.len(),
            names
        );
    }

    /// THE diamond budget regression. Fixture has 4 unique class names
    /// (`Root`, `Mid1`, `Mid2`, `Leaf`) but the shared root `Root` is
    /// reachable from `Leaf` via *two* arms (`Leaf -> Mid1 -> Root` AND
    /// `Leaf -> Mid2 -> Root`), so the total *visit* count is 5, while
    /// the unique-name count is 4. With `max_nodes = 4` (= unique count,
    /// strictly < visit count), the budget MUST NOT truncate — every
    /// unique name fits, the diamond's shared ancestor costs one slot
    /// even though it appears twice in the tree.
    ///
    /// **A naïve visit-counting implementation would truncate at 4 visits
    /// and miss the second `Root` expansion under `Mid2`.** The combination
    /// `truncated=false` + all four names present is the discriminator
    /// between correct unique-name counting and incorrect visit counting.
    #[test]
    fn class_hierarchy_diamond_counts_unique_names() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/diamond.cpp",
            Language::Cpp,
            vec![
                sym("Root", SymbolKind::Class, "/diamond.cpp"),
                sym("Mid1", SymbolKind::Class, "/diamond.cpp"),
                sym("Mid2", SymbolKind::Class, "/diamond.cpp"),
                sym("Leaf", SymbolKind::Class, "/diamond.cpp"),
            ],
            vec![
                // child -> parent edges. Diamond:
                //   Root has two derived children Mid1 and Mid2; both Mids
                //   have Leaf as their derived child. So Leaf is reachable
                //   via both Mid1 and Mid2 from Root in the down-DFS, and
                //   `class_hierarchy("Root", depth, max_nodes)` walks the
                //   shared `Leaf` twice via two separate paths.
                inherit_edge("Mid1", "Root", "/diamond.cpp"),
                inherit_edge("Mid2", "Root", "/diamond.cpp"),
                inherit_edge("Leaf", "Mid1", "/diamond.cpp"),
                inherit_edge("Leaf", "Mid2", "/diamond.cpp"),
            ],
        ));

        // Generous depth so the down-DFS would otherwise reach Leaf twice.
        let (root, total, truncated) = g.class_hierarchy("Root", 5, 4).expect("Root found");

        // The load-bearing assertions:
        assert!(
            !truncated,
            "max_nodes=4 (= unique name count) must NOT truncate even \
             though visit count is 5. truncated=true here means the \
             budget was charged per-visit instead of per-unique-name."
        );
        assert_eq!(
            total, 4,
            "total_nodes_seen must count unique names (4), not visits (5)"
        );

        let mut names: HashSet<String> = HashSet::new();
        collect_unique_names(&root, &mut names);
        assert_eq!(
            names.len(),
            4,
            "all four unique names must appear in the tree; got: {names:?}"
        );
        for want in ["Root", "Mid1", "Mid2", "Leaf"] {
            assert!(
                names.contains(want),
                "tree missing {want:?}; got: {names:?}. A visit-counting \
                 budget would have run out before reaching the second arm \
                 of the diamond."
            );
        }
    }

    /// Backward-compat regression: with `max_nodes = u32::MAX` the
    /// algorithm must produce the same tree shape as the
    /// `class_hierarchy_diamond_4_level_fixture` test (ref-stub walk).
    /// Asserts the budget plumbing doesn't perturb unbounded queries —
    /// `u32::MAX` should never truncate and never change the dedupe
    /// pattern.
    #[test]
    fn class_hierarchy_max_nodes_unbounded_matches_legacy() {
        // Same fixture as `class_hierarchy_diamond_4_level_fixture`.
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

        let (root, _total, truncated) = g
            .class_hierarchy("Derived", 3, u32::MAX)
            .expect("Derived found");
        assert!(!truncated, "u32::MAX budget never truncates");

        // Same shape as the ref-stub diamond test: MixinA is the
        // canonical arm with Base → Root fully expanded; MixinB at
        // Derived.bases[1] is the ref-stub. A regression in the budget
        // plumbing that perturbed the walk order or fired the budget
        // gate inappropriately would change either the canonical
        // expansion or the ref-stub placement.
        let mut bases = root.bases.clone();
        bases.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(bases.len(), 2);
        assert_eq!(bases[0].name, "MixinA");
        assert_eq!(bases[1].name, "MixinB");
        assert_eq!(bases[0].r#ref, None, "MixinA is canonical under u32::MAX");
        assert_eq!(bases[0].bases.len(), 1);
        assert_eq!(bases[0].bases[0].name, "Base");
        assert_eq!(bases[0].bases[0].r#ref, None);
        assert!(
            !bases[0].bases[0].bases.is_empty(),
            "Base under MixinA must expand to Root (canonical arm)"
        );
        assert_eq!(bases[0].bases[0].bases[0].name, "Root");

        assert_eq!(
            bases[1].r#ref,
            Some(true),
            "MixinB on the second arm must remain a ref-stub even under \
             unbounded budget — `max_nodes = u32::MAX` must not change the \
             walk's dedupe behavior."
        );
        assert!(bases[1].bases.is_empty());

        assert_eq!(root.derived.len(), 1);
        assert_eq!(root.derived[0].name, "Leaf");
    }
}
