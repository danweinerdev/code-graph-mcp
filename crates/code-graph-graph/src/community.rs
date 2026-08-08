//! File-graph aggregation and label-propagation community detection.
//!
//! Answers "what are the de-facto modules here?" (FR-24, FR-25, FR-44,
//! FR-45, FR-46) by collapsing the symbol-level call/include graph into a
//! weighted, undirected file graph ([`Graph::aggregate_file_edges`]) and
//! partitioning it with parameter-free label propagation
//! ([`Graph::file_communities`]).
//!
//! # Determinism (`Designs/GraphQueries` Decision 4/5, NFR-03)
//!
//! `Graph.nodes` / `Graph.adj` / `Graph.radj` are `std::collections::HashMap`
//! with a randomly seeded hasher — iterating either one directly
//! reintroduces per-run nondeterminism. [`Graph::aggregate_file_edges`]
//! drives iteration entirely from `Graph.files` (a `PathTrie`, whose DFS is
//! pre-sorted by path segment) and only ever does *keyed* lookups
//! (`.get(id)`) into `nodes`/`adj`/`includes`. File indices `0..F` are
//! assigned in that trie-iteration order, giving a stable node numbering
//! for free. Label propagation then sweeps nodes in ascending index order
//! with a fixed tie-break (smallest label id; keep the current label when
//! it is among the maxima), so the whole pipeline is a pure function of
//! graph contents, never of hash-map iteration order.
//!
//! Local `HashMap`s built and consumed entirely within a single call (the
//! weight table, per-node label tallies, the label→members grouping) do
//! not reintroduce nondeterminism even though they are iterated: their
//! *keys* are algorithmically derived (deterministic), and every place
//! their iteration order could leak into the result is followed by an
//! explicit deterministic sort or a full-scan reduction (e.g. "smallest
//! label achieving the max weight") whose outcome does not depend on scan
//! order.

use std::collections::HashMap;
use std::path::PathBuf;

use code_graph_core::EdgeKind;

use crate::Graph;

/// How label propagation ended. Reported so a caller never has to guess
/// whether a partition is a genuine fixed point or a ceiling cutoff
/// (`Designs/GraphQueries` Decision 5, AC-54).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Termination {
    /// A full sweep produced no label change.
    Converged { iterations: u32 },
    /// `max_iterations` was reached before a no-change sweep.
    IterationCeiling { iterations: u32 },
}

/// A degenerate partition, flagged rather than returned as an ordinary
/// result (`Designs/GraphQueries` Decision 8, AC-55).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Degeneracy {
    /// The largest community holds `>= 900` permille of all nodes.
    /// `share_permille` reports how close to total the collapse was.
    /// Only evaluated when `node_count >= 10`.
    Giant { share_permille: u32 },
    /// Community count equals node count (every file is its own
    /// community). Only evaluated when `node_count >= 10`.
    Atomized,
}

/// One community in a [`CommunityResult`]: its member files (ascending
/// path order) and a derived human-readable label.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileCommunity {
    pub members: Vec<PathBuf>,
    pub label: String,
}

/// Result of [`Graph::file_communities`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommunityResult {
    /// Descending member count, then ascending first-member path.
    pub communities: Vec<FileCommunity>,
    /// Number of files (nodes) in the aggregated file graph.
    pub node_count: u32,
    /// Number of distinct undirected file-pairs with nonzero weight in
    /// the aggregated file graph (not a raw symbol-edge count).
    pub edge_count: u32,
    pub termination: Termination,
    pub degeneracy: Option<Degeneracy>,
}

impl Graph {
    /// Build a weighted, undirected file graph in a single pass over
    /// `self.files` (`Designs/GraphQueries` Decision 7).
    ///
    /// Returns `(index_to_path, weights)`: `index_to_path[i]` is the file
    /// assigned index `i` (in `files`-trie DFS order — deterministic and
    /// lexicographically ordered); `weights` maps an undirected file pair
    /// `(min(i,j), max(i,j))` to the number of `Calls` + `Includes` edges
    /// folded between them.
    ///
    /// Two filters are part of the contract, not implementation detail:
    /// - **Only [`EdgeKind::Calls`] is read from `adj`.** `EdgeKind` has
    ///   four variants and `Inherits`/`Overrides` are routed into `adj`
    ///   too; without this filter they would silently become community
    ///   weight (`Designs/GraphQueries` Decision 7).
    /// - **Self-pairs (`i == j`) are skipped before the weight table.**
    ///   Intra-file calls are typically the majority of all `Calls`
    ///   edges; folded in, each becomes a self-loop that inflates every
    ///   node's vote for its own current label on every sweep, biasing
    ///   propagation toward stasis (the Trap this task exists to guard
    ///   against — see the self-loop-bias test below).
    ///
    /// This is `pub(crate)`: [`file_communities`](Graph::file_communities)
    /// is the public entry point; the raw weight table is an
    /// implementation detail today, kept as a separate function purely so
    /// the aggregation step can be unit-tested in isolation.
    pub(crate) fn aggregate_file_edges(&self) -> (Vec<PathBuf>, HashMap<(u32, u32), u32>) {
        // Pass 1: assign indices in files-trie DFS order. Building the
        // full index map up front lets pass 2 resolve ANY edge target
        // (including ones that appear later in trie order) via a keyed
        // lookup, never an iteration.
        let mut index_to_path: Vec<PathBuf> = Vec::with_capacity(self.files.len());
        let mut path_to_index: HashMap<PathBuf, u32> = HashMap::with_capacity(self.files.len());
        for (path, _entry) in self.files.iter() {
            let idx = index_to_path.len() as u32;
            path_to_index.insert(path.clone(), idx);
            index_to_path.push(path);
        }

        let mut weights: HashMap<(u32, u32), u32> = HashMap::new();

        for (path, entry) in self.files.iter() {
            let Some(&i) = path_to_index.get(&path) else {
                continue;
            };

            for id in &entry.symbol_ids {
                let Some(adj_entries) = self.adj.get(id) else {
                    continue;
                };
                for edge in adj_entries {
                    if edge.kind != EdgeKind::Calls {
                        continue;
                    }
                    let Some(target_node) = self.nodes.get(&edge.target) else {
                        continue;
                    };
                    let target_file = PathBuf::from(&target_node.symbol.file);
                    let Some(&j) = path_to_index.get(&target_file) else {
                        continue;
                    };
                    if i == j {
                        continue;
                    }
                    let key = (i.min(j), i.max(j));
                    *weights.entry(key).or_insert(0) += 1;
                }
            }

            if let Some(incs) = self.includes.get(&path) {
                for inc in incs {
                    let Some(&j) = path_to_index.get(&inc.path) else {
                        continue;
                    };
                    if i == j {
                        continue;
                    }
                    let key = (i.min(j), i.max(j));
                    *weights.entry(key).or_insert(0) += 1;
                }
            }
        }

        (index_to_path, weights)
    }

    /// Partition the file graph into communities via parameter-free label
    /// propagation (`Designs/GraphQueries` Decision 5).
    ///
    /// Nodes sweep in ascending file index. Each node adopts the label
    /// carrying the greatest summed neighbour weight; ties break toward
    /// the smallest label id; a node keeps its current label when that
    /// label is already among the maxima (damps two-node oscillation that
    /// would otherwise only stop at `max_iterations`). Terminates on a
    /// full sweep with no change, or at `max_iterations` — whichever
    /// fires first is reported via [`CommunityResult::termination`].
    ///
    /// Communities are ranked by descending member count, then ascending
    /// first-member path; members within a community are ascending path
    /// order. See [`community_label`] for the label derivation and
    /// [`detect_degeneracy`] for the `Giant`/`Atomized` thresholds.
    pub fn file_communities(&self, max_iterations: u32) -> CommunityResult {
        let (index_to_path, weights) = self.aggregate_file_edges();
        let node_count = index_to_path.len();

        let mut neighbours: Vec<Vec<(u32, u32)>> = vec![Vec::new(); node_count];
        for (&(i, j), &w) in &weights {
            neighbours[i as usize].push((j, w));
            neighbours[j as usize].push((i, w));
        }

        let mut labels: Vec<u32> = (0..node_count as u32).collect();
        let mut iterations: u32 = 0;
        let mut converged = false;

        while iterations < max_iterations {
            iterations += 1;
            let mut changed = false;

            for i in 0..node_count {
                let current = labels[i];

                let mut tally: HashMap<u32, u32> = HashMap::new();
                for &(j, w) in &neighbours[i] {
                    *tally.entry(labels[j as usize]).or_insert(0) += w;
                }
                if tally.is_empty() {
                    // Isolated node (no cross-file neighbours after the
                    // self-pair/edge-kind filters): keeps its own label.
                    continue;
                }

                // Full scan for "smallest label achieving the max
                // weight" — the outcome does not depend on `tally`'s
                // (HashMap-random) iteration order, only on its
                // key/value contents, so this stays deterministic.
                let max_weight = *tally.values().max().expect("tally is non-empty");
                let mut best = u32::MAX;
                for (&label, &w) in &tally {
                    if w == max_weight && label < best {
                        best = label;
                    }
                }
                // Damping: keep the current label if it is among the
                // maxima, even if a smaller label id also hits max_weight.
                if tally.get(&current) == Some(&max_weight) {
                    best = current;
                }

                if best != current {
                    labels[i] = best;
                    changed = true;
                }
            }

            if !changed {
                converged = true;
                break;
            }
        }

        let termination = if converged {
            Termination::Converged { iterations }
        } else {
            Termination::IterationCeiling { iterations }
        };

        let mut groups: HashMap<u32, Vec<u32>> = HashMap::new();
        for (i, &label) in labels.iter().enumerate() {
            groups.entry(label).or_default().push(i as u32);
        }

        let mut communities: Vec<FileCommunity> = groups
            .into_values()
            .map(|indices| {
                let mut members: Vec<PathBuf> = indices
                    .into_iter()
                    .map(|i| index_to_path[i as usize].clone())
                    .collect();
                members.sort();
                let label = community_label(&members);
                FileCommunity { members, label }
            })
            .collect();

        communities.sort_by(|a, b| {
            b.members
                .len()
                .cmp(&a.members.len())
                .then_with(|| a.members[0].cmp(&b.members[0]))
        });

        let degeneracy = detect_degeneracy(node_count as u32, &communities);

        CommunityResult {
            communities,
            node_count: node_count as u32,
            edge_count: weights.len() as u32,
            termination,
            degeneracy,
        }
    }
}

/// Derive a community's label: the longest directory prefix shared by the
/// greatest number of members, lexicographically smallest on a tie, or the
/// lexicographically smallest member path when members share no prefix.
///
/// `members` MUST already be sorted ascending (callers pass the
/// already-sorted `FileCommunity::members`).
///
/// The longest prefix shared by the *greatest number* of members is,
/// component-wise, the longest common prefix of ALL members: any deeper
/// candidate loses coverage (fewer members share it) and any prefix at the
/// same coverage (all members) that is shorter is not the longest. So this
/// reduces to computing the component-wise longest common prefix (LCA) of
/// every member path — a unique, deterministic value, which is why no
/// separate "count tie" step is needed here: the LCA computation itself
/// never admits a tie. When that LCA has at most one component (empty, or
/// just a bare filesystem root — `/` on Unix, a drive prefix on Windows —
/// which is not a meaningful "directory prefix"), members share no
/// prefix and the lexicographically smallest member path (`members[0]`,
/// given the sort precondition) is used instead. A singleton community's
/// LCA is trivially its own single path, which is already the correct
/// answer (identical to the fallback).
fn community_label(members: &[PathBuf]) -> String {
    debug_assert!(!members.is_empty(), "a community always has >= 1 member");

    let mut common: Vec<std::ffi::OsString> = members[0]
        .components()
        .map(|c| c.as_os_str().to_os_string())
        .collect();

    for member in &members[1..] {
        let comps: Vec<std::ffi::OsString> = member
            .components()
            .map(|c| c.as_os_str().to_os_string())
            .collect();
        let shared = common
            .iter()
            .zip(comps.iter())
            .take_while(|(a, b)| a == b)
            .count();
        common.truncate(shared);
        if common.is_empty() {
            break;
        }
    }

    // A shared prefix of length <= 1 is at most a bare filesystem root
    // (`/` on Unix, a drive prefix on Windows) — not a meaningful
    // "directory prefix" to label a community with. Treat it the same as
    // no shared prefix at all.
    if common.len() <= 1 {
        members[0].to_string_lossy().into_owned()
    } else {
        let mut prefix = PathBuf::new();
        for component in &common {
            prefix.push(component);
        }
        prefix.to_string_lossy().into_owned()
    }
}

/// Flag a degenerate partition (`Designs/GraphQueries` Decision 8, AC-55).
///
/// Below ten nodes neither condition fires — on a tiny graph "one
/// community" is a correct answer, not a failure. `communities` MUST
/// already be sorted descending by member count (the order
/// [`Graph::file_communities`] produces).
fn detect_degeneracy(node_count: u32, communities: &[FileCommunity]) -> Option<Degeneracy> {
    if node_count < 10 {
        return None;
    }

    if let Some(largest) = communities.first() {
        let share_permille = (largest.members.len() as u64 * 1000 / node_count as u64) as u32;
        if share_permille >= 900 {
            return Some(Degeneracy::Giant { share_permille });
        }
    }

    if node_count > 1 && communities.len() as u32 == node_count {
        return Some(Degeneracy::Atomized);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use code_graph_core::{Language, SymbolKind};

    use crate::test_fixtures::{call_edge, inherit_edge, make_fg, sym};

    /// Merge `n` files each with a single free function `f`, with no
    /// edges at all — the "edgeless" fixture.
    fn edgeless_graph(n: usize) -> Graph {
        let mut g = Graph::new();
        for i in 0..n {
            let path = format!("/f{i}.cpp");
            g.merge_file_graph(make_fg(
                &path,
                Language::Cpp,
                vec![sym("f", SymbolKind::Function, &path)],
                vec![],
            ));
        }
        g
    }

    // --- aggregate_file_edges -------------------------------------------

    #[test]
    fn aggregate_assigns_indices_in_trie_order() {
        let mut g = Graph::new();
        for path in ["/b.cpp", "/a.cpp", "/c.cpp"] {
            g.merge_file_graph(make_fg(
                path,
                Language::Cpp,
                vec![sym("f", SymbolKind::Function, path)],
                vec![],
            ));
        }
        let (index_to_path, _weights) = g.aggregate_file_edges();
        assert_eq!(
            index_to_path,
            vec![
                PathBuf::from("/a.cpp"),
                PathBuf::from("/b.cpp"),
                PathBuf::from("/c.cpp"),
            ]
        );
    }

    #[test]
    fn aggregate_folds_calls_and_includes_undirected() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("f", SymbolKind::Function, "/a.cpp")],
            vec![call_edge("/a.cpp:f", "/b.cpp:g", "/a.cpp", 1)],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("g", SymbolKind::Function, "/b.cpp")],
            vec![],
        ));

        let (index_to_path, weights) = g.aggregate_file_edges();
        let ia = index_to_path
            .iter()
            .position(|p| p == &PathBuf::from("/a.cpp"))
            .unwrap() as u32;
        let ib = index_to_path
            .iter()
            .position(|p| p == &PathBuf::from("/b.cpp"))
            .unwrap() as u32;
        let key = (ia.min(ib), ia.max(ib));
        assert_eq!(weights.get(&key), Some(&1));
        assert_eq!(weights.len(), 1);
    }

    #[test]
    fn aggregate_skips_self_pairs() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("f", SymbolKind::Function, "/a.cpp"),
                sym("h", SymbolKind::Function, "/a.cpp"),
            ],
            vec![call_edge("/a.cpp:f", "/a.cpp:h", "/a.cpp", 1)],
        ));

        let (_index_to_path, weights) = g.aggregate_file_edges();
        assert!(weights.is_empty(), "same-file call must not enter weights");
    }

    #[test]
    fn aggregate_filters_inherits_and_overrides_out_of_calls() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("Sub", SymbolKind::Class, "/a.cpp")],
            vec![inherit_edge("/a.cpp:Sub", "/b.cpp:Base", "/a.cpp")],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("Base", SymbolKind::Class, "/b.cpp")],
            vec![],
        ));

        let (_index_to_path, weights) = g.aggregate_file_edges();
        assert!(
            weights.is_empty(),
            "Inherits edges must not become community weight"
        );
    }

    // --- file_communities -------------------------------------------------

    #[test]
    fn two_dense_clusters_joined_by_one_edge_separate() {
        let mut g = Graph::new();
        // Cluster 1: a0..a3 all call each other's file's function densely.
        for i in 0..4 {
            let path = format!("/a{i}.cpp");
            let mut edges = vec![];
            for j in 0..4 {
                if i != j {
                    edges.push(call_edge(
                        &format!("/a{i}.cpp:f"),
                        &format!("/a{j}.cpp:f"),
                        &path,
                        1,
                    ));
                }
            }
            g.merge_file_graph(make_fg(
                &path,
                Language::Cpp,
                vec![sym("f", SymbolKind::Function, &path)],
                edges,
            ));
        }
        // Cluster 2: b0..b3 densely connected the same way.
        for i in 0..4 {
            let path = format!("/b{i}.cpp");
            let mut edges = vec![];
            for j in 0..4 {
                if i != j {
                    edges.push(call_edge(
                        &format!("/b{i}.cpp:f"),
                        &format!("/b{j}.cpp:f"),
                        &path,
                        1,
                    ));
                }
            }
            g.merge_file_graph(make_fg(
                &path,
                Language::Cpp,
                vec![sym("f", SymbolKind::Function, &path)],
                edges,
            ));
        }
        // Single bridge edge a0 -> b0.
        g.merge_file_graph(make_fg(
            "/a0.cpp",
            Language::Cpp,
            vec![sym("f", SymbolKind::Function, "/a0.cpp")],
            {
                let mut edges = vec![];
                for j in 1..4 {
                    edges.push(call_edge(
                        "/a0.cpp:f",
                        &format!("/a{j}.cpp:f"),
                        "/a0.cpp",
                        1,
                    ));
                }
                edges.push(call_edge("/a0.cpp:f", "/b0.cpp:f", "/a0.cpp", 99));
                edges
            },
        ));

        let result = g.file_communities(50);
        assert_eq!(result.communities.len(), 2, "two dense clusters expected");
        let sizes: Vec<usize> = result.communities.iter().map(|c| c.members.len()).collect();
        assert_eq!(sizes, vec![4, 4]);
    }

    #[test]
    fn star_graph_reports_giant() {
        // Hub file included by 12 leaves -> one dominant community.
        let n = 12;
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/hub.cpp",
            Language::Cpp,
            vec![sym("h", SymbolKind::Function, "/hub.cpp")],
            vec![],
        ));
        for i in 0..n {
            let path = format!("/leaf{i}.cpp");
            g.merge_file_graph(make_fg(
                &path,
                Language::Cpp,
                vec![sym("f", SymbolKind::Function, &path)],
                vec![call_edge(
                    &format!("/leaf{i}.cpp:f"),
                    "/hub.cpp:h",
                    &path,
                    1,
                )],
            ));
        }

        let result = g.file_communities(50);
        assert_eq!(
            result.degeneracy,
            Some(Degeneracy::Giant {
                share_permille: 1000
            })
        );
    }

    #[test]
    fn edgeless_graph_reports_atomized() {
        let g = edgeless_graph(12);
        let result = g.file_communities(50);
        assert_eq!(result.node_count, 12);
        assert_eq!(result.communities.len(), 12);
        assert_eq!(result.degeneracy, Some(Degeneracy::Atomized));
    }

    #[test]
    fn nine_node_collapse_reports_no_degeneracy() {
        // 8 leaves + 1 hub = 9 files total, all connected to one hub ->
        // single community, but below the node_count >= 10 floor, so
        // neither Giant nor Atomized fires.
        let n = 8;
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/hub.cpp",
            Language::Cpp,
            vec![sym("h", SymbolKind::Function, "/hub.cpp")],
            vec![],
        ));
        for i in 0..n {
            let path = format!("/leaf{i}.cpp");
            g.merge_file_graph(make_fg(
                &path,
                Language::Cpp,
                vec![sym("f", SymbolKind::Function, &path)],
                vec![call_edge(
                    &format!("/leaf{i}.cpp:f"),
                    "/hub.cpp:h",
                    &path,
                    1,
                )],
            ));
        }

        let result = g.file_communities(50);
        assert_eq!(result.node_count, 9);
        assert_eq!(result.degeneracy, None);
    }

    #[test]
    fn self_loop_bias_does_not_prevent_cross_file_merge() {
        // /a.cpp has many intra-file calls (would-be self-loops if not
        // filtered) plus a single genuine cross-file call to /b.cpp. If
        // self-pairs leaked into the weight table, /a.cpp's overwhelming
        // vote for its own label would swamp the one real cross-file edge
        // and it would never merge with /b.cpp.
        let mut g = Graph::new();
        let mut a_symbols = vec![];
        let mut a_edges = vec![];
        for i in 0..20 {
            a_symbols.push(sym(&format!("f{i}"), SymbolKind::Function, "/a.cpp"));
        }
        for i in 0..19 {
            a_edges.push(call_edge(
                &format!("/a.cpp:f{i}"),
                &format!("/a.cpp:f{}", i + 1),
                "/a.cpp",
                1,
            ));
        }
        a_edges.push(call_edge("/a.cpp:f0", "/b.cpp:g", "/a.cpp", 1));
        g.merge_file_graph(make_fg("/a.cpp", Language::Cpp, a_symbols, a_edges));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("g", SymbolKind::Function, "/b.cpp")],
            vec![],
        ));

        let result = g.file_communities(50);
        assert_eq!(
            result.communities.len(),
            1,
            "the one real cross-file edge must merge /a.cpp with /b.cpp"
        );
        assert_eq!(result.communities[0].members.len(), 2);
    }

    #[test]
    fn edge_kind_filter_yields_no_community_structure() {
        // Only Inherits edges cross file boundaries -> weight table stays
        // empty -> every file is its own community.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("Sub", SymbolKind::Class, "/a.cpp")],
            vec![inherit_edge("/a.cpp:Sub", "/b.cpp:Base", "/a.cpp")],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("Base", SymbolKind::Class, "/b.cpp")],
            vec![],
        ));

        let result = g.file_communities(50);
        assert_eq!(result.communities.len(), 2);
        assert_eq!(result.edge_count, 0);
    }

    #[test]
    fn determinism_twenty_runs_byte_identical_including_labels() {
        // A graph with enough branching that label propagation has real
        // choices to make (not a trivially single-answer topology).
        let mut g = Graph::new();
        for i in 0..6 {
            let path = format!("/mod_a/f{i}.cpp");
            let mut edges = vec![];
            for j in 0..6 {
                if i != j {
                    edges.push(call_edge(
                        &format!("/mod_a/f{i}.cpp:f"),
                        &format!("/mod_a/f{j}.cpp:f"),
                        &path,
                        1,
                    ));
                }
            }
            g.merge_file_graph(make_fg(
                &path,
                Language::Cpp,
                vec![sym("f", SymbolKind::Function, &path)],
                edges,
            ));
        }
        for i in 0..6 {
            let path = format!("/mod_b/g{i}.cpp");
            let mut edges = vec![];
            for j in 0..6 {
                if i != j {
                    edges.push(call_edge(
                        &format!("/mod_b/g{i}.cpp:f"),
                        &format!("/mod_b/g{j}.cpp:f"),
                        &path,
                        1,
                    ));
                }
            }
            g.merge_file_graph(make_fg(
                &path,
                Language::Cpp,
                vec![sym("f", SymbolKind::Function, &path)],
                edges,
            ));
        }
        // A couple of cross-module bridges so propagation isn't trivial.
        g.merge_file_graph(make_fg(
            "/mod_a/f0.cpp",
            Language::Cpp,
            vec![sym("f", SymbolKind::Function, "/mod_a/f0.cpp")],
            {
                let mut edges = vec![];
                for j in 1..6 {
                    edges.push(call_edge(
                        "/mod_a/f0.cpp:f",
                        &format!("/mod_a/f{j}.cpp:f"),
                        "/mod_a/f0.cpp",
                        1,
                    ));
                }
                edges.push(call_edge(
                    "/mod_a/f0.cpp:f",
                    "/mod_b/g0.cpp:f",
                    "/mod_a/f0.cpp",
                    2,
                ));
                edges
            },
        ));

        let first = g.file_communities(50);
        for _ in 0..20 {
            let run = g.file_communities(50);
            assert_eq!(
                run, first,
                "file_communities must be byte-identical run to run"
            );
        }
    }

    #[test]
    fn iteration_ceiling_reported_when_hit() {
        let g = edgeless_graph(3);
        let result = g.file_communities(0);
        assert_eq!(
            result.termination,
            Termination::IterationCeiling { iterations: 0 }
        );
    }

    #[test]
    fn converged_reported_on_natural_fixed_point() {
        let g = edgeless_graph(3);
        let result = g.file_communities(50);
        assert!(matches!(result.termination, Termination::Converged { .. }));
    }

    #[test]
    fn community_label_uses_longest_common_directory_prefix() {
        let members = vec![
            PathBuf::from("/proj/mod/a.cpp"),
            PathBuf::from("/proj/mod/sub/b.cpp"),
        ];
        assert_eq!(community_label(&members), "/proj/mod");
    }

    #[test]
    fn community_label_falls_back_to_smallest_path_with_no_shared_prefix() {
        let members = vec![PathBuf::from("/a/x.cpp"), PathBuf::from("/z/y.cpp")];
        assert_eq!(community_label(&members), "/a/x.cpp");
    }

    #[test]
    fn community_label_singleton_is_its_own_path() {
        let members = vec![PathBuf::from("/only/one.cpp")];
        assert_eq!(community_label(&members), "/only/one.cpp");
    }
}
