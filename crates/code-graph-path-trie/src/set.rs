//! [`PathSet`] — newtype wrapper around `PathTrie<()>`.
//!
//! Identical shape to [`std::collections::HashSet`] vs `HashMap`. Use
//! when you only need membership / prefix queries, not values.

use crate::iter::IterSubtree;
use crate::normalize::{IdentityNormalizer, Normalizer};
use crate::trie::PathTrie;
use std::path::{Path, PathBuf};

/// Set of paths with subtree + ancestor queries.
pub struct PathSet<N: Normalizer = IdentityNormalizer> {
    inner: PathTrie<(), N>,
}

impl PathSet<IdentityNormalizer> {
    pub fn new() -> Self {
        Self {
            inner: PathTrie::new(),
        }
    }
}

impl Default for PathSet<IdentityNormalizer> {
    fn default() -> Self {
        Self::new()
    }
}

impl<N: Normalizer> PathSet<N> {
    pub fn with_normalizer(normalizer: N) -> Self {
        Self {
            inner: PathTrie::with_normalizer(normalizer),
        }
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    pub fn insert<P: AsRef<Path>>(&mut self, path: P) -> bool {
        self.inner.insert(path, ()).is_none()
    }

    pub fn remove<P: AsRef<Path>>(&mut self, path: P) -> bool {
        self.inner.remove(path).is_some()
    }

    pub fn contains<P: AsRef<Path>>(&self, path: P) -> bool {
        self.inner.contains_path(path)
    }

    pub fn iter_subtree<P: AsRef<Path>>(&self, prefix: P) -> IterSubtree<'_, ()> {
        self.inner.iter_subtree(prefix)
    }

    pub fn count_subtree<P: AsRef<Path>>(&self, prefix: P) -> usize {
        self.inner.count_subtree(prefix)
    }

    pub fn longest_prefix<P: AsRef<Path>>(&self, path: P) -> Option<PathBuf> {
        self.inner.longest_prefix(path).map(|(p, _)| p)
    }
}
