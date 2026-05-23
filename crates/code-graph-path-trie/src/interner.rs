//! [`PathInterner`] — assign stable `u32` ids to paths.
//!
//! Backed by [`PathTrie<PathId>`]; layered on top so the core trie type
//! stays unaware of interning. Pull this in when you need to replace
//! `PathBuf` keys in some other data structure with a compact handle
//! (typical use: graph caches that repeat the same paths hundreds of
//! thousands of times — the wire / disk savings are the whole reason
//! this crate exists for the code-graph-mcp use case).
//!
//! `PathId` is a [`NonZeroU32`] so `Option<PathId>` is also 4 bytes.
//! Reserved value `0` = "no id" — the interner never assigns it.

use crate::normalize::{IdentityNormalizer, Normalizer};
use crate::trie::PathTrie;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

/// Stable, dense, monotonically-assigned handle for an interned path.
///
/// Ids are NOT recycled on `remove` — gaps in the id space are
/// permitted. If you compact, allocate a fresh interner and re-intern.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PathId(NonZeroU32);

impl PathId {
    #[inline]
    pub fn get(self) -> u32 {
        self.0.get()
    }

    /// For deserialization from a serialized id space. Panics if zero
    /// (zero is reserved).
    pub fn from_raw(v: u32) -> Self {
        Self(NonZeroU32::new(v).expect("PathId 0 is reserved"))
    }
}

pub struct PathInterner<N: Normalizer = IdentityNormalizer> {
    /// path → id (forward lookup).
    forward: PathTrie<PathId, N>,
    /// id → owned path (reverse lookup; index = `PathId.get() - 1`).
    reverse: Vec<PathBuf>,
}

impl PathInterner<IdentityNormalizer> {
    pub fn new() -> Self {
        Self::with_normalizer(IdentityNormalizer)
    }
}

impl Default for PathInterner<IdentityNormalizer> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: Normalizer> PathInterner<N> {
    pub fn with_normalizer(normalizer: N) -> Self {
        Self {
            forward: PathTrie::with_normalizer(normalizer),
            reverse: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.reverse.len()
    }

    pub fn is_empty(&self) -> bool {
        self.reverse.is_empty()
    }

    /// Get the id for `path`, allocating one if first-seen.
    pub fn intern<P: AsRef<Path>>(&mut self, path: P) -> PathId {
        let path = path.as_ref();
        if let Some(&id) = self.forward.get(path) {
            return id;
        }
        // Allocate next id (1-indexed; 0 is the reserved sentinel).
        let next =
            u32::try_from(self.reverse.len() + 1).expect("PathInterner exceeded u32::MAX entries");
        let id = PathId(NonZeroU32::new(next).expect("next is nonzero"));
        self.reverse.push(path.to_path_buf());
        self.forward.insert(path, id);
        id
    }

    /// Lookup without allocation. Returns `None` if not interned.
    pub fn get<P: AsRef<Path>>(&self, path: P) -> Option<PathId> {
        self.forward.get(path).copied()
    }

    /// Reverse lookup: id → path. Returns `None` if `id` is out of
    /// range (e.g. a serialized id from a different interner).
    pub fn resolve(&self, id: PathId) -> Option<&Path> {
        let idx = id.get() as usize - 1;
        self.reverse.get(idx).map(PathBuf::as_path)
    }
}
