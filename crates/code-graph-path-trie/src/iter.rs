//! Iterator types returned by [`PathTrie`].
//!
//! All iterators are *borrowing* — they tie to the trie's lifetime and
//! yield `(PathBuf, &V)` or `(PathBuf, &mut V)` pairs. `PathBuf` is
//! reconstructed by walking the Patricia edges from root to each leaf
//! during yield; per-yield allocation is the practical compromise vs
//! `LendingIterator` ergonomics.
//!
//! [`PathTrie`]: crate::PathTrie

use crate::node::{Node, NodeId};
use slotmap::SlotMap;
use std::path::{Path, PathBuf};

// ============================================================================
// Shared DFS state
// ============================================================================

/// One frame on the DFS stack.
pub(crate) struct DfsFrame {
    pub(crate) node_id: NodeId,
    /// Cumulative path to `node_id`, including its own edge segments.
    pub(crate) path: PathBuf,
    /// Children IDs, sorted reverse-lex by their first edge segment so
    /// `.pop()` yields them lex-ascending.
    pub(crate) pending_children: Vec<NodeId>,
}

/// Build a frame for the given node, pre-sorting its children.
pub(crate) fn build_frame<V>(
    nodes: &SlotMap<NodeId, Node<V>>,
    node_id: NodeId,
    path: PathBuf,
) -> DfsFrame {
    let mut children: Vec<NodeId> = nodes[node_id].children.values().copied().collect();
    children.sort_by(|&a, &b| nodes[b].edge[0].cmp(&nodes[a].edge[0]));
    DfsFrame {
        node_id,
        path,
        pending_children: children,
    }
}

/// Append a child's edge segments to a base path, producing the child's
/// cumulative path.
pub(crate) fn extend_path<V>(
    nodes: &SlotMap<NodeId, Node<V>>,
    base: &Path,
    child_id: NodeId,
) -> PathBuf {
    let mut out = base.to_path_buf();
    for seg in &nodes[child_id].edge {
        out.push(seg);
    }
    out
}

// ============================================================================
// Iter — full / subtree DFS, yielding (PathBuf, &V)
// ============================================================================

/// DFS over the whole trie, lex-sorted within siblings.
pub struct Iter<'a, V: 'a> {
    pub(crate) nodes: &'a SlotMap<NodeId, Node<V>>,
    pub(crate) stack: Vec<DfsFrame>,
    /// `true` after pushing a frame whose node has `Some(value)` — the
    /// next `next()` call yields it before descending into children.
    pub(crate) yield_top_value: bool,
}

impl<'a, V> Iterator for Iter<'a, V> {
    type Item = (PathBuf, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.yield_top_value {
                self.yield_top_value = false;
                let frame = self.stack.last().expect("yield_top_value implies a frame");
                // SAFETY-equivalent reasoning: the value reference is
                // tied to the lifetime of `self.nodes` (`'a`), not to
                // the mutable iterator borrow. SlotMap indexing returns
                // a `&Node<V>` with lifetime tied to the SlotMap borrow,
                // which is `'a` here.
                let value = self.nodes[frame.node_id]
                    .value
                    .as_ref()
                    .expect("yield_top_value implies Some");
                return Some((frame.path.clone(), value));
            }

            let frame = self.stack.last_mut()?;
            match frame.pending_children.pop() {
                Some(child_id) => {
                    let child_path = extend_path(self.nodes, &frame.path, child_id);
                    let child_frame = build_frame(self.nodes, child_id, child_path);
                    self.yield_top_value = self.nodes[child_id].value.is_some();
                    self.stack.push(child_frame);
                }
                None => {
                    self.stack.pop();
                }
            }
        }
    }
}

/// DFS limited to the subtree rooted at a given prefix. Includes the
/// value at the prefix itself if one is stored there.
///
/// Internally identical to [`Iter`] but exposed as a distinct type so
/// future implementations can specialize (e.g. byte-budget early stop).
pub struct IterSubtree<'a, V: 'a> {
    pub(crate) inner: Iter<'a, V>,
}

impl<'a, V> Iterator for IterSubtree<'a, V> {
    type Item = (PathBuf, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }
}

// ============================================================================
// IterAncestors — root-down walk of every valued ancestor on a path
// ============================================================================

/// Yields valued ancestors of a path, root-down (shortest first).
pub struct IterAncestors<'a, V: 'a> {
    pub(crate) nodes: &'a SlotMap<NodeId, Node<V>>,
    /// Pre-collected `(path, node_id)` pairs for valued nodes on the
    /// walk, in root-down order. Built once at construction.
    pub(crate) queue: std::vec::IntoIter<(PathBuf, NodeId)>,
}

impl<'a, V> Iterator for IterAncestors<'a, V> {
    type Item = (PathBuf, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        let (path, id) = self.queue.next()?;
        let v = self.nodes[id].value.as_ref()?;
        Some((path, v))
    }
}

// ============================================================================
// Paths / Values / PathValues — projections of Iter
// ============================================================================

/// Just the keys (paths) of the trie. Same order as [`Iter`].
pub struct Paths<'a, V: 'a> {
    pub(crate) inner: Iter<'a, V>,
}

impl<V> Iterator for Paths<'_, V> {
    type Item = PathBuf;
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(p, _)| p)
    }
}

/// Just the values of the trie. Same order as [`Iter`].
pub struct Values<'a, V: 'a> {
    pub(crate) inner: Iter<'a, V>,
}

impl<'a, V> Iterator for Values<'a, V> {
    type Item = &'a V;
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(|(_, v)| v)
    }
}

/// Like [`Iter`] but kept as a distinct type for API symmetry with
/// [`PathTrie::path_values`].
///
/// [`PathTrie::path_values`]: crate::PathTrie::path_values
pub struct PathValues<'a, V: 'a> {
    pub(crate) inner: Iter<'a, V>,
}

impl<'a, V> Iterator for PathValues<'a, V> {
    type Item = (PathBuf, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next()
    }
}
