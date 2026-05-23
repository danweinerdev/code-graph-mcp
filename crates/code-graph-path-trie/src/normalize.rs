//! Path normalization policy hook.
//!
//! [`PathTrie`] stores whatever you give it; if `/Foo/Bar.txt` and
//! `/foo/bar.txt` should be the *same* key, plug in a [`Normalizer`]
//! that case-folds segments. The hook runs once at every insertion and
//! every query, so the cost is on the boundary, not on every node.
//!
//! Built-in: [`IdentityNormalizer`] (the default — leaves paths alone).
//!
//! Callers wanting Windows-style case-insensitive lookups, dunce-style
//! `\\?\` stripping, or `..` collapse implement [`Normalizer`]
//! themselves; the trie has no opinion.
//!
//! [`PathTrie`]: crate::PathTrie

use std::borrow::Cow;
use std::path::Path;

/// Policy for transforming user-supplied paths before they enter the
/// trie's key space.
///
/// `normalize` runs on every insert and every query. Returning
/// `Cow::Borrowed(p)` avoids the allocation in the identity case.
pub trait Normalizer {
    fn normalize<'a>(&self, path: &'a Path) -> Cow<'a, Path>;
}

/// No-op normalizer. The default for [`PathTrie`].
///
/// [`PathTrie`]: crate::PathTrie
#[derive(Debug, Default, Clone, Copy)]
pub struct IdentityNormalizer;

impl Normalizer for IdentityNormalizer {
    fn normalize<'a>(&self, path: &'a Path) -> Cow<'a, Path> {
        Cow::Borrowed(path)
    }
}
