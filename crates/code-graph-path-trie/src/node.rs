//! Internal node + key type definitions.
//!
//! `NodeId` is a [`slotmap`] key (generational `u32` + version), so it's
//! stable across mutations of unrelated nodes and rejects stale handles
//! after the node is removed. Public API never exposes `NodeId`.

use rustc_hash::FxHashMap;
use slotmap::new_key_type;
use smallvec::SmallVec;
use std::ffi::OsString;

new_key_type! {
    /// Internal node handle. Not part of the public API.
    pub(crate) struct NodeId;
}

/// Edge label between a node and its parent.
///
/// With Patricia compression a chain of single-child segments collapses
/// into one edge labeled with `SmallVec<[OsString; 2]>`. The inline
/// capacity (2) covers the common path-fork shape (`crates/foo/src/...`
/// → `crates/foo` collapses; the split happens at `src`). Allocates on
/// chains longer than 2; sized to avoid the alloc in the hot case.
pub(crate) type EdgeLabel = SmallVec<[OsString; 2]>;

/// Child map keyed by the first segment of the child's [`EdgeLabel`].
///
/// `FxHashMap` (non-cryptographic, deterministic) chosen for predictable
/// throughput on small / medium fanout. Most directories have <100
/// children; `FxHashMap` is faster than `HashMap` here and faster than
/// `BTreeMap` until you need ordered iteration (we sort at iter time
/// instead).
pub(crate) type ChildMap = FxHashMap<OsString, NodeId>;

/// One trie node. Internal — public API addresses nodes by path.
#[derive(Clone)]
pub(crate) struct Node<V> {
    /// Edge label from parent. Empty for the root.
    pub(crate) edge: EdgeLabel,
    /// Value attached to the path ending at this node. `None` for
    /// intermediate nodes that exist only to host children.
    pub(crate) value: Option<V>,
    /// Outgoing edges keyed by first segment.
    pub(crate) children: ChildMap,
    /// Back-pointer for ancestor walks. `None` only for the root.
    pub(crate) parent: Option<NodeId>,
}

impl<V> Node<V> {
    pub(crate) fn root() -> Self {
        Self {
            edge: EdgeLabel::new(),
            value: None,
            children: ChildMap::default(),
            parent: None,
        }
    }
}
