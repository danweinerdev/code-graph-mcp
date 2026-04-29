//! Coupling metrics and diagram BFS / Mermaid rendering.
//!
//! Mirrors the Go reference at `internal/graph/graph.go` lines 488–567
//! (`Coupling`, `IncomingCoupling`, `mermaidLabel`) and `internal/graph/diagram.go`
//! (full file: [`DiagramEdge`], [`DiagramResult`], `DiagramCallGraph`,
//! `DiagramFileGraph`, `DiagramInheritance`, `RenderMermaid`).
//!
//! All BFS traversals are bounded by both `depth` and `max_nodes`. Edges
//! are deduplicated before being emitted, and the truncation guard from
//! the Go reference (`!visited[from] || !visited[to]`) is preserved so
//! a max-nodes cutoff doesn't leave dangling endpoints in the output.
//!
//! Determinism note: [`DiagramResult::render_mermaid`] produces
//! byte-identical output for a fixed `DiagramResult` — the
//! [`indexmap::IndexMap`]-based node-id assignment preserves insertion
//! order across invocations. The Go reference iterates a
//! `map[string]bool` whose order is randomized per process; that
//! randomness is not portable to a test gate, so the Rust port pins
//! determinism at this layer instead.
//!
//! The BFS methods (`diagram_call_graph`, `diagram_file_graph`,
//! `diagram_inheritance`), in contrast, traverse `HashMap`-backed
//! adjacency maps (`adj` / `radj` / `includes`) whose iteration order
//! is randomized. The resulting [`DiagramResult::edges`] ordering is
//! **not** stable across invocations — only the *set* of emitted edges
//! is deterministic. Tests that need byte-equality of rendered output
//! must construct the `DiagramResult` directly rather than rely on BFS
//! output; tests over BFS results must compare edges as a set (e.g. via
//! `contains` checks on `(from, to)` pairs).
//!
//! Locking is not handled in this module. Task 2.6 wraps [`Graph`] in
//! `parking_lot::RwLock`; until then these methods take `&self` and rely
//! on the caller for synchronization.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use codegraph_core::{EdgeKind, SymbolId, SymbolKind};
use indexmap::IndexMap;

use crate::graph::Node;
use crate::Graph;

/// One labeled edge in a [`DiagramResult`]. The `from`/`to` are display
/// names already (post `mermaid_label` for symbol diagrams, basename for
/// file diagrams, raw class name for inheritance) — [`DiagramResult::render_mermaid`]
/// does not transform them further.
///
/// Field order matches the Go shape's JSON tags exactly so a mixed
/// Go/Rust client cluster sees identical wire-format output.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiagramEdge {
    pub from: String,
    pub to: String,
    pub label: String,
}

/// BFS traversal result ready for [`DiagramResult::render_mermaid`].
///
/// `center` is the seed node (its display name); `edges` is the
/// deduplicated edge list collected by the BFS. `edges` is `Vec`, never
/// `Option`, so JSON serialization yields `[]` (not `null`) when the BFS
/// finds no edges — preserving the wire-format invariant from the
/// LLMOptimization debrief. Do not add `skip_serializing_if` here; the
/// empty-edges case is meaningful and must be visible to clients.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize, Default)]
pub struct DiagramResult {
    pub center: String,
    #[serde(default)]
    pub edges: Vec<DiagramEdge>,
}

impl Graph {
    /// Outgoing cross-file edge counts: how many calls + includes leave
    /// `path` for each other file.
    ///
    /// Calls inside `path` (same-file) are excluded — only edges that
    /// cross a file boundary count. Returns an empty `HashMap` for
    /// unknown paths so JSON serializes as `{}`, never `null`.
    ///
    /// Mirrors the Go reference at `graph.go:488–516`. The Rust port
    /// uses `HashMap<PathBuf, u32>` where Go used `map[string]int`; the
    /// `u32` is sufficient because cross-file edge counts cannot exceed
    /// the symbol count and our per-file budget is well under 4 billion.
    pub fn coupling(&self, path: &Path) -> HashMap<PathBuf, u32> {
        let mut counts: HashMap<PathBuf, u32> = HashMap::new();

        // Cross-file calls originating from symbols in this file.
        if let Some(entry) = self.files.get(path) {
            for id in &entry.symbol_ids {
                let Some(adj_entries) = self.adj.get(id) else {
                    continue;
                };
                for edge in adj_entries {
                    if edge.kind != EdgeKind::Calls {
                        continue;
                    }
                    if let Some(target_node) = self.nodes.get(&edge.target) {
                        let target_file = PathBuf::from(&target_node.symbol.file);
                        if target_file != path {
                            *counts.entry(target_file).or_insert(0) += 1;
                        }
                    }
                }
            }
        }

        // Include edges from this file. The includes map is keyed by the
        // source file directly, so missing-key just means no includes —
        // the per-target counter increments unconditionally.
        if let Some(incs) = self.includes.get(path) {
            for inc in incs {
                *counts.entry(inc.clone()).or_insert(0) += 1;
            }
        }

        counts
    }

    /// Incoming cross-file edge counts: how many calls + includes point
    /// **into** `path` from each other file.
    ///
    /// The reverse-include scan is `O(N×M)` (every other file's include
    /// list is scanned) — that mirrors the Go reference exactly. Phase 3
    /// may add a reverse-include index; this task just preserves parity.
    ///
    /// Mirrors the Go reference at `graph.go:518–553`.
    pub fn incoming_coupling(&self, path: &Path) -> HashMap<PathBuf, u32> {
        let mut counts: HashMap<PathBuf, u32> = HashMap::new();

        // Incoming call edges into symbols in this file. radj[id] lists
        // edges whose `target` field is the *caller's* ID (because radj
        // is the reverse adjacency: target → list-of-(caller, kind)).
        if let Some(entry) = self.files.get(path) {
            for id in &entry.symbol_ids {
                let Some(radj_entries) = self.radj.get(id) else {
                    continue;
                };
                for edge in radj_entries {
                    if edge.kind != EdgeKind::Calls {
                        continue;
                    }
                    if let Some(caller_node) = self.nodes.get(&edge.target) {
                        let caller_file = PathBuf::from(&caller_node.symbol.file);
                        if caller_file != path {
                            *counts.entry(caller_file).or_insert(0) += 1;
                        }
                    }
                }
            }
        }

        // Files that include this file. The includes map has no reverse
        // index (that's the Phase-3 optimization), so we scan every
        // entry and check membership of `path` in its target list.
        for (from, incs) in &self.includes {
            if from == path {
                continue;
            }
            for inc in incs {
                if inc == path {
                    *counts.entry(from.clone()).or_insert(0) += 1;
                }
            }
        }

        counts
    }

    /// BFS over the call graph centered on `start_id`, returning a
    /// [`DiagramResult`] ready for Mermaid rendering. Returns `None` if
    /// `start_id` is not a known symbol; otherwise always returns
    /// `Some` (possibly with an empty `edges` vec).
    ///
    /// `depth = 0` is normalized to `1`; `max_nodes = 0` is normalized
    /// to `30`. Both forward (`adj`) and reverse (`radj`) `Calls` edges
    /// are walked so the diagram shows both who-calls-X and who-X-calls.
    ///
    /// The truncation guard `!visited[from] || !visited[to]` after the
    /// BFS is essential: when `max_nodes` cuts the visit budget mid-walk,
    /// some raw edges have endpoints that never made it into `visited`,
    /// and emitting them would produce dangling nodes in the rendered
    /// graph. The guard drops those edges silently — exact Go parity.
    pub fn diagram_call_graph(
        &self,
        start_id: &str,
        depth: u32,
        max_nodes: u32,
    ) -> Option<DiagramResult> {
        if !self.nodes.contains_key(start_id) {
            return None;
        }

        let depth = if depth == 0 { 1 } else { depth };
        let max_nodes = if max_nodes == 0 { 30 } else { max_nodes } as usize;

        let mut visited: HashSet<SymbolId> = HashSet::new();
        visited.insert(start_id.to_string());

        let mut queue: VecDeque<(SymbolId, u32)> = VecDeque::new();
        queue.push_back((start_id.to_string(), 0));

        // raw_edges always stores (source, target) in forward direction:
        // adj traversal pushes (curr, target); radj traversal pushes
        // (radj_entry.target, curr) because radj's `target` field is the
        // *source* of the original Calls edge.
        let mut raw_edges: Vec<(String, String)> = Vec::new();

        while let Some((curr_id, curr_depth)) = queue.pop_front() {
            if visited.len() >= max_nodes {
                break;
            }
            if curr_depth >= depth {
                continue;
            }

            if let Some(entries) = self.adj.get(&curr_id) {
                for entry in entries {
                    if entry.kind != EdgeKind::Calls {
                        continue;
                    }
                    raw_edges.push((curr_id.clone(), entry.target.clone()));
                    if !visited.contains(&entry.target) && visited.len() < max_nodes {
                        visited.insert(entry.target.clone());
                        queue.push_back((entry.target.clone(), curr_depth + 1));
                    }
                }
            }

            if let Some(entries) = self.radj.get(&curr_id) {
                for entry in entries {
                    if entry.kind != EdgeKind::Calls {
                        continue;
                    }
                    // radj's `target` is the SOURCE of the original
                    // Calls edge; emit the forward direction.
                    raw_edges.push((entry.target.clone(), curr_id.clone()));
                    if !visited.contains(&entry.target) && visited.len() < max_nodes {
                        visited.insert(entry.target.clone());
                        queue.push_back((entry.target.clone(), curr_depth + 1));
                    }
                }
            }
        }

        let mut result = DiagramResult {
            center: mermaid_label(start_id, &self.nodes),
            edges: Vec::new(),
        };
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for (from, to) in raw_edges {
            if seen.contains(&(from.clone(), to.clone())) {
                continue;
            }
            // Truncation guard: when max_nodes cuts mid-walk, one
            // endpoint may not be in `visited`. Dropping the edge keeps
            // the rendered graph fully connected through `visited`.
            if !visited.contains(&from) || !visited.contains(&to) {
                continue;
            }
            seen.insert((from.clone(), to.clone()));
            result.edges.push(DiagramEdge {
                from: mermaid_label(&from, &self.nodes),
                to: mermaid_label(&to, &self.nodes),
                label: "calls".to_string(),
            });
        }
        Some(result)
    }

    /// BFS over the include graph centered on `start_path`. Returns
    /// `None` if the path is not a known file; otherwise returns
    /// `Some(DiagramResult)`.
    ///
    /// `depth = 0` is normalized to `1`; `max_nodes = 0` to `30`. Both
    /// outgoing (the file's includes) and incoming (other files that
    /// include this one) edges are walked. The incoming scan is O(N×M)
    /// — every other file's include list is checked at every BFS step.
    /// This mirrors the Go reference exactly; Phase 3 may add a reverse
    /// include index.
    ///
    /// Display names use the file basename (`Path::file_name`) so a
    /// rendered graph stays readable even with deep paths. The center
    /// falls back to the full path string if the path has no basename
    /// (e.g. ends in `/` — unlikely for indexed files but possible).
    pub fn diagram_file_graph(
        &self,
        start_path: &Path,
        depth: u32,
        max_nodes: u32,
    ) -> Option<DiagramResult> {
        if !self.files.contains_key(start_path) {
            return None;
        }

        let depth = if depth == 0 { 1 } else { depth };
        let max_nodes = if max_nodes == 0 { 30 } else { max_nodes } as usize;

        let mut visited: HashSet<PathBuf> = HashSet::new();
        visited.insert(start_path.to_path_buf());

        let mut queue: VecDeque<(PathBuf, u32)> = VecDeque::new();
        queue.push_back((start_path.to_path_buf(), 0));

        let mut raw_edges: Vec<(PathBuf, PathBuf)> = Vec::new();

        while let Some((curr, curr_depth)) = queue.pop_front() {
            if visited.len() >= max_nodes {
                break;
            }
            if curr_depth >= depth {
                continue;
            }

            // Outgoing includes from `curr`.
            if let Some(incs) = self.includes.get(&curr) {
                for inc in incs {
                    raw_edges.push((curr.clone(), inc.clone()));
                    if !visited.contains(inc) && visited.len() < max_nodes {
                        visited.insert(inc.clone());
                        queue.push_back((inc.clone(), curr_depth + 1));
                    }
                }
            }

            // Incoming includes: scan every other file's include list
            // for entries pointing at `curr`. Faithful O(N×M) port.
            for (from, incs) in &self.includes {
                for inc in incs {
                    if inc == &curr {
                        raw_edges.push((from.clone(), curr.clone()));
                        if !visited.contains(from) && visited.len() < max_nodes {
                            visited.insert(from.clone());
                            queue.push_back((from.clone(), curr_depth + 1));
                        }
                    }
                }
            }
        }

        let center = filename_only(start_path);
        let mut result = DiagramResult {
            center,
            edges: Vec::new(),
        };
        let mut seen: HashSet<(PathBuf, PathBuf)> = HashSet::new();
        for (from, to) in raw_edges {
            if seen.contains(&(from.clone(), to.clone())) {
                continue;
            }
            if !visited.contains(&from) || !visited.contains(&to) {
                continue;
            }
            seen.insert((from.clone(), to.clone()));
            result.edges.push(DiagramEdge {
                from: filename_only(&from),
                to: filename_only(&to),
                label: "includes".to_string(),
            });
        }
        Some(result)
    }

    /// BFS over the inheritance graph centered on the class named
    /// `name`. Returns `None` if no symbol with the given name exists
    /// with a class-like kind (`Class`, `Struct`, `Interface`, `Trait`)
    /// — the same widened filter used by [`Graph::class_hierarchy`].
    ///
    /// `depth = 0` is normalized to **2** (note: NOT 1 like the call /
    /// file diagrams — the Go reference picks 2 here so the default
    /// view shows direct base + grandparent in one shot).
    /// `max_nodes = 0` is normalized to 30.
    ///
    /// The BFS walks `Inherits` edges in both directions (forward via
    /// `adj` for bases, reverse via `radj` for derived). Display names
    /// are the raw class names — classes don't have a `Parent::Name`
    /// form to flatten and don't carry file paths to shorten.
    pub fn diagram_inheritance(
        &self,
        name: &str,
        depth: u32,
        max_nodes: u32,
    ) -> Option<DiagramResult> {
        // Existence + kind check using the widened filter. Mirrors
        // class_hierarchy's pre-flight (algorithms.rs).
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

        // Inheritance default depth is 2, NOT 1. See `diagram.go:183`.
        let depth = if depth == 0 { 2 } else { depth };
        let max_nodes = if max_nodes == 0 { 30 } else { max_nodes } as usize;

        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(name.to_string());

        let mut queue: VecDeque<(String, u32)> = VecDeque::new();
        queue.push_back((name.to_string(), 0));

        let mut raw_edges: Vec<(String, String)> = Vec::new();

        while let Some((curr, curr_depth)) = queue.pop_front() {
            if visited.len() >= max_nodes {
                break;
            }
            if curr_depth >= depth {
                continue;
            }

            if let Some(entries) = self.adj.get(&curr) {
                for entry in entries {
                    if entry.kind != EdgeKind::Inherits {
                        continue;
                    }
                    raw_edges.push((curr.clone(), entry.target.clone()));
                    if !visited.contains(&entry.target) && visited.len() < max_nodes {
                        visited.insert(entry.target.clone());
                        queue.push_back((entry.target.clone(), curr_depth + 1));
                    }
                }
            }

            if let Some(entries) = self.radj.get(&curr) {
                for entry in entries {
                    if entry.kind != EdgeKind::Inherits {
                        continue;
                    }
                    // radj's target is the SOURCE of the original
                    // Inherits edge; emit forward direction (child→parent).
                    raw_edges.push((entry.target.clone(), curr.clone()));
                    if !visited.contains(&entry.target) && visited.len() < max_nodes {
                        visited.insert(entry.target.clone());
                        queue.push_back((entry.target.clone(), curr_depth + 1));
                    }
                }
            }
        }

        let mut result = DiagramResult {
            center: name.to_string(),
            edges: Vec::new(),
        };
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for (from, to) in raw_edges {
            if seen.contains(&(from.clone(), to.clone())) {
                continue;
            }
            if !visited.contains(&from) || !visited.contains(&to) {
                continue;
            }
            seen.insert((from.clone(), to.clone()));
            result.edges.push(DiagramEdge {
                from,
                to,
                label: "inherits".to_string(),
            });
        }
        Some(result)
    }
}

impl DiagramResult {
    /// Convert this result into a Mermaid `graph DIR` string.
    ///
    /// Returns an empty `String` when `edges` is empty — matches the Go
    /// reference (`if dr == nil || len(dr.Edges) == 0`). Empty `direction`
    /// defaults to `"TD"`. When `styled` is `true` the center node gets
    /// `:::center` and a `classDef center fill:#f96,stroke:#333` trailer
    /// is appended so the rendered diagram visually distinguishes the
    /// seed.
    ///
    /// Determinism: node IDs (`n0`, `n1`, ...) are assigned in insertion
    /// order via [`indexmap::IndexMap`]. The Go reference iterates a
    /// `map[string]bool`, whose order is randomized per process —
    /// useful in production (no hidden ordering invariants for clients
    /// to rely on) but unportable to a byte-equality test gate. The
    /// IndexMap port produces stable output and the
    /// `render_mermaid_deterministic` test pins this contract.
    pub fn render_mermaid(&self, direction: &str, styled: bool) -> String {
        if self.edges.is_empty() {
            return String::new();
        }

        let direction = if direction.is_empty() {
            "TD"
        } else {
            direction
        };

        // Collect unique node names in insertion order. Using IndexMap
        // (rather than HashMap + Vec or sorted Vec) keeps the assignment
        // O(N) and stable under repeat invocations.
        let mut short_ids: IndexMap<String, String> = IndexMap::new();
        for edge in &self.edges {
            // `entry()` here returns a vacant entry on first sight and
            // populates `nN`; subsequent sightings are a no-op.
            let next_id = format!("n{}", short_ids.len());
            short_ids.entry(edge.from.clone()).or_insert(next_id);
            let next_id = format!("n{}", short_ids.len());
            short_ids.entry(edge.to.clone()).or_insert(next_id);
        }

        let mut out = String::new();
        out.push_str(&format!("graph {direction}\n"));

        for (name, sid) in &short_ids {
            if styled && name == &self.center {
                out.push_str(&format!("    {sid}[\"{name}\"]:::center\n"));
            } else {
                out.push_str(&format!("    {sid}[\"{name}\"]\n"));
            }
        }

        for edge in &self.edges {
            // `unwrap` here is safe: we just populated the map from the
            // edges, so every endpoint has a short id. A panic would
            // mean the loop above failed to insert, which would also
            // break the node-emission loop — caught in tests.
            let from_sid = short_ids
                .get(&edge.from)
                .expect("edge endpoint must be in short_ids");
            let to_sid = short_ids
                .get(&edge.to)
                .expect("edge endpoint must be in short_ids");
            if edge.label.is_empty() {
                out.push_str(&format!("    {from_sid} --> {to_sid}\n"));
            } else {
                let label = &edge.label;
                out.push_str(&format!("    {from_sid} -->|{label}| {to_sid}\n"));
            }
        }

        if styled {
            out.push_str("    classDef center fill:#f96,stroke:#333\n");
        }

        out
    }
}

/// Display label for a node ID in a call-graph diagram.
///
/// If `id` is a known symbol, return `Parent::Name` when `Parent` is
/// non-empty, otherwise `Name`. If `id` is *not* a known symbol it might
/// be a bare callee name extracted by the parser (an unresolved external
/// call) — in that case absolute paths are shortened to their basename
/// for readability, while non-path identifiers are returned verbatim.
///
/// Mirrors the Go reference at `graph.go:555–567`. The `is_absolute`
/// check uses [`Path::is_absolute`] which is correct on Unix; on
/// Windows it's slightly more lenient than Go's `filepath.IsAbs` but
/// the Rust binary is built and run on the same platforms as the Go
/// reference, so the divergence has no observable effect.
fn mermaid_label(id: &str, nodes: &HashMap<SymbolId, Node>) -> String {
    if let Some(node) = nodes.get(id) {
        if !node.symbol.parent.is_empty() {
            return format!("{}::{}", node.symbol.parent, node.symbol.name);
        }
        return node.symbol.name.clone();
    }
    let p = Path::new(id);
    if p.is_absolute() {
        if let Some(base) = p.file_name() {
            return base.to_string_lossy().into_owned();
        }
    }
    id.to_string()
}

/// Display name for a file path in a file-graph diagram. Returns the
/// basename when present, otherwise falls back to the full path string.
/// Mirrors `filepath.Base` semantics for the cases this binary actually
/// indexes (always full paths to real files).
fn filename_only(path: &Path) -> String {
    match path.file_name() {
        Some(base) => base.to_string_lossy().into_owned(),
        None => path.to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::{call_edge, include_edge, inherit_edge, make_fg, sym, sym_full};
    use codegraph_core::Language;

    // ----- Coupling -----

    #[test]
    fn coupling_outgoing_calls_includes() {
        let mut g = Graph::new();
        // /a.cpp: foo calls bar (same file, doesn't count) and ext (in
        // /b.cpp, counts), plus #include "/b.h" (counts).
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("foo", SymbolKind::Function, "/a.cpp"),
                sym("bar", SymbolKind::Function, "/a.cpp"),
            ],
            vec![
                call_edge("/a.cpp:foo", "/a.cpp:bar", "/a.cpp", 1),
                call_edge("/a.cpp:foo", "/b.cpp:ext", "/a.cpp", 2),
                include_edge("/a.cpp", "/b.h", "/a.cpp"),
            ],
        ));
        // /b.cpp must exist so the call target resolves.
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("ext", SymbolKind::Function, "/b.cpp")],
            vec![],
        ));

        let counts = g.coupling(&PathBuf::from("/a.cpp"));
        assert_eq!(
            counts.len(),
            2,
            "expected 2 cross-file targets, got {counts:?}"
        );
        assert_eq!(counts[&PathBuf::from("/b.cpp")], 1, "1 cross-file call");
        assert_eq!(counts[&PathBuf::from("/b.h")], 1, "1 include");
    }

    #[test]
    fn coupling_unknown_path_returns_empty() {
        let g = Graph::new();
        let counts = g.coupling(&PathBuf::from("/never-merged.cpp"));
        // HashMap, not Option — JSON must serialize as `{}`.
        assert!(counts.is_empty());
    }

    #[test]
    fn coupling_same_file_calls_excluded() {
        // Pure same-file call graph — `coupling` should report nothing.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("a", SymbolKind::Function, "/a.cpp"),
                sym("b", SymbolKind::Function, "/a.cpp"),
            ],
            vec![call_edge("/a.cpp:a", "/a.cpp:b", "/a.cpp", 1)],
        ));
        let counts = g.coupling(&PathBuf::from("/a.cpp"));
        assert!(
            counts.is_empty(),
            "same-file calls must not contribute: {counts:?}",
        );
    }

    #[test]
    fn incoming_coupling_calls_and_includes_merge() {
        // /a.cpp BOTH calls into /b.cpp AND includes /b.cpp. The two
        // contributions hit the same `from` key, so the count is 2.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("caller", SymbolKind::Function, "/a.cpp")],
            vec![
                call_edge("/a.cpp:caller", "/b.cpp:target", "/a.cpp", 1),
                include_edge("/a.cpp", "/b.cpp", "/a.cpp"),
            ],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("target", SymbolKind::Function, "/b.cpp")],
            vec![],
        ));

        let counts = g.incoming_coupling(&PathBuf::from("/b.cpp"));
        assert_eq!(counts.len(), 1);
        assert_eq!(
            counts[&PathBuf::from("/a.cpp")],
            2,
            "1 call + 1 include from /a.cpp must merge into the same key",
        );
    }

    #[test]
    fn incoming_coupling_excludes_self_includes() {
        // Hypothetical self-include (e.g. `#include "self.cpp"` from
        // within self.cpp). The Go `IncomingCoupling` skips `from == path`,
        // so the count must stay at 0.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![],
            vec![include_edge("/a.cpp", "/a.cpp", "/a.cpp")],
        ));
        let counts = g.incoming_coupling(&PathBuf::from("/a.cpp"));
        assert!(
            counts.is_empty(),
            "self-include must not show up in incoming_coupling: {counts:?}",
        );
    }

    // ----- diagram_call_graph -----

    #[test]
    fn diagram_call_graph_unknown_returns_none() {
        let g = Graph::new();
        assert!(g.diagram_call_graph("nonexistent", 1, 30).is_none());
    }

    #[test]
    fn diagram_call_graph_simple_chain() {
        // a -> b -> c. Centered on a, depth=2, returns both edges.
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
            ],
        ));

        let result = g.diagram_call_graph("/x.cpp:a", 2, 30).expect("a is known");
        assert_eq!(result.center, "a");
        assert_eq!(result.edges.len(), 2);
        // Edges are deduplicated and contain exactly the two forward calls.
        let pairs: Vec<(String, String)> = result
            .edges
            .iter()
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();
        assert!(pairs.contains(&("a".to_string(), "b".to_string())));
        assert!(pairs.contains(&("b".to_string(), "c".to_string())));
        for e in &result.edges {
            assert_eq!(e.label, "calls");
        }
    }

    #[test]
    fn diagram_call_graph_max_nodes_truncates() {
        // 5-node chain a -> b -> c -> d -> e. max_nodes=3 caps the
        // visit budget; the truncation guard drops edges with
        // unvisited endpoints, so the result has at most 2 edges
        // among the 3 visited nodes.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            Language::Cpp,
            vec![
                sym("a", SymbolKind::Function, "/x.cpp"),
                sym("b", SymbolKind::Function, "/x.cpp"),
                sym("c", SymbolKind::Function, "/x.cpp"),
                sym("d", SymbolKind::Function, "/x.cpp"),
                sym("e", SymbolKind::Function, "/x.cpp"),
            ],
            vec![
                call_edge("/x.cpp:a", "/x.cpp:b", "/x.cpp", 1),
                call_edge("/x.cpp:b", "/x.cpp:c", "/x.cpp", 2),
                call_edge("/x.cpp:c", "/x.cpp:d", "/x.cpp", 3),
                call_edge("/x.cpp:d", "/x.cpp:e", "/x.cpp", 4),
            ],
        ));

        let result = g.diagram_call_graph("/x.cpp:a", 10, 3).expect("a is known");
        // At most 3 unique nodes participated; therefore at most 2
        // edges among them (a chain of 3 nodes has 2 edges).
        assert!(
            result.edges.len() <= 2,
            "expected ≤2 edges under max_nodes=3, got {}: {:?}",
            result.edges.len(),
            result.edges,
        );
        // First two edges of the chain must be present.
        let pairs: Vec<(String, String)> = result
            .edges
            .iter()
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();
        assert!(pairs.contains(&("a".to_string(), "b".to_string())));
    }

    #[test]
    fn diagram_call_graph_includes_reverse_direction() {
        // caller -> target. Centered on `target` at depth=1, the BFS
        // walks radj and surfaces the inbound edge `caller -> target`.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            Language::Cpp,
            vec![
                sym("caller", SymbolKind::Function, "/x.cpp"),
                sym("target", SymbolKind::Function, "/x.cpp"),
            ],
            vec![call_edge("/x.cpp:caller", "/x.cpp:target", "/x.cpp", 1)],
        ));

        let result = g
            .diagram_call_graph("/x.cpp:target", 1, 30)
            .expect("target is known");
        assert_eq!(result.center, "target");
        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.edges[0].from, "caller");
        assert_eq!(result.edges[0].to, "target");
        assert_eq!(result.edges[0].label, "calls");
    }

    #[test]
    fn diagram_call_graph_dedupes() {
        // a -> b, b -> a (cycle). From a, every edge appears at most
        // once in `result.edges`.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            Language::Cpp,
            vec![
                sym("a", SymbolKind::Function, "/x.cpp"),
                sym("b", SymbolKind::Function, "/x.cpp"),
            ],
            vec![
                call_edge("/x.cpp:a", "/x.cpp:b", "/x.cpp", 1),
                call_edge("/x.cpp:b", "/x.cpp:a", "/x.cpp", 2),
            ],
        ));

        let result = g.diagram_call_graph("/x.cpp:a", 5, 30).expect("a is known");
        let pairs: Vec<(String, String)> = result
            .edges
            .iter()
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();
        // Both directions present, each exactly once.
        let ab = pairs.iter().filter(|p| p.0 == "a" && p.1 == "b").count();
        let ba = pairs.iter().filter(|p| p.0 == "b" && p.1 == "a").count();
        assert_eq!(ab, 1, "a -> b should appear exactly once: {pairs:?}");
        assert_eq!(ba, 1, "b -> a should appear exactly once: {pairs:?}");
    }

    #[test]
    fn diagram_call_graph_depth_zero_normalized_to_one() {
        // a -> b -> c. depth=0 must normalize to 1, surfacing only
        // the immediate edge a -> b. depth=2 (for contrast) would
        // include both edges. Compares zero-vs-one byte-equally and
        // pins the edge count so a regression to "depth=0 means
        // unbounded" or "depth=0 means 2" both fail.
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
            ],
        ));

        let zero = g.diagram_call_graph("/x.cpp:a", 0, 30).expect("a is known");
        let one = g.diagram_call_graph("/x.cpp:a", 1, 30).expect("a is known");
        assert_eq!(
            zero.edges, one.edges,
            "depth=0 must produce the same edges as depth=1",
        );
        assert_eq!(
            zero.edges.len(),
            1,
            "depth=1 must return only the direct edge a -> b: {:?}",
            zero.edges,
        );
        assert_eq!(zero.edges[0].from, "a");
        assert_eq!(zero.edges[0].to, "b");
    }

    #[test]
    fn diagram_call_graph_uses_parent_label_for_methods() {
        // A method symbol with non-empty `parent` gets a "Parent::Name"
        // display label via mermaid_label. Confirms the formatter is
        // wired up — the Go binary's label semantics matter for clients
        // that disambiguate overloads by parent class.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            Language::Cpp,
            vec![
                sym_full(
                    "doWork",
                    SymbolKind::Method,
                    "/x.cpp",
                    "",
                    "MyClass",
                    Language::Cpp,
                ),
                sym("helper", SymbolKind::Function, "/x.cpp"),
            ],
            vec![call_edge(
                "/x.cpp:MyClass::doWork",
                "/x.cpp:helper",
                "/x.cpp",
                1,
            )],
        ));

        let result = g
            .diagram_call_graph("/x.cpp:MyClass::doWork", 1, 30)
            .expect("method is known");
        assert_eq!(result.center, "MyClass::doWork");
        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.edges[0].from, "MyClass::doWork");
    }

    // ----- diagram_file_graph -----

    #[test]
    fn diagram_file_graph_unknown_returns_none() {
        let g = Graph::new();
        let result = g.diagram_file_graph(&PathBuf::from("/nope.cpp"), 1, 30);
        assert!(result.is_none());
    }

    #[test]
    fn diagram_file_graph_simple() {
        // A includes B; B includes C. Centered on A at depth=2 returns
        // both edges.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/A.h",
            Language::Cpp,
            vec![],
            vec![include_edge("/A.h", "/B.h", "/A.h")],
        ));
        g.merge_file_graph(make_fg(
            "/B.h",
            Language::Cpp,
            vec![],
            vec![include_edge("/B.h", "/C.h", "/B.h")],
        ));
        // /C.h must be a known file so the BFS traverses past it
        // (`files` contains-key check gates the public API; the BFS
        // itself walks via `includes` regardless, so /C.h doesn't
        // strictly need to be merged — but we add it for realism).
        g.merge_file_graph(make_fg("/C.h", Language::Cpp, vec![], vec![]));

        let result = g
            .diagram_file_graph(&PathBuf::from("/A.h"), 2, 30)
            .expect("A.h is known");
        assert_eq!(result.center, "A.h");
        assert_eq!(result.edges.len(), 2, "got: {:?}", result.edges);
        let pairs: Vec<(String, String)> = result
            .edges
            .iter()
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();
        assert!(pairs.contains(&("A.h".to_string(), "B.h".to_string())));
        assert!(pairs.contains(&("B.h".to_string(), "C.h".to_string())));
        for e in &result.edges {
            assert_eq!(e.label, "includes");
        }
    }

    #[test]
    fn diagram_file_graph_includes_reverse_for_known_file() {
        // A includes B. Centered on B at depth=1, the reverse-include
        // scan surfaces the edge A -> B.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/A.h",
            Language::Cpp,
            vec![],
            vec![include_edge("/A.h", "/B.h", "/A.h")],
        ));
        g.merge_file_graph(make_fg("/B.h", Language::Cpp, vec![], vec![]));

        let result = g
            .diagram_file_graph(&PathBuf::from("/B.h"), 1, 30)
            .expect("B.h is known");
        assert_eq!(result.center, "B.h");
        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.edges[0].from, "A.h");
        assert_eq!(result.edges[0].to, "B.h");
    }

    #[test]
    fn diagram_file_graph_depth_zero_normalized_to_one() {
        // A includes B includes C. depth=0 normalizes to 1; only
        // the immediate A -> B edge surfaces. Identical edge set
        // to an explicit depth=1 call.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/A.cpp",
            Language::Cpp,
            vec![],
            vec![include_edge("/A.cpp", "/B.cpp", "/A.cpp")],
        ));
        g.merge_file_graph(make_fg(
            "/B.cpp",
            Language::Cpp,
            vec![],
            vec![include_edge("/B.cpp", "/C.cpp", "/B.cpp")],
        ));
        g.merge_file_graph(make_fg("/C.cpp", Language::Cpp, vec![], vec![]));

        let zero = g
            .diagram_file_graph(Path::new("/A.cpp"), 0, 30)
            .expect("A.cpp is known");
        let one = g
            .diagram_file_graph(Path::new("/A.cpp"), 1, 30)
            .expect("A.cpp is known");
        assert_eq!(
            zero.edges, one.edges,
            "depth=0 must produce the same edges as depth=1",
        );
        assert_eq!(
            zero.edges.len(),
            1,
            "depth=1 must return only A -> B: {:?}",
            zero.edges,
        );
        assert_eq!(zero.edges[0].from, "A.cpp");
        assert_eq!(zero.edges[0].to, "B.cpp");
    }

    // ----- diagram_inheritance -----

    #[test]
    fn diagram_inheritance_unknown_returns_none() {
        let g = Graph::new();
        assert!(g.diagram_inheritance("Nope", 1, 30).is_none());
    }

    #[test]
    fn diagram_inheritance_default_depth_is_two() {
        // 5-level chain: GrandBase ← Base ← Mid ← Leaf ← GrandLeaf.
        // We need a chain this long because the depth-0 BFS step
        // already collects both forward (Mid -> Base) and reverse
        // (Leaf -> Mid) edges incident to the seed before any
        // depth-1 expansion runs. A 3-class chain would therefore
        // pass identically for depth=1 and depth=2 — vacuously
        // confirming the default. With 5 classes, depth=2 reaches
        // the second-hop edges (Base -> GrandBase, GrandLeaf -> Leaf)
        // that depth=1 cannot, so the test fails if anyone changes
        // the default from 2 to 1.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/x.cpp",
            Language::Cpp,
            vec![
                sym("GrandBase", SymbolKind::Class, "/x.cpp"),
                sym("Base", SymbolKind::Class, "/x.cpp"),
                sym("Mid", SymbolKind::Class, "/x.cpp"),
                sym("Leaf", SymbolKind::Class, "/x.cpp"),
                sym("GrandLeaf", SymbolKind::Class, "/x.cpp"),
            ],
            vec![
                inherit_edge("Base", "GrandBase", "/x.cpp"),
                inherit_edge("Mid", "Base", "/x.cpp"),
                inherit_edge("Leaf", "Mid", "/x.cpp"),
                inherit_edge("GrandLeaf", "Leaf", "/x.cpp"),
            ],
        ));

        // depth=0 → default (2). All four edges incident to the
        // 2-hop neighborhood of Mid must show up: the immediate
        // pair (Mid -> Base, Leaf -> Mid) plus the second-hop pair
        // (Base -> GrandBase forward, GrandLeaf -> Leaf reverse).
        let result = g
            .diagram_inheritance("Mid", 0, 30)
            .expect("Mid is a known class");
        assert_eq!(result.center, "Mid");
        let pairs: Vec<(String, String)> = result
            .edges
            .iter()
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();
        assert_eq!(
            pairs.len(),
            4,
            "depth=0 (normalized to 2) must surface 4 edges: {pairs:?}",
        );
        assert!(
            pairs.contains(&("Mid".to_string(), "Base".to_string())),
            "first-hop forward Mid -> Base missing: {pairs:?}",
        );
        assert!(
            pairs.contains(&("Leaf".to_string(), "Mid".to_string())),
            "first-hop reverse Leaf -> Mid missing: {pairs:?}",
        );
        assert!(
            pairs.contains(&("Base".to_string(), "GrandBase".to_string())),
            "second-hop forward Base -> GrandBase missing — \
             default depth may have regressed from 2 to 1: {pairs:?}",
        );
        assert!(
            pairs.contains(&("GrandLeaf".to_string(), "Leaf".to_string())),
            "second-hop reverse GrandLeaf -> Leaf missing — \
             default depth may have regressed from 2 to 1: {pairs:?}",
        );
        for e in &result.edges {
            assert_eq!(e.label, "inherits");
        }

        // Sanity check: depth=1 must surface ONLY the first-hop
        // edges. If this assertion ever passes with 4 edges, the
        // depth-clamp at the BFS head broke and the depth=0
        // assertion above is no longer non-vacuous.
        let shallow = g
            .diagram_inheritance("Mid", 1, 30)
            .expect("Mid is a known class");
        let shallow_pairs: Vec<(String, String)> = shallow
            .edges
            .iter()
            .map(|e| (e.from.clone(), e.to.clone()))
            .collect();
        assert_eq!(
            shallow_pairs.len(),
            2,
            "depth=1 must surface ONLY first-hop edges: {shallow_pairs:?}",
        );
        assert!(
            shallow_pairs.contains(&("Mid".to_string(), "Base".to_string())),
            "depth=1 must include Mid -> Base: {shallow_pairs:?}",
        );
        assert!(
            shallow_pairs.contains(&("Leaf".to_string(), "Mid".to_string())),
            "depth=1 must include Leaf -> Mid: {shallow_pairs:?}",
        );
        assert!(
            !shallow_pairs.contains(&("Base".to_string(), "GrandBase".to_string())),
            "depth=1 must NOT include second-hop Base -> GrandBase: {shallow_pairs:?}",
        );
        assert!(
            !shallow_pairs.contains(&("GrandLeaf".to_string(), "Leaf".to_string())),
            "depth=1 must NOT include second-hop GrandLeaf -> Leaf: {shallow_pairs:?}",
        );
    }

    #[test]
    fn diagram_inheritance_widened_kind_filter_trait() {
        // Trait-kind root must resolve (widened filter, same as
        // class_hierarchy).
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/lib.rs",
            Language::Rust,
            vec![sym("MyTrait", SymbolKind::Trait, "/lib.rs")],
            vec![],
        ));
        let result = g.diagram_inheritance("MyTrait", 1, 30);
        assert!(result.is_some(), "Trait root must resolve");
        let r = result.unwrap();
        assert_eq!(r.center, "MyTrait");
        // No inherits edges in the fixture → empty edges, but the
        // result is Some (existence + Some-with-empty is meaningful).
        assert!(r.edges.is_empty());
    }

    // ----- RenderMermaid -----

    #[test]
    fn render_mermaid_empty_edges() {
        let dr = DiagramResult::default();
        assert_eq!(dr.render_mermaid("TD", false), "");
        assert_eq!(dr.render_mermaid("TD", true), "");
    }

    #[test]
    fn render_mermaid_basic() {
        let dr = DiagramResult {
            center: "Foo".to_string(),
            edges: vec![DiagramEdge {
                from: "Foo".to_string(),
                to: "Bar".to_string(),
                label: "calls".to_string(),
            }],
        };
        let out = dr.render_mermaid("", false);
        assert!(out.starts_with("graph TD\n"), "default direction: {out:?}");
        // Both nodes emitted with shortened ids n0/n1.
        assert!(out.contains("    n0[\"Foo\"]\n"), "n0 must be Foo: {out:?}");
        assert!(out.contains("    n1[\"Bar\"]\n"), "n1 must be Bar: {out:?}");
        // Edge with the right label.
        assert!(
            out.contains("    n0 -->|calls| n1\n"),
            "edge with label: {out:?}",
        );
        // No classDef in unstyled mode.
        assert!(
            !out.contains("classDef"),
            "unstyled output must not contain classDef: {out:?}",
        );
    }

    #[test]
    fn render_mermaid_styled_marks_center() {
        let dr = DiagramResult {
            center: "Foo".to_string(),
            edges: vec![DiagramEdge {
                from: "Foo".to_string(),
                to: "Bar".to_string(),
                label: "calls".to_string(),
            }],
        };
        let out = dr.render_mermaid("TD", true);
        assert!(
            out.contains(":::center"),
            "center node must be tagged: {out:?}",
        );
        assert!(
            out.contains("    n0[\"Foo\"]:::center\n"),
            "Foo specifically must carry :::center: {out:?}",
        );
        // Bar is non-center; no tag.
        assert!(
            out.contains("    n1[\"Bar\"]\n"),
            "Bar must NOT carry :::center: {out:?}",
        );
        assert!(
            out.contains("classDef center fill:#f96,stroke:#333\n"),
            "trailer must include the classDef line: {out:?}",
        );
    }

    #[test]
    fn render_mermaid_direction_passthrough() {
        let dr = DiagramResult {
            center: "X".to_string(),
            edges: vec![DiagramEdge {
                from: "X".to_string(),
                to: "Y".to_string(),
                label: "inherits".to_string(),
            }],
        };
        let out = dr.render_mermaid("BT", false);
        assert!(
            out.starts_with("graph BT\n"),
            "BT direction passthrough: {out:?}"
        );
    }

    #[test]
    fn render_mermaid_deterministic() {
        // Build the same DiagramResult twice and assert byte-equal output.
        // Catches regressions if anyone swaps IndexMap for HashMap.
        let make = || DiagramResult {
            center: "root".to_string(),
            edges: vec![
                DiagramEdge {
                    from: "root".to_string(),
                    to: "a".to_string(),
                    label: "calls".to_string(),
                },
                DiagramEdge {
                    from: "root".to_string(),
                    to: "b".to_string(),
                    label: "calls".to_string(),
                },
                DiagramEdge {
                    from: "a".to_string(),
                    to: "c".to_string(),
                    label: "calls".to_string(),
                },
                DiagramEdge {
                    from: "b".to_string(),
                    to: "c".to_string(),
                    label: "calls".to_string(),
                },
            ],
        };
        let first = make().render_mermaid("TD", true);
        let second = make().render_mermaid("TD", true);
        assert_eq!(first, second, "render_mermaid output must be deterministic");
    }

    #[test]
    fn render_mermaid_edge_without_label_uses_plain_arrow() {
        // Empty label falls through to the unlabeled `-->` form. Mirrors
        // the Go reference branch at `diagram.go:289–291`.
        let dr = DiagramResult {
            center: "X".to_string(),
            edges: vec![DiagramEdge {
                from: "X".to_string(),
                to: "Y".to_string(),
                label: String::new(),
            }],
        };
        let out = dr.render_mermaid("TD", false);
        assert!(
            out.contains("    n0 --> n1\n"),
            "unlabeled edge must use `-->` without `|label|`: {out:?}",
        );
        assert!(
            !out.contains("-->|"),
            "unlabeled edge must not emit `|...|`: {out:?}",
        );
    }
}
