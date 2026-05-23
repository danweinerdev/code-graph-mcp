#![forbid(unsafe_code)]

//! Segment-keyed Patricia path trie.
//!
//! `PathTrie<V>` stores values keyed by [`std::path::Path`]. Internally,
//! keys are split into [`OsString`] segments via [`Path::components`] and
//! laid out as a trie where each edge labels one (or, with Patricia
//! compression, a run of) segments. The shape directly mirrors filesystem
//! semantics: `/foo/bar` and `/foo/bart` share `/foo/` and nothing more,
//! so `iter_subtree("/foo/bar")` cleanly excludes `bart`.
//!
//! # Highlights
//!
//! - **O(depth)** insert / get / remove (depth = path component count;
//!   Patricia collapse cuts most chains to 1).
//! - **Subtree operations** as a first-class primitive:
//!   [`PathTrie::iter_subtree`], [`PathTrie::remove_subtree`],
//!   [`PathTrie::count_subtree`].
//! - **Ancestor queries**: [`PathTrie::longest_prefix`],
//!   [`PathTrie::iter_ancestors`].
//! - **UTF-8 and non-UTF-8 paths both supported losslessly** — segments
//!   are stored as [`OsString`], not [`String`].
//! - **No `unsafe`** — workspace forbids it.
//!
//! # Not in scope
//!
//! - Filesystem traversal (`walkdir`/`ignore` already do this well).
//! - Symlink / `..` resolution (callers must pre-canonicalize).
//! - Case-folding policy (use the [`Normalizer`] hook for that).
//!
//! [`Path::components`]: std::path::Path::components

#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod interner;
pub mod iter;
pub mod node;
pub mod normalize;
pub mod set;
pub mod trie;

pub use interner::{PathId, PathInterner};
pub use iter::{Iter, IterAncestors, IterSubtree, PathValues, Paths, Values};
pub use normalize::{IdentityNormalizer, Normalizer};
pub use set::PathSet;
pub use trie::{Entry, OccupiedEntry, PathTrie, VacantEntry};
