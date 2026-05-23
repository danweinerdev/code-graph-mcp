//! [`PathTrie<V>`] — the core map.
//!
//! Generic over both the value type `V` and a [`Normalizer`] policy `N`
//! (default [`IdentityNormalizer`]). Most users write `PathTrie<u32>`
//! and never see `N`; callers that want case-folding or `dunce`-style
//! stripping write `PathTrie<u32, MyNormalizer>`.

use crate::iter::{
    build_frame, extend_path, Iter, IterAncestors, IterSubtree, PathValues, Paths, Values,
};
use crate::node::{ChildMap, EdgeLabel, Node, NodeId};
use crate::normalize::{IdentityNormalizer, Normalizer};
use slotmap::SlotMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Segment-keyed Patricia trie of [`std::path::Path`] → `V`.
///
/// See [crate docs](crate) for the design rationale.
///
/// # Generic parameters
///
/// - `V`: value type. `Clone` is required only to call
///   [`PathTrie::clone`].
/// - `N`: normalization policy (default [`IdentityNormalizer`]). Runs on
///   every key in/out.
///
/// # Iteration order
///
/// Within a single parent, children are visited in lexicographic
/// [`OsStr`] order. The trie does NOT preserve insertion order.
///
/// [`OsStr`]: std::ffi::OsStr
pub struct PathTrie<V, N: Normalizer = IdentityNormalizer> {
    pub(crate) nodes: SlotMap<NodeId, Node<V>>,
    pub(crate) root: NodeId,
    pub(crate) len: usize,
    pub(crate) normalizer: N,
}

impl<V: std::fmt::Debug, N: Normalizer> std::fmt::Debug for PathTrie<V, N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Render as a {path: value} map for parity with HashMap's Debug
        // output — load-bearing for code that does `eprintln!("{:?}",
        // graph)` and pretty-asserts.
        f.debug_map().entries(self.iter()).finish()
    }
}

/// `&PathTrie` → `Iter`, so `for (path, value) in &trie {…}` compiles
/// the same way it does for `&HashMap`. Yields `(PathBuf, &V)` pairs —
/// the path is reconstructed during iteration (per [`Iter`]'s
/// design).
impl<'a, V, N: Normalizer> IntoIterator for &'a PathTrie<V, N> {
    type Item = (std::path::PathBuf, &'a V);
    type IntoIter = Iter<'a, V>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Two tries are equal iff they contain the same `(path, value)` set.
/// Internal node identity (slotmap key, edge-label compression shape)
/// is ignored — two tries built from the same insert sequence but
/// with different intermediate Patricia splits compare equal.
impl<V: PartialEq, N: Normalizer> PartialEq for PathTrie<V, N> {
    fn eq(&self, other: &Self) -> bool {
        if self.len != other.len {
            return false;
        }
        // Walk self; every key must exist in other with the same value.
        for (path, value) in self {
            match other.get(&path) {
                Some(o) if o == value => continue,
                _ => return false,
            }
        }
        true
    }
}

impl<V: Eq, N: Normalizer> Eq for PathTrie<V, N> {}

impl<V> PathTrie<V, IdentityNormalizer> {
    /// New empty trie with the identity normalizer.
    pub fn new() -> Self {
        Self::with_normalizer(IdentityNormalizer)
    }
}

impl<V> Default for PathTrie<V, IdentityNormalizer> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V, N: Normalizer> PathTrie<V, N> {
    /// New empty trie using a custom normalizer.
    pub fn with_normalizer(normalizer: N) -> Self {
        let mut nodes = SlotMap::with_key();
        let root = nodes.insert(Node::root());
        Self {
            nodes,
            root,
            len: 0,
            normalizer,
        }
    }

    // ---- size / membership ----

    /// Number of paths with an associated value (intermediate nodes do
    /// not count).
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Drop every entry and reset the trie to the empty state. The
    /// normalizer policy is preserved.
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.root = self.nodes.insert(Node::root());
        self.len = 0;
    }

    /// `HashMap`-compatibility alias for [`paths`](PathTrie::paths). The
    /// two are byte-identical; this exists so a `HashMap<PathBuf, V>` →
    /// `PathTrie<V>` shape swap doesn't have to rename every call site.
    pub fn keys(&self) -> Paths<'_, V> {
        self.paths()
    }

    pub fn contains_path<P: AsRef<Path>>(&self, path: P) -> bool {
        self.find_terminal(path.as_ref()).is_some()
    }

    // ---- core get / insert / remove ----

    pub fn get<P: AsRef<Path>>(&self, path: P) -> Option<&V> {
        let id = self.find_terminal(path.as_ref())?;
        self.nodes[id].value.as_ref()
    }

    pub fn get_mut<P: AsRef<Path>>(&mut self, path: P) -> Option<&mut V> {
        let id = self.find_terminal(path.as_ref())?;
        self.nodes.get_mut(id)?.value.as_mut()
    }

    /// Insert (or overwrite). Returns the previous value if one was
    /// present at this exact path.
    pub fn insert<P: AsRef<Path>>(&mut self, path: P, value: V) -> Option<V> {
        let segments = self.segments_of(path.as_ref());

        // Empty path → value lives on the root.
        if segments.is_empty() {
            let old = self.nodes[self.root].value.replace(value);
            if old.is_none() {
                self.len += 1;
            }
            return old;
        }

        let mut current_id = self.root;
        let mut idx = 0;

        loop {
            let seg = segments[idx].clone();

            // Look up child by first segment of its edge label.
            let child_id = match self.nodes[current_id].children.get(&seg) {
                Some(&id) => id,
                None => {
                    // No matching child — create leaf carrying all remaining segments.
                    let edge: EdgeLabel = segments[idx..].iter().cloned().collect();
                    let new_node = Node {
                        edge,
                        value: Some(value),
                        children: ChildMap::default(),
                        parent: Some(current_id),
                    };
                    let new_id = self.nodes.insert(new_node);
                    self.nodes[current_id].children.insert(seg, new_id);
                    self.len += 1;
                    return None;
                }
            };

            // Match child.edge against segments[idx..] segment-by-segment.
            // We clone the edge label up-front to release the borrow on self.nodes
            // before any mutation. The clone is bounded by the Patricia compression
            // ratio (typically <= 2 segments).
            let child_edge = self.nodes[child_id].edge.clone();
            let remaining = segments.len() - idx;

            let mut match_len = 0;
            while match_len < child_edge.len()
                && match_len < remaining
                && child_edge[match_len] == segments[idx + match_len]
            {
                match_len += 1;
            }

            if match_len == child_edge.len() {
                // Full edge consumed — descend.
                current_id = child_id;
                idx += match_len;

                if idx == segments.len() {
                    // At the terminal — set value here.
                    let old = self.nodes[current_id].value.replace(value);
                    if old.is_none() {
                        self.len += 1;
                    }
                    return old;
                }
                continue;
            }

            // Partial match — split required.
            //
            //   Before: current → child(edge = [a, b, c, d], …)
            //   Insert path segments[idx..] = [a, b, X, Y]
            //   match_len = 2 (a, b shared)
            //
            //   After:  current → intermediate(edge = [a, b])
            //                       ├── child(edge = [c, d], …)  -- old child, shifted
            //                       └── new_leaf(edge = [X, Y], value = Some)
            //
            // Special case: if segments[idx..] is exactly the common prefix
            // (no remaining new segments), intermediate IS the insertion
            // point — value goes on intermediate, no second child created.
            let common_prefix: EdgeLabel = child_edge[..match_len].iter().cloned().collect();
            let old_child_remaining: EdgeLabel = child_edge[match_len..].iter().cloned().collect();
            let old_child_first = old_child_remaining[0].clone();
            let map_key = common_prefix[0].clone(); // == seg

            // Shift the existing child's edge to the post-split tail.
            self.nodes[child_id].edge = old_child_remaining;

            let intermediate_id = if idx + match_len == segments.len() {
                // Intermediate IS the insertion point.
                let mut intermediate_children = ChildMap::default();
                intermediate_children.insert(old_child_first, child_id);
                let intermediate = Node {
                    edge: common_prefix,
                    value: Some(value),
                    children: intermediate_children,
                    parent: Some(current_id),
                };
                self.nodes.insert(intermediate)
            } else {
                // Intermediate is purely structural; two children.
                let new_leaf_edge: EdgeLabel =
                    segments[idx + match_len..].iter().cloned().collect();
                let new_leaf_first = new_leaf_edge[0].clone();
                let new_leaf = Node {
                    edge: new_leaf_edge,
                    value: Some(value),
                    children: ChildMap::default(),
                    parent: None, // set below once intermediate_id is known
                };
                let new_leaf_id = self.nodes.insert(new_leaf);

                let mut intermediate_children = ChildMap::default();
                intermediate_children.insert(old_child_first, child_id);
                intermediate_children.insert(new_leaf_first, new_leaf_id);
                let intermediate = Node {
                    edge: common_prefix,
                    value: None,
                    children: intermediate_children,
                    parent: Some(current_id),
                };
                let intermediate_id = self.nodes.insert(intermediate);
                self.nodes[new_leaf_id].parent = Some(intermediate_id);
                intermediate_id
            };

            self.nodes[child_id].parent = Some(intermediate_id);
            self.nodes[current_id]
                .children
                .insert(map_key, intermediate_id);
            self.len += 1;
            return None;
        }
    }

    // ---- private helpers ----

    /// Normalize `path` and break it into owned segments.
    pub(crate) fn segments_of(&self, path: &Path) -> Vec<OsString> {
        let normalized = self.normalizer.normalize(path);
        normalized
            .components()
            .map(|c| c.as_os_str().to_os_string())
            .collect()
    }

    /// Walk to the node whose path matches `path` exactly AND holds a
    /// value. Returns `None` for paths that don't exist OR that resolve
    /// to a value-less intermediate node (a directory in the trie sense:
    /// it has children but no associated value of its own).
    pub(crate) fn find_terminal(&self, path: &Path) -> Option<NodeId> {
        let segments = self.segments_of(path);
        self.find_terminal_segments(&segments)
    }

    /// Same as `find_terminal` but takes already-split segments.
    pub(crate) fn find_terminal_segments(&self, segments: &[OsString]) -> Option<NodeId> {
        let id = self.find_node_segments(segments)?;
        if self.nodes[id].value.is_some() {
            Some(id)
        } else {
            None
        }
    }

    /// Walk to the node whose path matches `segments` exactly,
    /// regardless of whether it carries a value. Returns `None` if no
    /// such node exists (i.e. the path falls outside the trie).
    pub(crate) fn find_node_segments(&self, segments: &[OsString]) -> Option<NodeId> {
        if segments.is_empty() {
            return Some(self.root);
        }

        let mut current_id = self.root;
        let mut idx = 0;

        loop {
            let seg = &segments[idx];
            let child_id = self.nodes[current_id].children.get(seg).copied()?;
            let child_edge = &self.nodes[child_id].edge;
            let remaining = segments.len() - idx;

            if child_edge.len() > remaining {
                return None;
            }

            for k in 0..child_edge.len() {
                if child_edge[k] != segments[idx + k] {
                    return None;
                }
            }

            current_id = child_id;
            idx += child_edge.len();

            if idx == segments.len() {
                return Some(current_id);
            }
        }
    }

    /// Remove the value at `path`. Returns the value if present. May
    /// collapse the parent chain if removal leaves an empty branch.
    pub fn remove<P: AsRef<Path>>(&mut self, path: P) -> Option<V> {
        let id = self.find_terminal(path.as_ref())?;
        let value = self.nodes[id]
            .value
            .take()
            .expect("find_terminal guarantees Some");
        self.len -= 1;
        self.collapse_from(id);
        Some(value)
    }

    /// Restore the Patricia invariant after `start_id`'s value was
    /// cleared. Walks parent-ward, removing value-less leaves and
    /// collapsing single-child value-less intermediate nodes into their
    /// (only) child.
    ///
    /// The root is never removed or collapsed: it's the only allowed
    /// value-less single-child node, by design (it anchors the trie).
    fn collapse_from(&mut self, start_id: NodeId) {
        let mut current = start_id;
        loop {
            if current == self.root {
                return;
            }
            let node = &self.nodes[current];
            let has_value = node.value.is_some();
            let child_count = node.children.len();

            if has_value {
                return; // Node still carries a value; nothing to collapse.
            }

            if child_count == 0 {
                // Detach leaf from parent, then recurse on parent.
                let parent = node.parent.expect("non-root has parent");
                let key = node.edge[0].clone();
                self.nodes[parent].children.remove(&key);
                self.nodes.remove(current);
                current = parent;
                continue;
            }

            if child_count == 1 {
                // Collapse: child absorbs our edge, takes our slot in parent.
                let child_id = *node
                    .children
                    .values()
                    .next()
                    .expect("child_count == 1 guarantees one");
                let our_edge = node.edge.clone();
                let map_key = our_edge[0].clone();
                let parent = node.parent.expect("non-root has parent");

                // Prepend our edge to child's edge, retarget child's parent.
                let mut new_edge: EdgeLabel = our_edge;
                for seg in &self.nodes[child_id].edge {
                    new_edge.push(seg.clone());
                }
                self.nodes[child_id].edge = new_edge;
                self.nodes[child_id].parent = Some(parent);

                // Overwrite ourselves in parent's children map with the child.
                self.nodes[parent].children.insert(map_key, child_id);
                self.nodes.remove(current);
                return; // Parent's structure unchanged; no further collapse.
            }

            // child_count >= 2 — node is a legitimate Patricia fork point,
            // stop.
            return;
        }
    }

    pub fn entry<P: AsRef<Path>>(&mut self, path: P) -> Entry<'_, V, N> {
        // Normalize once; both branches need the canonical path form.
        let normalized = self.normalizer.normalize(path.as_ref()).into_owned();
        match self.find_terminal(&normalized) {
            Some(node_id) => Entry::Occupied(OccupiedEntry {
                trie: self,
                node_id,
            }),
            None => Entry::Vacant(VacantEntry {
                trie: self,
                path: normalized,
            }),
        }
    }

    // ---- subtree ops (the differentiator vs flat HashMap) ----

    /// All `(path, &value)` pairs whose path is at or below `prefix`.
    /// Includes the value at `prefix` itself if present.
    pub fn iter_subtree<P: AsRef<Path>>(&self, prefix: P) -> IterSubtree<'_, V> {
        let inner = self.iter_from_prefix(prefix.as_ref());
        IterSubtree { inner }
    }

    /// Apply `f` to every `(path, &mut value)` at or below `prefix`.
    ///
    /// Closure-based instead of iterator-based because yielding
    /// `&mut V` across successive `Iterator::next()` calls cannot be
    /// expressed safely on stable Rust without a `LendingIterator`
    /// or `unsafe` lifetime extension; the workspace lint
    /// `unsafe_code = "forbid"` precludes the latter. Pre-collects
    /// the subtree's `(PathBuf, NodeId)` pairs (one immutable walk),
    /// then iterates them, taking the `&mut V` for each via
    /// [`slotmap::SlotMap::get_mut`].
    pub fn for_each_subtree_mut<P, F>(&mut self, prefix: P, mut f: F)
    where
        P: AsRef<Path>,
        F: FnMut(PathBuf, &mut V),
    {
        let pairs = self.collect_subtree_pairs(prefix.as_ref());
        for (path, id) in pairs {
            if let Some(value) = self.nodes.get_mut(id).and_then(|n| n.value.as_mut()) {
                f(path, value);
            }
        }
    }

    /// Number of stored values at or below `prefix`. O(subtree-size);
    /// cheaper than collecting [`iter_subtree`] because it skips
    /// `PathBuf` materialization.
    ///
    /// [`iter_subtree`]: PathTrie::iter_subtree
    pub fn count_subtree<P: AsRef<Path>>(&self, prefix: P) -> usize {
        let segments = self.segments_of(prefix.as_ref());
        let (root_id, _) = match self.find_subtree_root_segments(&segments) {
            Some(pair) => pair,
            None => return 0,
        };
        let mut count = 0usize;
        let mut stack: Vec<NodeId> = vec![root_id];
        while let Some(id) = stack.pop() {
            let node = &self.nodes[id];
            if node.value.is_some() {
                count += 1;
            }
            stack.extend(node.children.values().copied());
        }
        count
    }

    /// Drop every value at or below `prefix`. Returns the removed
    /// `(path, value)` pairs as a `Vec`.
    ///
    /// v0.1 eager design (returns owned `Vec` rather than a lazy drain
    /// iterator). A streaming variant would need `&mut self` held by
    /// the iterator AND a `Drop` impl to clean partial drains — both
    /// added complexity for no current-use-case win. Revisit if a
    /// caller materializes huge subtrees they only partially consume.
    pub fn remove_subtree<P: AsRef<Path>>(&mut self, prefix: P) -> Vec<(PathBuf, V)> {
        let segments = self.segments_of(prefix.as_ref());

        // Empty prefix → drain the whole trie by swapping the root.
        if segments.is_empty() {
            let new_root = self.nodes.insert(Node::root());
            let old_root = std::mem::replace(&mut self.root, new_root);
            return self.drain_subtree(old_root, PathBuf::new());
        }

        // Walk to find the subtree root + compute its cumulative path.
        let mut current_id = self.root;
        let mut accumulated = PathBuf::new();
        let mut idx = 0;

        let subtree_id = loop {
            if idx == segments.len() {
                break current_id;
            }
            let seg = &segments[idx];
            let child_id = match self.nodes[current_id].children.get(seg).copied() {
                Some(id) => id,
                None => return Vec::new(),
            };
            let child_edge = self.nodes[child_id].edge.clone();
            let remaining = segments.len() - idx;

            if child_edge.len() <= remaining {
                // Child's edge fits within remaining — verify match, then descend.
                for k in 0..child_edge.len() {
                    if child_edge[k] != segments[idx + k] {
                        return Vec::new();
                    }
                }
                for seg in &child_edge {
                    accumulated.push(seg);
                }
                current_id = child_id;
                idx += child_edge.len();
            } else {
                // Prefix ends inside child's edge — verify the prefix matches
                // the head of child's edge, then the whole child's subtree
                // is what we want to drop.
                for k in 0..remaining {
                    if child_edge[k] != segments[idx + k] {
                        return Vec::new();
                    }
                }
                for seg in &child_edge {
                    accumulated.push(seg);
                }
                break child_id;
            }
        };

        // Detach subtree_id from parent.
        let parent = self.nodes[subtree_id]
            .parent
            .expect("non-root subtree has a parent");
        let key = self.nodes[subtree_id].edge[0].clone();
        self.nodes[parent].children.remove(&key);
        self.nodes[subtree_id].parent = None;

        let drained = self.drain_subtree(subtree_id, accumulated);

        // Removing the subtree may leave parent value-less + single-child.
        self.collapse_from(parent);

        drained
    }

    /// Walk and remove every node in the subtree rooted at `start_id`,
    /// freeing the SlotMap slots. Returns the `(path, value)` pairs for
    /// every valued node, in DFS order (lex within siblings).
    fn drain_subtree(&mut self, start_id: NodeId, base_path: PathBuf) -> Vec<(PathBuf, V)> {
        let mut out: Vec<(PathBuf, V)> = Vec::new();
        // Pre-walk to collect node IDs + paths (immutable borrow).
        let pairs = self.collect_subtree_nodes(start_id, base_path);
        // Drain values + free slots (mutable borrow).
        for (path, id) in pairs {
            if let Some(node) = self.nodes.remove(id) {
                if let Some(v) = node.value {
                    self.len -= 1;
                    out.push((path, v));
                }
            }
        }
        out
    }

    /// Collect every node in the subtree rooted at `root_id` with its
    /// cumulative path. Includes value-less intermediates so the caller
    /// can drop them from the SlotMap.
    fn collect_subtree_nodes(&self, root_id: NodeId, base: PathBuf) -> Vec<(PathBuf, NodeId)> {
        let mut out = Vec::new();
        let mut stack: Vec<(NodeId, PathBuf)> = vec![(root_id, base)];
        while let Some((id, path)) = stack.pop() {
            out.push((path.clone(), id));
            let mut kids: Vec<NodeId> = self.nodes[id].children.values().copied().collect();
            kids.sort_by(|&a, &b| self.nodes[b].edge[0].cmp(&self.nodes[a].edge[0]));
            for kid in kids {
                let child_path = extend_path(&self.nodes, &path, kid);
                stack.push((kid, child_path));
            }
        }
        out
    }

    // ---- ancestor / prefix queries ----

    /// The longest stored path that is a prefix of `path`. Useful for
    /// "which indexed root owns this file?".
    pub fn longest_prefix<P: AsRef<Path>>(&self, path: P) -> Option<(PathBuf, &V)> {
        let walk = self.walk_ancestors(path.as_ref());
        walk.into_iter()
            .rev()
            .find_map(|(p, id)| self.nodes[id].value.as_ref().map(|v| (p, v)))
    }

    /// Every stored path that is a prefix of `path`, root-down (shortest
    /// first).
    pub fn iter_ancestors<P: AsRef<Path>>(&self, path: P) -> IterAncestors<'_, V> {
        let walk = self.walk_ancestors(path.as_ref());
        // Filter to valued nodes only (skip intermediate Patricia nodes).
        let queue: Vec<(PathBuf, NodeId)> = walk
            .into_iter()
            .filter(|&(_, id)| self.nodes[id].value.is_some())
            .collect();
        IterAncestors {
            nodes: &self.nodes,
            queue: queue.into_iter(),
        }
    }

    // ---- full iteration ----

    pub fn iter(&self) -> Iter<'_, V> {
        self.iter_from_node(self.root, PathBuf::new())
    }

    pub fn paths(&self) -> Paths<'_, V> {
        Paths { inner: self.iter() }
    }

    pub fn values(&self) -> Values<'_, V> {
        Values { inner: self.iter() }
    }

    pub fn path_values(&self) -> PathValues<'_, V> {
        PathValues { inner: self.iter() }
    }

    // ---- iterator constructors (private) ----

    fn iter_from_node(&self, node_id: NodeId, path: PathBuf) -> Iter<'_, V> {
        let frame = build_frame(&self.nodes, node_id, path);
        let yield_top_value = self.nodes[node_id].value.is_some();
        Iter {
            nodes: &self.nodes,
            stack: vec![frame],
            yield_top_value,
        }
    }

    fn iter_from_prefix(&self, prefix: &Path) -> Iter<'_, V> {
        let segments = self.segments_of(prefix);
        match self.find_subtree_root_segments(&segments) {
            Some((id, full_path)) => self.iter_from_node(id, full_path),
            None => Iter {
                nodes: &self.nodes,
                stack: Vec::new(),
                yield_top_value: false,
            },
        }
    }

    /// Walk to the node anchoring the subtree containing every stored
    /// path that extends `segments`.
    ///
    /// - Exact prefix match → that node + its cumulative path.
    /// - Prefix ends inside a Patricia edge → the child whose edge head
    ///   matches; the child's whole subtree extends the prefix.
    /// - Prefix diverges → `None`.
    ///
    /// `find_node_segments` is the strict-equality cousin (no
    /// inside-edge resolution); useful when the caller demands a node
    /// boundary at the input.
    pub(crate) fn find_subtree_root_segments(
        &self,
        segments: &[OsString],
    ) -> Option<(NodeId, PathBuf)> {
        if segments.is_empty() {
            return Some((self.root, PathBuf::new()));
        }

        let mut current_id = self.root;
        let mut accumulated = PathBuf::new();
        let mut idx = 0;

        loop {
            if idx == segments.len() {
                return Some((current_id, accumulated));
            }
            let seg = &segments[idx];
            let child_id = self.nodes[current_id].children.get(seg).copied()?;
            let child_edge = &self.nodes[child_id].edge;
            let remaining = segments.len() - idx;

            if child_edge.len() <= remaining {
                for k in 0..child_edge.len() {
                    if child_edge[k] != segments[idx + k] {
                        return None;
                    }
                }
                for s in child_edge {
                    accumulated.push(s);
                }
                current_id = child_id;
                idx += child_edge.len();
            } else {
                // Prefix ends inside child's edge — verify the matched head.
                for k in 0..remaining {
                    if child_edge[k] != segments[idx + k] {
                        return None;
                    }
                }
                for s in child_edge {
                    accumulated.push(s);
                }
                return Some((child_id, accumulated));
            }
        }
    }

    /// Collect (path, node_id) pairs for every node at or below `prefix`,
    /// in DFS order. Used by `for_each_subtree_mut` to pre-walk under
    /// an immutable borrow before re-borrowing mutably per-entry.
    pub(crate) fn collect_subtree_pairs(&self, prefix: &Path) -> Vec<(PathBuf, NodeId)> {
        let segments = self.segments_of(prefix);
        let root_id = match self.find_node_segments(&segments) {
            Some(id) => id,
            None => return Vec::new(),
        };
        let mut out = Vec::new();
        let base: PathBuf = segments.iter().collect();
        let mut stack: Vec<(NodeId, PathBuf)> = vec![(root_id, base)];
        while let Some((id, path)) = stack.pop() {
            if self.nodes[id].value.is_some() {
                out.push((path.clone(), id));
            }
            // Push children in reverse-sorted order so DFS pop yields lex order.
            let mut kids: Vec<NodeId> = self.nodes[id].children.values().copied().collect();
            kids.sort_by(|&a, &b| self.nodes[b].edge[0].cmp(&self.nodes[a].edge[0]));
            for kid in kids {
                let child_path = extend_path(&self.nodes, &path, kid);
                stack.push((kid, child_path));
            }
        }
        out
    }

    /// Walk from root toward `path`, yielding `(cumulative_path, node_id)`
    /// for every node visited (root included), in root-down order.
    /// Stops when the next segment doesn't match any child OR when the
    /// remaining path doesn't fully match a child's edge.
    fn walk_ancestors(&self, path: &Path) -> Vec<(PathBuf, NodeId)> {
        let segments = self.segments_of(path);
        let mut out: Vec<(PathBuf, NodeId)> = Vec::new();
        let mut current_id = self.root;
        let mut accumulated = PathBuf::new();
        out.push((accumulated.clone(), current_id));

        let mut idx = 0;
        while idx < segments.len() {
            let seg = &segments[idx];
            let child_id = match self.nodes[current_id].children.get(seg) {
                Some(&id) => id,
                None => break,
            };
            let child_edge = &self.nodes[child_id].edge;
            let remaining = segments.len() - idx;
            if child_edge.len() > remaining {
                break;
            }
            for k in 0..child_edge.len() {
                if child_edge[k] != segments[idx + k] {
                    return out;
                }
            }
            for seg in child_edge {
                accumulated.push(seg);
            }
            current_id = child_id;
            idx += child_edge.len();
            out.push((accumulated.clone(), current_id));
        }
        out
    }
}

// ============================================================================
// Entry API
// ============================================================================

/// `entry(path).or_insert(default)` and friends — same shape as
/// [`std::collections::hash_map::Entry`].
pub enum Entry<'a, V: 'a, N: Normalizer + 'a> {
    Occupied(OccupiedEntry<'a, V, N>),
    Vacant(VacantEntry<'a, V, N>),
}

pub struct OccupiedEntry<'a, V: 'a, N: Normalizer + 'a> {
    trie: &'a mut PathTrie<V, N>,
    node_id: NodeId,
}

pub struct VacantEntry<'a, V: 'a, N: Normalizer + 'a> {
    trie: &'a mut PathTrie<V, N>,
    /// Already-normalized path. We hold it owned so a later
    /// `or_insert*` can insert without re-normalizing.
    path: PathBuf,
}

impl<'a, V, N: Normalizer> Entry<'a, V, N> {
    pub fn or_insert(self, default: V) -> &'a mut V {
        match self {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(default),
        }
    }

    pub fn or_insert_with<F: FnOnce() -> V>(self, default: F) -> &'a mut V {
        match self {
            Entry::Occupied(e) => e.into_mut(),
            Entry::Vacant(e) => e.insert(default()),
        }
    }

    pub fn or_default(self) -> &'a mut V
    where
        V: Default,
    {
        self.or_insert_with(V::default)
    }

    pub fn and_modify<F: FnOnce(&mut V)>(self, f: F) -> Self {
        match self {
            Entry::Occupied(mut e) => {
                f(e.get_mut());
                Entry::Occupied(e)
            }
            other => other,
        }
    }
}

impl<'a, V, N: Normalizer> OccupiedEntry<'a, V, N> {
    pub fn get(&self) -> &V {
        self.trie.nodes[self.node_id]
            .value
            .as_ref()
            .expect("OccupiedEntry node holds Some")
    }

    pub fn get_mut(&mut self) -> &mut V {
        self.trie.nodes[self.node_id]
            .value
            .as_mut()
            .expect("OccupiedEntry node holds Some")
    }

    /// Consumes the entry to extend the value reference's lifetime to
    /// the entry's borrow.
    pub fn into_mut(self) -> &'a mut V {
        self.trie.nodes[self.node_id]
            .value
            .as_mut()
            .expect("OccupiedEntry node holds Some")
    }
}

impl<'a, V, N: Normalizer> VacantEntry<'a, V, N> {
    /// Insert `value`. Returns a mutable reference to it.
    pub fn insert(self, value: V) -> &'a mut V {
        // The trie's `insert` is responsible for any Patricia split;
        // afterward we look up the inserted node to return the
        // long-lifetime `&'a mut V`.
        let VacantEntry { trie, path } = self;
        trie.insert(&path, value);
        trie.get_mut(&path).expect("just inserted")
    }
}

// ============================================================================
// Clone, Debug, PartialEq
// ============================================================================

impl<V: Clone, N: Normalizer + Clone> Clone for PathTrie<V, N> {
    fn clone(&self) -> Self {
        // SlotMap<K, V: Clone> is Clone; nodes carry Clone V.
        Self {
            nodes: self.nodes.clone(),
            root: self.root,
            len: self.len,
            normalizer: self.normalizer.clone(),
        }
    }
}
