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
//!
//! # Production wiring status in `code-graph-mcp`
//!
//! This crate was built as part of the PackedCache / TrieKeyedGraph
//! effort in `code-graph-mcp`. Not every feature ended up with a
//! production caller. The matrix below records the actual wiring
//! state so future contributors don't waste cycles wondering "why is
//! half this API surface unused?" — the answer is usually that the
//! workspace doesn't have the access pattern those features need.
//!
//! Wired (has at least one production caller):
//!
//! | Feature | Production site(s) |
//! |---|---|
//! | [`PathInterner`] (intern/get/resolve) | `code-graph-graph::persist::packed::encode` — cache writer interns every path once |
//! | [`PathTrie<V>`] as storage shape | `Graph.files` and `Graph.includes` (Phase E shape swap) |
//! | [`PathTrie::iter_subtree`] | `Graph::orphans_under` (subtree-scoped `get_orphans`), `Graph::search` (subtree-scoped `search_symbols` via pre-built file set), `Graph::drop_files_in_scope` and `Graph::evict_missing_in_scope` (scoped-analyze cache-hygiene paths), `detect_cycles` subtree post-filter |
//! | [`PathTrie::longest_prefix`] | `code_graph_lang_rust::crate_model` (RCMM owning-crate lookup), `code_graph_lang_go::module_model` (GMM owning-module lookup) |
//! | [`PathTrie::remove_subtree`] | `Graph::remove_files_under` → watch handler directory-remove path |
//!
//! **Intentionally dormant** in this workspace — the API exists and
//! is tested, but no current production code path benefits:
//!
//! - [`PathTrie::iter_ancestors`]. The natural fit is "walk every
//!   indexed parent that owns this file" — but
//!   [`crate_model`](https://docs.rs/code-graph-lang-rust)'s RCMM and
//!   [`module_model`](https://docs.rs/code-graph-lang-go)'s GMM only
//!   store the *single* deepest-nesting owning crate/module per
//!   file. Using `iter_ancestors` would require restructuring those
//!   models to keep the whole ancestor chain — a feature addition,
//!   not a wire-up. Available for downstream uses (e.g. a
//!   diagnostic tool that explains "which Cargo.tomls were
//!   considered for this file's namespace derivation").
//!
//! - [`PathTrie::for_each_subtree_mut`]. No current code path
//!   bulk-mutates `FileEntry` values under a directory prefix.
//!   The watch handler's nearest pattern is REMOVAL, not mutation,
//!   already served by `remove_subtree`. Available for future
//!   bulk-touch operations (e.g. mark-stale tagging, lazy namespace
//!   precompute) if they materialize.
//!
//! - [`PathSet`]. The only candidate `HashSet<PathBuf>` sites in the
//!   workspace (Tarjan SCC's `all_files`/`on_stack`, `diagrams.rs`'s
//!   BFS `visited`) use the sets purely for membership checks — none
//!   would actually exercise the trie's subtree or ancestor queries,
//!   so a swap would add Patricia/slotmap overhead for zero
//!   functional benefit. `PathSet` stays exported and tested for
//!   contexts where prefix queries on a path-only set actually
//!   matter.
//!
//! - [`Normalizer`] (non-default). Every `PathTrie` instance in
//!   `code-graph-mcp` uses the default [`IdentityNormalizer`]; no
//!   `with_normalizer` call exists in production code. Plugging in
//!   a non-identity normalizer would change observable semantics
//!   (case-folding for Windows lookups is an explicit non-goal per
//!   `CLAUDE.md` "Known cross-cutting limitations"). Plugging in
//!   `dunce::canonicalize` would re-do filesystem work the index
//!   pass already did. The trait is the right shape for the case
//!   when normalization SHOULD be policy-driven; this workspace
//!   just doesn't have that case yet.
//!
//! If you're adding a new tool that wants any of the above features,
//! the wire-up is straightforward — the relevant types are already
//! `pub`. Update this matrix when you do.

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
