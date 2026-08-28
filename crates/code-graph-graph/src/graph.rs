//! Graph storage and the merge / remove / clear mutators.
//!
//! This module mirrors the Go reference at `internal/graph/graph.go` lines
//! 1–175 (`Graph`, `Node`, `EdgeEntry`, `New`, `MergeFileGraph`, `RemoveFile`,
//! `removeFileUnsafe`, `Clear`). The Rust port adds a [`FileEntry`] that
//! records the source [`Language`] alongside the file's symbol IDs so the
//! cache v2 format can persist the language without re-deriving it from the
//! file extension. Locking is **not** introduced here — the server-side
//! [`Graph`] is wrapped behind a `parking_lot::RwLock` at the call site.
//!
//! Keys for the file-scoped maps (`files`, `includes`) are `PathBuf` rather
//! than `String` so callers do not have to launder `Path` ↔ `String`
//! conversions at every boundary. Symbol IDs remain `String` (aliased as
//! [`SymbolId`] in `code-graph-core`) because they are arbitrary identifiers,
//! not filesystem paths.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use code_graph_core::{
    symbol_id, Confidence, EdgeKind, FileGraph, Language, ResolverMetadata, Symbol, SymbolId,
};
use serde::{Deserialize, Serialize};

/// In-memory directed graph of code symbols.
///
/// Storage layout matches the Go reference exactly:
/// - `nodes`: symbol id → [`Node`]
/// - `adj` / `radj`: symbol id → outgoing / incoming `Calls` and `Inherits`
///   edges (the Go binary keeps both directions for cheap callers/callees)
/// - `files`: file path → [`FileEntry`] (language + owned symbol IDs)
/// - `includes`: file path → [`IncludeEntry`] list (included file path +
///   source line; kept separate from `adj`/`radj` because include edges
///   are file-to-file, not symbol-to-symbol)
#[derive(Debug, Default)]
pub struct Graph {
    pub(crate) nodes: HashMap<SymbolId, Node>,
    pub(crate) adj: HashMap<SymbolId, Vec<EdgeEntry>>,
    pub(crate) radj: HashMap<SymbolId, Vec<EdgeEntry>>,
    /// Per-file metadata, keyed by absolute file path. Backed by a
    /// segment-keyed Patricia trie (Phase E) so subtree-scoped tools
    /// can walk all files under a directory prefix in `O(subtree)`
    /// instead of scanning the full map. The
    /// `HashMap`-shape-compatible methods (`get`, `insert`, `remove`,
    /// `contains_path`, `len`, `is_empty`, `clear`, `keys`, `values`,
    /// `iter`, `entry`) are direct drop-in equivalents to `HashMap`'s
    /// (note `contains_path` vs the old `contains_key`); subtree
    /// methods (`iter_subtree`, `count_subtree`, `remove_subtree`,
    /// `longest_prefix`, `iter_ancestors`) are the new capability
    /// surface unlocked by Phase E.
    pub(crate) files: code_graph_path_trie::PathTrie<FileEntry>,
    /// Per-file include lists. Same trie shape and rationale as
    /// `files`.
    pub(crate) includes: code_graph_path_trie::PathTrie<Vec<IncludeEntry>>,
    /// Sparse language-specific resolver facts. Kept out of [`FileEntry`] and
    /// [`FileGraph`] so files that do not need them pay no in-memory or cache
    /// payload cost.
    pub(crate) resolver_metadata: code_graph_path_trie::PathTrie<ResolverMetadata>,
    /// Per-file list of `adj` keys that hold that file's edges but are
    /// **not** recoverable from its [`FileEntry::symbol_ids`] or its
    /// file-path pseudo-key.
    ///
    /// Why it is needed: `Calls`/`Overrides` edges are keyed by the
    /// originating *symbol id* (`file:name`), and file-scope
    /// lambda/closure calls are keyed by the file path itself — both
    /// reachable from `files[path]` when the file is removed.
    /// `Inherits` edges are not: their `from` is the derived **type
    /// name** verbatim (`Derived`, and the generics-carrying forms
    /// `Foo<T>` / `Vec<T>` — see the "generic verbatim in
    /// `Inherits.from`" per-language notes in CLAUDE.md), which is
    /// neither a symbol id nor derivable from one. Recording that
    /// residual per file is what lets
    /// [`Graph::remove_edges_from_file`] cost `O(edges-of-file)`
    /// instead of `O(all edges in the graph)`.
    ///
    /// Deliberately **not** persisted: the cache stores `adj`/`radj`
    /// verbatim, so [`Graph::rebuild_adj_extra_keys`] reconstructs this
    /// index in one linear pass on load, where persisting it would cost
    /// a `CACHE_VERSION` bump (and a full re-index of every existing
    /// cache) for data that is already derivable.
    ///
    /// The index is superset-tolerant by design: a listed key that is
    /// absent from `adj` costs one failed hash lookup, whereas a
    /// *missing* key would leak edges — so both the merge-time record
    /// and the load-time rebuild err toward listing too much.
    pub(crate) adj_extra_keys: HashMap<PathBuf, Vec<SymbolId>>,
    /// Nanoseconds since UNIX_EPOCH when the out-of-scope hygiene
    /// sweep last ran across this graph. Persisted in the cache
    /// (`GraphCache.last_sweep_at`) so the cadence survives process
    /// restarts. `0` means "never swept" — true for a freshly-built
    /// graph that has not yet had a sweep run against it. Updated by
    /// [`Graph::set_last_sweep_at`] from the `analyze_codebase`
    /// handler immediately after a sweep, before the save.
    pub(crate) last_sweep_at: u64,
}

/// Wrapper around a [`Symbol`] stored in the graph. Mirrors Go's `Node`.
///
/// `Serialize`/`Deserialize` are derived so callers can round-trip a `Node`
/// directly when convenient. Cache v2 (`persist.rs`) does not use them — it
/// flattens to `HashMap<SymbolId, Symbol>` to match the Go cache shape — but
/// other persistence layers may.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub symbol: Symbol,
}

/// One directed edge in either the forward (`adj`) or reverse (`radj`)
/// adjacency list. Mirrors Go's `EdgeEntry`. The `target` is the *other end*
/// of the edge from the map key's perspective: in `adj[from]` it is the
/// destination; in `radj[to]` it is the origin.
///
/// `PathBuf`'s default serde impl serializes as a string on Unix (and a
/// best-effort string on Windows), which is what cache v2 expects.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EdgeEntry {
    pub target: SymbolId,
    pub kind: EdgeKind,
    pub file: PathBuf,
    pub line: u32,
    /// Resolver confidence in `target`. Copied verbatim from the source
    /// [`code_graph_core::Edge::confidence`] at merge time. Defaults to
    /// [`Confidence::Resolved`] for older cached entries written before
    /// the field existed; in practice the CACHE_VERSION bump that
    /// added this field also triggers a silent re-index, so the default
    /// is rarely exercised in production.
    #[serde(default)]
    pub confidence: Confidence,
    /// How many same-named candidates competed for `target` (FR-48,
    /// D-0007). Copied verbatim from
    /// [`code_graph_core::Edge::candidates`] at merge time: `1` =
    /// sole candidate or declarative (NOT necessarily verified — a
    /// receiver-typed sole-candidate pick is `Heuristic/1`, F2),
    /// `N ≥ 2` = scope-rule pick among N. The
    /// serde default (1) exists for hand-written fixtures; cache-format
    /// safety comes from the v11 CACHE_VERSION bump — a pre-bump cache
    /// re-indexes rather than being read with a guessed count.
    #[serde(default = "default_candidate_count")]
    pub candidates: u32,
}

/// Serde default for [`EdgeEntry::candidates`]: the sole-candidate count
/// (which does not imply `Resolved` — a receiver-typed sole-candidate
/// pick is `Heuristic/1`, F2).
fn default_candidate_count() -> u32 {
    1
}

/// Per-file metadata recorded at merge time. The Go reference stores only
/// `map[string][]string` (path → symbol IDs); the Rust port also captures
/// the source [`Language`] so the cache v2 format can persist it without
/// re-deriving from the path extension.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub language: Language,
    pub symbol_ids: Vec<SymbolId>,
}

/// One entry in a file's include list: the included file path plus the
/// source line of the `#include`-style directive that produced it.
///
/// The include map previously stored bare `PathBuf`s, discarding the line.
/// Carrying the line lets the dependency query report *where* in the
/// source each include was declared instead of just *that* it exists.
/// `path`/`line` mirror the wire-format field names directly (no
/// `rename_all` needed); serde derives match the sibling cached structs so
/// the on-disk cache shape stays a single-deserializer-compatible JSON.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IncludeEntry {
    pub path: PathBuf,
    pub line: u32,
}

/// Quick storage-size summary returned by [`Graph::stats`]. The `edges`
/// count includes both adjacency entries (calls + inherits) and include
/// edges, matching the Go binary's `Stats()` semantics.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GraphStats {
    pub nodes: u32,
    pub edges: u32,
    pub files: u32,
}

impl Graph {
    /// Construct an empty graph with all maps initialized. Never panics and
    /// never allocates beyond `HashMap`'s zero-capacity default.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `id` resolves to a node currently stored in this graph.
    ///
    /// The "resolved" predicate shared by call-graph BFS and diagram
    /// edge rendering: an edge target string that fails this check is a
    /// bare callee token the parser captured but the call resolver could
    /// not bind to a definition (external function, macro identifier,
    /// stdlib call like `Ok`/`Err`/`printf`/`to_string`, etc.). Such
    /// tokens must NOT enter call-graph BFS `visited` sets — their
    /// presence would distort depth attribution for resolved neighbors at
    /// depth >= 2 by short-circuiting later legitimate visits via false
    /// `visited` membership — and must not render as path-basename
    /// pseudo-nodes in diagrams. Both [`Graph::bfs`] (used by
    /// `callers`/`callees`) and `Graph::diagram_call_graph`'s BFS
    /// expansion pivot on this exact `nodes.contains_key` check before
    /// inserting into `visited`, so the two tools stay behaviorally
    /// consistent on what counts as a "real" callee. The diagram path
    /// additionally uses the same predicate inside `diagrams::mermaid_label`
    /// as a post-BFS defense-in-depth filter for `raw_edges` entries
    /// that bypassed the expansion-time guard (truncation tail).
    pub(crate) fn is_resolved_node(&self, id: &str) -> bool {
        self.nodes.contains_key(id)
    }

    /// Add or replace all symbols and edges from a parsed [`FileGraph`].
    ///
    /// If the path is already known, its previous contents are removed first
    /// (via `remove_file_unsafe`) so re-merging is naturally
    /// idempotent — the post-state depends only on the input `fg`, not on
    /// whether the path was previously merged.
    ///
    /// Edges are routed by kind:
    /// - `Calls` and `Inherits` → both `adj[from]` and `radj[to]`
    /// - `Includes` → `includes[from]` only (file-to-file, not symbol-to-symbol)
    pub fn merge_file_graph(&mut self, fg: FileGraph) {
        let path = PathBuf::from(&fg.path);

        // Remove stale data if file was previously indexed.
        if self.files.contains_path(&path) {
            self.remove_file_unsafe(&path);
        }
        // Metadata describes the previous parse of this path. A caller that
        // has fresh parser metadata must set it after merging the new graph.
        self.resolver_metadata.remove(&path);

        // Add symbols as nodes.
        let mut symbol_ids: Vec<SymbolId> = Vec::with_capacity(fg.symbols.len());
        for symbol in fg.symbols {
            let id = symbol_id(&symbol);
            self.nodes.insert(id.clone(), Node { symbol });
            symbol_ids.push(id);
        }
        // Adjacency keys this file's edges land under that
        // `remove_edges_from_file` can rediscover on its own: the file's
        // own symbol ids (ordinary `Calls`/`Overrides` sources) plus the
        // file-path pseudo-key (file-scope lambda / package-level
        // closure calls). Anything else — in practice an `Inherits`
        // edge keyed by the derived type name — is recorded in
        // `adj_extra_keys` below so the removal path can reach it
        // without scanning every edge in the graph.
        let derivable_keys: HashSet<&str> = symbol_ids.iter().map(String::as_str).collect();
        let path_key = path.to_string_lossy().into_owned();
        let mut extra_keys: Vec<SymbolId> = Vec::new();

        // Add edges, routing by kind. Confidence rides into both adj and
        // radj entries so the per-tool `min_confidence` filter can apply
        // uniformly regardless of which direction the query walks.
        // Include edges do not carry confidence today because the
        // include-target path is the resolver's verbatim output, not a
        // pick-one-of-many — multi-candidate include resolution still
        // returns a single path via the suffix-disambiguation rule.
        for edge in fg.edges {
            let edge_file = PathBuf::from(&edge.file);
            match edge.kind {
                EdgeKind::Calls | EdgeKind::Inherits | EdgeKind::Overrides => {
                    if edge.from != path_key && !derivable_keys.contains(edge.from.as_str()) {
                        extra_keys.push(edge.from.clone());
                    }
                    self.adj
                        .entry(edge.from.clone())
                        .or_default()
                        .push(EdgeEntry {
                            target: edge.to.clone(),
                            kind: edge.kind,
                            file: edge_file.clone(),
                            line: edge.line,
                            confidence: edge.confidence,
                            candidates: edge.candidates,
                        });
                    self.radj.entry(edge.to).or_default().push(EdgeEntry {
                        target: edge.from,
                        kind: edge.kind,
                        file: edge_file,
                        line: edge.line,
                        confidence: edge.confidence,
                        candidates: edge.candidates,
                    });
                }
                EdgeKind::Includes => {
                    self.includes
                        .entry(PathBuf::from(&edge.from))
                        .or_default()
                        .push(IncludeEntry {
                            path: PathBuf::from(&edge.to),
                            line: edge.line,
                        });
                }
                // `EdgeKind` is `#[non_exhaustive]`; future variants are silently
                // ignored here until the routing rule is extended.
                _ => {}
            }
        }

        // Sorted + deduped so the recorded index is deterministic
        // regardless of edge order (one type name typically contributes
        // several `Inherits` edges — one per base).
        if !extra_keys.is_empty() {
            extra_keys.sort_unstable();
            extra_keys.dedup();
            self.adj_extra_keys.insert(path.clone(), extra_keys);
        }

        // Inserted last: `derivable_keys` borrows `symbol_ids` for the
        // edge loop above, and the loop reads nothing out of `files`.
        self.files.insert(
            path,
            FileEntry {
                language: fg.language,
                symbol_ids,
            },
        );
    }

    /// Remove all symbols and edges originating from the given file.
    ///
    /// Cleanup covers all five storage maps:
    /// - `nodes`: every symbol whose ID appears in `files[path].symbol_ids`
    /// - `adj` and `radj`: every entry whose `file` field equals `path`,
    ///   purging keys whose vec becomes empty
    /// - `includes[path]`: removed
    /// - `files[path]`: removed
    ///
    /// Unknown paths are a no-op.
    pub fn remove_file(&mut self, path: &Path) {
        self.remove_file_unsafe(path);
    }

    /// Drop every cached file whose path lies inside `scope`. Returns the
    /// paths that were removed (useful for warning surfaces and
    /// telemetry).
    ///
    /// Use case: `analyze_codebase(scope, force=true)` on a subtree of
    /// a configured project. The project-wide cache may contain prior
    /// entries from inside `scope` whose state is about to be
    /// re-parsed from disk. Wiping them first ensures the rebuild
    /// starts from a known-empty slate within the scope. Entries
    /// OUTSIDE `scope` are untouched — the "lazy / scoped indexing"
    /// contract requires that a `force=true` at a subtree only
    /// invalidates that subtree.
    ///
    /// Scope membership is the path-trie subtree containment relation,
    /// equivalent to `Path::starts_with` after segment normalization:
    /// `/proj/a/b/c.cpp` is in scope `/proj/a` but not in scope
    /// `/proj/d`. Both arguments must already be canonicalized; the
    /// indexer guarantees stored paths are absolute and `\\?\`-stripped
    /// (see CLAUDE.md Core invariants). The trie walk is
    /// `O(files-in-scope)` rather than the full-map filter the
    /// pre-Phase-E HashMap shape required.
    pub fn drop_files_in_scope(&mut self, scope: &Path) -> Vec<PathBuf> {
        let in_scope: Vec<PathBuf> = self.files.iter_subtree(scope).map(|(p, _)| p).collect();
        for path in &in_scope {
            self.remove_file_unsafe(path);
        }
        in_scope
    }

    /// Stat every cached file whose path lies inside `scope` and drop
    /// the ones that no longer exist on disk. Returns the removed
    /// paths.
    ///
    /// Use case: lazy / scoped `analyze_codebase(scope)` without
    /// `force=true`. The cache may have entries for files that were
    /// deleted on disk since the last invocation; on a scoped analyze
    /// the parser would never observe them as missing (it only walks
    /// what's there now), so eviction needs an explicit pass. Entries
    /// outside `scope` are NOT stat-checked here — that's the
    /// opportunistic out-of-scope sweep's job, run on a separate
    /// cadence by [`Graph::sweep_missing_out_of_scope`].
    ///
    /// Cost: one `fs::metadata` per cached file under `scope`. On a
    /// large subtree this is O(files-in-scope) syscalls; cheap relative
    /// to re-parsing but not free. Callers should not invoke this on
    /// the project root unless they intend a full project re-validation.
    pub fn evict_missing_in_scope(&mut self, scope: &Path) -> Vec<PathBuf> {
        let candidates: Vec<PathBuf> = self.files.iter_subtree(scope).map(|(p, _)| p).collect();
        let mut removed = Vec::new();
        for path in candidates {
            if !path.exists() {
                self.remove_file_unsafe(&path);
                removed.push(path);
            }
        }
        removed
    }

    /// Stat every cached file whose path lies OUTSIDE `scope` and drop
    /// the ones that no longer exist on disk. Returns the removed
    /// paths.
    ///
    /// Use case: the opportunistic project-wide hygiene sweep run
    /// every N hours since the last sweep. A scoped
    /// `analyze_codebase(scope)` doesn't touch out-of-scope entries by
    /// design, so files that were moved or deleted in other parts of
    /// the project would otherwise accumulate as cache ghosts.
    /// Periodic sweep keeps the cache consistent without paying full
    /// re-validation cost on every invocation.
    ///
    /// Cost: one `fs::metadata` per cached file OUTSIDE `scope`. On a
    /// large project this is O(files-out-of-scope) syscalls and is
    /// only done at the configured sweep cadence (default: 24h —
    /// see persist.rs `LastSweepAt`). Walks the whole trie since the
    /// query is the complement of a subtree.
    pub fn sweep_missing_out_of_scope(&mut self, scope: &Path) -> Vec<PathBuf> {
        let candidates: Vec<PathBuf> = self
            .files
            .keys()
            .filter(|p| !p.starts_with(scope))
            .collect();
        let mut removed = Vec::new();
        for path in candidates {
            if !path.exists() {
                self.remove_file_unsafe(&path);
                removed.push(path);
            }
        }
        removed
    }

    /// Remove every indexed file at or under `prefix`, returning the
    /// union of symbol IDs that were dropped across all of them.
    ///
    /// **Phase E.3 (B) payoff site.** The file-table drop is a single
    /// `PathTrie::remove_subtree` walk (`O(subtree-size)`) instead of
    /// the iterate-the-full-files-map-and-filter-by-prefix loop the
    /// pre-Phase-E HashMap shape would have required. Includes-table
    /// drop is similarly one trie op. Per-file `adj`/`radj` scrubbing
    /// goes through [`Graph::remove_edges_from_file`], which visits only
    /// the adjacency keys that can hold each dropped file's edges — so
    /// the whole call is `O(subtree-size + edges-of-subtree)` rather
    /// than the `O(files-in-subtree × all edges)` the previous
    /// whole-map filter cost.
    ///
    /// Callers (today: the watch handler's directory-remove path)
    /// must feed the returned `HashSet<SymbolId>` to
    /// [`Graph::prune_dangling_edges`] to scrub any cross-file edges
    /// that target the now-removed symbols, mirroring the
    /// single-file [`Graph::remove_file`] + `prune_dangling_edges`
    /// dance.
    ///
    /// Returns an empty set if `prefix` doesn't match any indexed
    /// file.
    pub fn remove_files_under(&mut self, prefix: &Path) -> HashSet<SymbolId> {
        let mut removed_ids: HashSet<SymbolId> = HashSet::new();
        // Drop the files-table subtree in one trie op and use the
        // returned (path, FileEntry) pairs to drive per-file cleanup
        // of nodes / adj / radj.
        for (path, entry) in self.files.remove_subtree(prefix) {
            for id in &entry.symbol_ids {
                self.nodes.remove(id);
                removed_ids.insert(id.clone());
            }
            self.remove_edges_from_file(&path, &entry.symbol_ids);
        }
        // Includes table: a separate trie op; the dropped entries
        // need no follow-up since we already scrubbed adj/radj per
        // file above (and `Includes` edges live in `includes`, not
        // adj/radj).
        let _ = self.includes.remove_subtree(prefix);
        let _ = self.resolver_metadata.remove_subtree(prefix);
        removed_ids
    }

    // "unsafe" here means "caller must hold the write lock on the
    // `parking_lot::RwLock` that wraps the server-side Graph". No Rust
    // `unsafe` code is involved.
    fn remove_file_unsafe(&mut self, path: &Path) {
        // Take the file entry out first: it owns the symbol-id list the
        // targeted edge scrub needs as its key set, and taking it means
        // that list is never cloned on the merge hot path. Nothing
        // between here and the end of the method reads `files`.
        let symbol_ids = self
            .files
            .remove(path)
            .map(|entry| entry.symbol_ids)
            .unwrap_or_default();

        // Remove nodes for this file's symbols.
        for id in &symbol_ids {
            self.nodes.remove(id);
        }

        // Remove adj/radj entries sourced from this file. Runs even when
        // `files` did not know the path: the previous whole-map filter
        // dropped `file == path` entries regardless of `files`
        // membership, and the file-path pseudo-key can carry edges
        // (file-scope lambdas) with no owning symbol at all.
        self.remove_edges_from_file(path, &symbol_ids);

        self.includes.remove(path);
        self.resolver_metadata.remove(path);
    }

    /// Remove every `adj`/`radj` entry contributed by `path`'s parse,
    /// visiting only the adjacency keys that can hold them.
    ///
    /// **Why this exists.** The predecessor (`retain_edges_not_from`)
    /// filtered the *entire* `adj` and then the entire `radj` map on
    /// every call, so removing one file cost `O(all edges in the
    /// graph)` with a component-wise `Path` compare per entry. Because
    /// [`Graph::merge_file_graph`] removes each re-merged file first, an
    /// incremental re-index of a large cached graph degraded to
    /// `O(files × all edges)` — observed as a multi-day single-threaded
    /// spin on an 11 GB graph. Visiting only the keys that can hold
    /// `path`'s edges makes a removal `O(edges-of-file)`.
    ///
    /// **Key-set invariant.** Every `adj` entry whose `file` equals
    /// `path` lives under one of three keys:
    /// 1. one of `path`'s own symbol ids (`symbol_ids`) — ordinary
    ///    `Calls` and `Overrides` edges, whose `from` is the enclosing
    ///    symbol's id;
    /// 2. the file-path pseudo-key — the exact path string that
    ///    [`Graph::merge_file_graph`] saw as `FileGraph.path`, used as
    ///    `from` by file-scope call edges (C++ lambda at global scope,
    ///    Go package-level closure fallback);
    /// 3. [`Graph::adj_extra_keys`] for `path` — every remaining key,
    ///    in practice `Inherits` edges keyed by the derived type name
    ///    (`Derived`, `Foo<T>`), recorded at merge time and rebuilt
    ///    after a cache load by [`Graph::rebuild_adj_extra_keys`].
    ///
    /// `radj` needs no key index of its own: every `radj` entry with
    /// `file == path` is the mirror of one of the `adj` entries dropped
    /// here, and its key is that entry's `target` — so the targets
    /// collected while filtering forward are exactly the reverse keys to
    /// scrub. Cross-file edges that merely *target* a symbol of `path`
    /// carry their own file in `file` and are deliberately left alone;
    /// [`Graph::prune_dangling_edges`] is the caller-driven pass for
    /// those.
    fn remove_edges_from_file(&mut self, path: &Path, symbol_ids: &[SymbolId]) {
        let path_key = path.to_string_lossy().into_owned();
        // Taken, not read: the file's residual keys describe the parse
        // being removed, so they go away with it. `merge_file_graph`
        // records the fresh set afterwards.
        let extra_keys = self.adj_extra_keys.remove(path).unwrap_or_default();

        // Reverse keys to scrub, gathered while filtering forward.
        // Duplicates are harmless — a repeat visit finds the key already
        // filtered or already gone.
        let mut targets: Vec<SymbolId> = Vec::new();
        for key in symbol_ids
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(path_key.as_str()))
            .chain(extra_keys.iter().map(String::as_str))
        {
            let Some(entries) = self.adj.get_mut(key) else {
                continue;
            };
            entries.retain(|e| {
                if e.file == path {
                    targets.push(e.target.clone());
                    false
                } else {
                    true
                }
            });
            // Keys whose vec becomes empty are dropped so `adj.len()`
            // keeps reflecting active sources.
            if entries.is_empty() {
                self.adj.remove(key);
            }
        }

        for target in targets {
            let Some(entries) = self.radj.get_mut(&target) else {
                continue;
            };
            entries.retain(|e| e.file != path);
            if entries.is_empty() {
                self.radj.remove(&target);
            }
        }
    }

    /// Rebuild [`Graph::adj_extra_keys`] from the current `adj` map.
    ///
    /// A cache load assigns `adj`/`radj` wholesale, and the index is not
    /// part of the on-disk format, so the load path calls this once to
    /// restore the removal fast path for cached files. Without it the
    /// first removal or re-merge of a cached file would leave its
    /// type-name-keyed `Inherits` entries behind and then duplicate them
    /// on merge.
    ///
    /// Cost: one pass over `adj` — the same order as the load that
    /// produced the map, and paid once per load rather than per removed
    /// file.
    ///
    /// A key is treated as derivable — and so left out of the index —
    /// only when it is the file's own pseudo-key or a `<file>:…`-shaped
    /// symbol id ([`code_graph_core::symbol_id`]) that still has a live
    /// node. Anything else is recorded, including a `<file>:…`-shaped
    /// key with no surviving node, so the rebuild errs toward listing
    /// too much (cheap) rather than too little (a leak).
    pub(crate) fn rebuild_adj_extra_keys(&mut self) {
        let mut extras: HashMap<PathBuf, Vec<SymbolId>> = HashMap::new();
        for (key, entries) in &self.adj {
            for entry in entries {
                if Self::key_is_derivable_from_file(key, &entry.file, &self.nodes) {
                    continue;
                }
                extras
                    .entry(entry.file.clone())
                    .or_default()
                    .push(key.clone());
            }
        }
        for keys in extras.values_mut() {
            keys.sort_unstable();
            keys.dedup();
        }
        self.adj_extra_keys = extras;
    }

    /// Whether [`Graph::remove_edges_from_file`] reaches `key` for
    /// `file` without consulting [`Graph::adj_extra_keys`]: the
    /// file-path pseudo-key, or a `<file>:…`-shaped symbol id backed by
    /// a live node.
    fn key_is_derivable_from_file(key: &str, file: &Path, nodes: &HashMap<SymbolId, Node>) -> bool {
        let file_key = file.to_string_lossy();
        if key == file_key {
            return true;
        }
        key.strip_prefix(file_key.as_ref())
            .is_some_and(|rest| rest.starts_with(':') && nodes.contains_key(key))
    }

    /// Scrub every adjacency entry that points at a symbol in `removed_ids`.
    ///
    /// `remove_file_unsafe` only deletes edges whose `file` equals the
    /// removed path. That covers edges *originating from* the file, but
    /// leaves dangling cross-file edges that *target* a now-removed symbol:
    /// e.g. file `B`'s call edge `B:caller → A:old_fn` is stored with
    /// `file = B` and survives a `remove_file(A)`, even though `A:old_fn`
    /// is gone from `nodes`. The watch-mode reindex path uses this method,
    /// scoped to the symbol IDs that genuinely disappeared during a
    /// rename / delete, to keep `adj`/`radj` consistent with `nodes`.
    ///
    /// Cost: O(edges touching the removed IDs), not O(all edges). The
    /// `HashSet` lookup is O(1), and most reindexes have a removed-set of
    /// size 0 or 1 (a routine modify with no rename touches no IDs at all).
    ///
    /// Inbound re-resolution — rebinding `B:caller`'s call to a renamed
    /// `A:new_fn` — is intentionally **out of scope**: that requires
    /// re-parsing `B`, which the watch event for `A` does not warrant. The
    /// agent sees `B:caller` with no recorded callee instead of phantom
    /// data; a subsequent edit to `B` will re-resolve naturally.
    pub fn prune_dangling_edges(&mut self, removed_ids: &HashSet<SymbolId>) {
        if removed_ids.is_empty() {
            return;
        }
        Self::retain_edges_not_targeting(&mut self.adj, removed_ids);
        Self::retain_edges_not_targeting(&mut self.radj, removed_ids);
        // Also drop any radj keys for removed symbols: their incoming-edge
        // list belongs to a node that no longer exists. (The same-file
        // incoming entries were already cleaned by remove_file_unsafe; this
        // catches cross-file ones.)
        for id in removed_ids {
            self.radj.remove(id);
            self.adj.remove(id);
        }
    }

    /// Filter edge map entries: drop edges whose `target` is in `removed`,
    /// and drop keys whose retained vec is empty.
    fn retain_edges_not_targeting(
        map: &mut HashMap<SymbolId, Vec<EdgeEntry>>,
        removed: &HashSet<SymbolId>,
    ) {
        map.retain(|_, entries| {
            entries.retain(|e| !removed.contains(&e.target));
            !entries.is_empty()
        });
    }

    /// Reset the graph to empty. All sparse storage maps are cleared and the
    /// sweep timestamp is reset to `0` (never-swept). After a
    /// `clear()` the graph is structurally equivalent to a fresh
    /// [`Graph::new`].
    pub fn clear(&mut self) {
        self.nodes.clear();
        self.adj.clear();
        self.radj.clear();
        self.files.clear();
        self.includes.clear();
        self.resolver_metadata.clear();
        self.adj_extra_keys.clear();
        self.last_sweep_at = 0;
    }

    /// Nanoseconds since UNIX_EPOCH of the most recent out-of-scope
    /// hygiene sweep. `0` means "never swept" — the
    /// `analyze_codebase` handler treats this as "run a sweep now"
    /// on the first invocation after a load.
    pub fn last_sweep_at(&self) -> u64 {
        self.last_sweep_at
    }

    /// Count of cached files whose path lies inside `scope`. Used by
    /// the `analyze_codebase` handler to surface a "project cache
    /// contains N files outside the current scope" warning so users
    /// running scoped analyses can see the breakdown of accumulated
    /// vs. just-refreshed state.
    pub fn files_in_scope_count(&self, scope: &Path) -> usize {
        self.files.keys().filter(|p| p.starts_with(scope)).count()
    }

    /// Iterate every indexed [`Symbol`] in the graph. Used by the
    /// `search_symbols` Levenshtein fallback (`handlers/symbols.rs`)
    /// when substring search returns zero hits and the user's query
    /// is plausibly a typo: an N-way scan over symbol names looking
    /// for ones within an edit-distance threshold.
    ///
    /// Order is HashMap-defined; callers that need a deterministic
    /// ordering must sort the output themselves. No allocation beyond
    /// the iterator state.
    pub fn all_symbols(&self) -> impl Iterator<Item = &Symbol> {
        self.nodes.values().map(|n| &n.symbol)
    }

    /// Return every class-like symbol whose `name` exactly equals
    /// `name`. "Class-like" means
    /// `Class | Struct | Interface | Trait` — the same set
    /// `Graph::class_hierarchy` treats as a hierarchy entry-point.
    ///
    /// Used by `get_class_hierarchy`'s ambiguity gate: when more
    /// than one class-like symbol shares a bare name (e.g. UE's
    /// `UObject` and ICU's `UObject`), the hierarchy walker would
    /// silently merge them under a single bare-key node — surfacing
    /// the ambiguity as an explicit error with the candidate list
    /// (drawn from this function) lets the agent disambiguate by
    /// fully-qualified symbol_id instead.
    ///
    /// Match is case-sensitive (matches the existing
    /// `class_hierarchy` behaviour). Order is HashMap-defined;
    /// callers should sort if they need stable output.
    pub fn find_classes_named<'a>(&'a self, name: &str) -> Vec<&'a Symbol> {
        self.nodes
            .values()
            .filter(|node| {
                node.symbol.name == name
                    && matches!(
                        node.symbol.kind,
                        code_graph_core::SymbolKind::Class
                            | code_graph_core::SymbolKind::Struct
                            | code_graph_core::SymbolKind::Interface
                            | code_graph_core::SymbolKind::Trait
                    )
            })
            .map(|n| &n.symbol)
            .collect()
    }

    /// Set the sweep timestamp. The handler calls this immediately
    /// after running `sweep_missing_out_of_scope` so the value
    /// persists to the cache via the next [`Graph::save`].
    pub fn set_last_sweep_at(&mut self, nanos: u64) {
        self.last_sweep_at = nanos;
    }

    /// Associate sparse resolver metadata with an indexed path. This is kept
    /// separate from [`FileGraph`] because only selected language plugins use
    /// it. A subsequent [`Self::merge_file_graph`] for the same path clears
    /// the old value; set fresh metadata after that merge. Unknown paths and
    /// metadata whose language does not match the indexed file are no-ops.
    pub fn set_resolver_metadata(&mut self, path: PathBuf, metadata: ResolverMetadata) {
        if matches!(&metadata, ResolverMetadata::Go { .. })
            && matches!(self.files.get(&path), Some(file) if file.language == Language::Go)
        {
            self.resolver_metadata.insert(path, metadata);
        }
    }

    /// Remove sparse resolver metadata for one path. Unknown paths are a
    /// no-op, mirroring [`Self::remove_file`].
    pub fn remove_resolver_metadata(&mut self, path: &Path) {
        self.resolver_metadata.remove(path);
    }

    /// Clone all sparse resolver metadata in deterministic path order for
    /// restoration into language plugins after a cache load.
    pub fn resolver_metadata_snapshot(&self) -> Vec<(PathBuf, ResolverMetadata)> {
        let mut out: Vec<_> = self
            .resolver_metadata
            .iter()
            .map(|(path, metadata)| (path, metadata.clone()))
            .collect();
        out.sort_by(|(left, _), (right, _)| left.cmp(right));
        out
    }

    /// Return indexed files of `language` that have no persisted resolver
    /// metadata. This identifies caches written before a language introduced
    /// its sparse metadata extension without exposing the metadata table.
    pub fn files_missing_resolver_metadata(&self, language: Language) -> Vec<PathBuf> {
        let mut paths: Vec<_> = self
            .files
            .iter()
            .filter_map(|(path, entry)| {
                (entry.language == language && !self.resolver_metadata.contains_path(&path))
                    .then_some(path)
            })
            .collect();
        paths.sort();
        paths
    }

    /// Reconstruct a `Vec<FileGraph>` from internal storage with the
    /// `symbols` populated (in insertion order) and `edges` left empty.
    ///
    /// This is the cheap snapshot the watch-mode incremental reindex
    /// path needs: to call language-aware edge resolution
    /// (`code_graph_tools::indexer::resolve_all_edges`) on a single
    /// re-parsed file, the resolver builds a `(Language, name)`-keyed
    /// `SymbolIndex` plus a basename-keyed `FileIndex` over the *whole*
    /// graph. Both indexes look at `fg.symbols` and `fg.language` only —
    /// `fg.edges` is irrelevant to index construction — so this snapshot
    /// can leave `edges` empty and still produce a complete index.
    ///
    /// Cost: O(symbols) clones plus one `Vec<FileGraph>` allocation.
    /// Iteration order over `files` is HashMap-defined, but the resolver
    /// does not depend on order (it's an inverted index).
    pub fn file_graphs_snapshot(&self) -> Vec<FileGraph> {
        let mut out = Vec::with_capacity(self.files.len());
        for (path, entry) in &self.files {
            let mut symbols = Vec::with_capacity(entry.symbol_ids.len());
            for id in &entry.symbol_ids {
                if let Some(node) = self.nodes.get(id) {
                    symbols.push(node.symbol.clone());
                }
            }
            out.push(FileGraph {
                path: path.to_string_lossy().into_owned(),
                language: entry.language,
                symbols,
                edges: Vec::new(),
            });
        }
        out
    }

    /// Storage-size summary. `edges` sums adjacency entries and include
    /// edges, matching Go's `Stats()` (which counts each include once and
    /// each call/inherit once via the forward `adj` map only — the reverse
    /// `radj` is *not* double-counted).
    pub fn stats(&self) -> GraphStats {
        let adj_edges: usize = self.adj.values().map(Vec::len).sum();
        let include_edges: usize = self.includes.values().map(Vec::len).sum();
        GraphStats {
            nodes: self.nodes.len() as u32,
            edges: (adj_edges + include_edges) as u32,
            files: self.files.len() as u32,
        }
    }

    // ----- Read accessors used only by the in-module tests. The public
    // query surface (`file_symbols`, `symbol_detail`, etc.) is built on top
    // of these private maps. Gated to `cfg(test)` so the dead_code lint
    // stays clean.
    #[cfg(test)]
    fn nodes(&self) -> &HashMap<SymbolId, Node> {
        &self.nodes
    }

    #[cfg(test)]
    fn adj(&self) -> &HashMap<SymbolId, Vec<EdgeEntry>> {
        &self.adj
    }

    #[cfg(test)]
    fn radj(&self) -> &HashMap<SymbolId, Vec<EdgeEntry>> {
        &self.radj
    }

    #[cfg(test)]
    fn files(&self) -> &code_graph_path_trie::PathTrie<FileEntry> {
        &self.files
    }

    #[cfg(test)]
    fn includes(&self) -> &code_graph_path_trie::PathTrie<Vec<IncludeEntry>> {
        &self.includes
    }

    #[cfg(test)]
    fn resolver_metadata(&self) -> &code_graph_path_trie::PathTrie<ResolverMetadata> {
        &self.resolver_metadata
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::{call_edge, include_edge, inherit_edge, make_fg, sym};
    use code_graph_core::{Edge, SymbolKind};

    #[test]
    fn merge_one_file_adds_nodes_and_edges() {
        let mut g = Graph::new();
        let fg = make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("foo", SymbolKind::Function, "/a.cpp"),
                sym("bar", SymbolKind::Function, "/a.cpp"),
            ],
            vec![
                call_edge("/a.cpp:foo", "/a.cpp:bar", "/a.cpp", 1),
                // Inline (not the `line: 0` fixture) so the include's
                // source line is a distinctive non-zero value: this is
                // what proves `merge_file_graph` propagates `edge.line`
                // into the stored `IncludeEntry` rather than defaulting it.
                Edge {
                    from: "/a.cpp".to_string(),
                    to: "/utils.h".to_string(),
                    kind: EdgeKind::Includes,
                    file: "/a.cpp".to_string(),
                    line: 12,
                    confidence: Confidence::Resolved,
                    candidates: 1,
                    shape: Default::default(),
                },
            ],
        );

        g.merge_file_graph(fg);

        let stats = g.stats();
        assert_eq!(stats.nodes, 2);
        assert_eq!(stats.edges, 2, "1 call + 1 include = 2 edges");
        assert_eq!(stats.files, 1);

        assert!(g.nodes().contains_key("/a.cpp:foo"));
        assert!(g.nodes().contains_key("/a.cpp:bar"));

        // Call edge: forward in adj, reverse in radj.
        assert_eq!(g.adj()["/a.cpp:foo"][0].target, "/a.cpp:bar");
        assert_eq!(g.adj()["/a.cpp:foo"][0].kind, EdgeKind::Calls);
        assert_eq!(g.radj()["/a.cpp:bar"][0].target, "/a.cpp:foo");
        assert_eq!(g.radj()["/a.cpp:bar"][0].kind, EdgeKind::Calls);

        // Include edge: in includes (with its source line preserved),
        // NOT in adj/radj.
        let key = PathBuf::from("/a.cpp");
        assert_eq!(
            *g.includes().get(&key).unwrap(),
            vec![IncludeEntry {
                path: PathBuf::from("/utils.h"),
                line: 12,
            }],
        );

        // Files map records the language.
        assert!(g.files().contains_path(&key));
        assert_eq!(g.files().get(&key).unwrap().language, Language::Cpp);
    }

    #[test]
    fn merge_two_files_aggregates() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("foo", SymbolKind::Function, "/a.cpp")],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("bar", SymbolKind::Function, "/b.cpp")],
            vec![],
        ));

        let stats = g.stats();
        assert_eq!(stats.nodes, 2);
        assert_eq!(stats.files, 2);
        assert_eq!(stats.edges, 0);
        assert!(g.nodes().contains_key("/a.cpp:foo"));
        assert!(g.nodes().contains_key("/b.cpp:bar"));
    }

    #[test]
    fn re_merge_same_path_replaces() {
        let mut g = Graph::new();

        // Initial merge: 2 symbols.
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("foo", SymbolKind::Function, "/a.cpp"),
                sym("bar", SymbolKind::Function, "/a.cpp"),
            ],
            vec![],
        ));
        assert_eq!(g.stats().nodes, 2);

        // Re-merge with a single different symbol — old ones must be gone.
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("baz", SymbolKind::Function, "/a.cpp")],
            vec![],
        ));
        let stats = g.stats();
        assert_eq!(stats.nodes, 1);
        assert_eq!(stats.files, 1);
        assert!(!g.nodes().contains_key("/a.cpp:foo"));
        assert!(!g.nodes().contains_key("/a.cpp:bar"));
        assert!(g.nodes().contains_key("/a.cpp:baz"));

        // Idempotency: re-merging the same shape twice yields the same final state.
        let again = make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("baz", SymbolKind::Function, "/a.cpp")],
            vec![],
        );
        g.merge_file_graph(again.clone());
        g.merge_file_graph(again);
        let stats2 = g.stats();
        assert_eq!(stats, stats2);
        assert_eq!(stats2.nodes, 1);
    }

    #[test]
    fn re_merge_replaces_edges_not_just_nodes() {
        // The realistic incremental-index scenario: a file is edited so its
        // call targets change. Re-merging must drop stale edges, not just
        // stale nodes.
        let mut g = Graph::new();

        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("a", SymbolKind::Function, "/a.cpp"),
                sym("b", SymbolKind::Function, "/a.cpp"),
                sym("c", SymbolKind::Function, "/a.cpp"),
            ],
            vec![call_edge("/a.cpp:a", "/a.cpp:b", "/a.cpp", 1)],
        ));
        assert_eq!(g.adj()["/a.cpp:a"][0].target, "/a.cpp:b");
        assert!(g.radj().contains_key("/a.cpp:b"));

        // Re-merge: same nodes but `a` now calls `c` instead of `b`.
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("a", SymbolKind::Function, "/a.cpp"),
                sym("b", SymbolKind::Function, "/a.cpp"),
                sym("c", SymbolKind::Function, "/a.cpp"),
            ],
            vec![call_edge("/a.cpp:a", "/a.cpp:c", "/a.cpp", 1)],
        ));

        // Stale edge gone, new edge present, no doubling.
        assert_eq!(g.adj()["/a.cpp:a"].len(), 1);
        assert_eq!(g.adj()["/a.cpp:a"][0].target, "/a.cpp:c");
        assert!(
            !g.radj().contains_key("/a.cpp:b"),
            "stale reverse-edge key for old target must be dropped"
        );
        assert!(g.radj().contains_key("/a.cpp:c"));
        assert_eq!(g.stats().edges, 1);
    }

    #[test]
    fn remove_file_cleans_all_storage() {
        let mut g = Graph::new();

        // /a.cpp has a symbol that calls a symbol in /b.cpp, plus an include.
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("foo", SymbolKind::Function, "/a.cpp")],
            vec![
                call_edge("/a.cpp:foo", "/b.cpp:bar", "/a.cpp", 1),
                include_edge("/a.cpp", "/utils.h", "/a.cpp"),
            ],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("bar", SymbolKind::Function, "/b.cpp")],
            vec![call_edge("/b.cpp:bar", "/b.cpp:helper", "/b.cpp", 1)],
        ));

        let path_a = PathBuf::from("/a.cpp");
        g.remove_file(&path_a);

        // Node from /a.cpp gone; node from /b.cpp remains.
        assert!(!g.nodes().contains_key("/a.cpp:foo"));
        assert!(g.nodes().contains_key("/b.cpp:bar"));

        // adj/radj entries with file == /a.cpp gone. The radj key
        // "/b.cpp:bar" was populated only by the /a.cpp call, so the whole
        // key is gone — the edge originating from /b.cpp survives.
        assert!(!g.adj().contains_key("/a.cpp:foo"));
        assert!(!g.radj().contains_key("/b.cpp:bar"));
        // Edge originating from /b.cpp (file=/b.cpp) is preserved.
        assert!(g.adj().contains_key("/b.cpp:bar"));
        assert_eq!(g.adj()["/b.cpp:bar"][0].target, "/b.cpp:helper");

        // Includes for /a.cpp gone; files entry for /a.cpp gone.
        assert!(!g.includes().contains_path(&path_a));
        assert!(!g.files().contains_path(&path_a));
        assert!(g.files().contains_path(PathBuf::from("/b.cpp")));
    }

    // --- Targeted edge removal (`remove_edges_from_file`) ---------------
    //
    // These pin the key-set invariant the targeted scrub relies on. The
    // predecessor filtered every entry in `adj` and `radj` on each call,
    // so it could not miss a key by construction; the fast path visits
    // only the keys it can derive, and each test below covers one of the
    // three key shapes (own symbol id, file-path pseudo-key, recorded
    // residual type-name key).

    #[test]
    fn remove_file_scrubs_reverse_mirror_under_cross_file_target() {
        // The `radj` half of the invariant: A's outbound call is stored
        // twice — forward under `A:a_fn`, reverse under the *target*
        // `B:b_fn`. Removing A must scrub both, and the reverse key it
        // has to reach is keyed by a symbol that belongs to B, not A.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("a_fn", SymbolKind::Function, "/a.cpp")],
            vec![call_edge("/a.cpp:a_fn", "/b.cpp:b_fn", "/a.cpp", 4)],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("b_fn", SymbolKind::Function, "/b.cpp")],
            vec![call_edge("/b.cpp:b_fn", "/b.cpp:helper", "/b.cpp", 9)],
        ));

        // Sentinel: the reverse mirror exists before the removal, so a
        // failure below is the scrub misbehaving rather than the merge
        // never having recorded the edge.
        assert_eq!(
            g.radj()["/b.cpp:b_fn"]
                .iter()
                .filter(|e| e.file == Path::new("/a.cpp"))
                .count(),
            1,
            "sentinel: A's call must be mirrored under B's symbol pre-removal"
        );

        g.remove_file(Path::new("/a.cpp"));

        // Forward key gone with the file.
        assert!(!g.adj().contains_key("/a.cpp:a_fn"));
        // Reverse mirror under B's symbol carries no A-sourced entry.
        // `/a.cpp` was that key's only contributor, so the key itself is
        // dropped rather than left empty.
        assert!(
            g.radj()
                .get("/b.cpp:b_fn")
                .is_none_or(|v| v.iter().all(|e| e.file != Path::new("/a.cpp"))),
            "reverse mirror of a removed file's edge must be scrubbed"
        );
        // B is untouched: its own symbol, edge, and reverse key survive.
        assert!(g.nodes().contains_key("/b.cpp:b_fn"));
        assert_eq!(g.adj()["/b.cpp:b_fn"][0].target, "/b.cpp:helper");
        assert!(g.radj().contains_key("/b.cpp:helper"));
    }

    #[test]
    fn remove_file_scrubs_file_scope_pseudo_key_edges() {
        // C++ lambda-at-global-scope / Go package-level closure shape:
        // the call has no enclosing symbol, so `from` is the file path
        // itself and the file contributes zero symbols. The pseudo-key
        // is the only way to reach that edge.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/m.cpp",
            Language::Cpp,
            vec![sym("m_fn", SymbolKind::Function, "/m.cpp")],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            "/l.cpp",
            Language::Cpp,
            // No symbols at all — `symbol_ids` is empty for this file.
            vec![],
            vec![call_edge("/l.cpp", "/m.cpp:m_fn", "/l.cpp", 2)],
        ));

        // Sentinel: the pseudo-key edge landed where we think it did.
        assert_eq!(g.adj()["/l.cpp"][0].target, "/m.cpp:m_fn");
        assert!(g.radj().contains_key("/m.cpp:m_fn"));

        g.remove_file(Path::new("/l.cpp"));

        assert!(
            !g.adj().contains_key("/l.cpp"),
            "file-path pseudo-key must be scrubbed with the file"
        );
        assert!(
            !g.radj().contains_key("/m.cpp:m_fn"),
            "pseudo-key edge's reverse mirror must go too"
        );
        // The callee's own file is untouched.
        assert!(g.nodes().contains_key("/m.cpp:m_fn"));
        assert!(g.files().contains_path(PathBuf::from("/m.cpp")));
    }

    #[test]
    fn remove_file_scrubs_inherits_edges_keyed_by_type_name() {
        // `Inherits` edges are keyed by the derived TYPE NAME, not by a
        // symbol id: `adj["Derived"]`, and `adj["Box<T>"]` for the
        // generics-verbatim forms. Neither is derivable from
        // `files[path].symbol_ids`, so they are only reachable through
        // the recorded residual key set. A miss here would leave the
        // edge behind and duplicate it on the next merge.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("Base", SymbolKind::Class, "/a.cpp"),
                sym("Derived", SymbolKind::Class, "/a.cpp"),
            ],
            vec![
                inherit_edge("Derived", "Base", "/a.cpp"),
                // Generic form: no symbol carries this name at all.
                inherit_edge("Box<T>", "Base", "/a.cpp"),
            ],
        ));
        // A second file inheriting from the same base: its entries must
        // survive, proving the scrub filters by `file` rather than
        // dropping shared keys wholesale.
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("Other", SymbolKind::Class, "/b.cpp")],
            vec![inherit_edge("Other", "Base", "/b.cpp")],
        ));

        // Sentinel: both type-name keys exist pre-removal.
        assert!(g.adj().contains_key("Derived"));
        assert!(g.adj().contains_key("Box<T>"));
        assert_eq!(g.radj()["Base"].len(), 3);

        g.remove_file(Path::new("/a.cpp"));

        assert!(
            !g.adj().contains_key("Derived"),
            "bare type-name inherits key must be scrubbed"
        );
        assert!(
            !g.adj().contains_key("Box<T>"),
            "generics-verbatim inherits key must be scrubbed"
        );
        // The shared reverse key survives, holding only B's entry.
        let base_in = g.radj().get("Base").expect("B still inherits from Base");
        assert_eq!(base_in.len(), 1);
        assert_eq!(base_in[0].target, "Other");
        assert!(g.adj().contains_key("Other"));
    }

    #[test]
    fn re_merge_replaces_inherits_edges_keyed_by_type_name() {
        // Watch-mode reindex of a file whose class changes base. The
        // type-name key is not a symbol id, so a removal that only
        // consulted `symbol_ids` would accumulate one stale entry per
        // re-merge instead of replacing it.
        let mut g = Graph::new();
        let first = make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("Derived", SymbolKind::Class, "/a.cpp")],
            vec![inherit_edge("Derived", "Base", "/a.cpp")],
        );
        g.merge_file_graph(first.clone());
        let baseline = g.stats();

        // Same shape twice: counts must not grow.
        g.merge_file_graph(first);
        assert_eq!(
            g.stats(),
            baseline,
            "re-merging an identical FileGraph must not duplicate inherits edges"
        );
        assert_eq!(g.adj()["Derived"].len(), 1);

        // Now the base changes: the stale edge must be gone, not merely
        // joined by the new one.
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("Derived", SymbolKind::Class, "/a.cpp")],
            vec![inherit_edge("Derived", "Rebased", "/a.cpp")],
        ));
        assert_eq!(g.adj()["Derived"].len(), 1);
        assert_eq!(g.adj()["Derived"][0].target, "Rebased");
        assert!(
            !g.radj().contains_key("Base"),
            "reverse key of the abandoned base must be dropped"
        );
        assert!(g.radj().contains_key("Rebased"));
        assert_eq!(g.stats().edges, 1);
    }

    #[test]
    fn remove_unknown_path_leaves_graph_untouched() {
        // Unknown-path removal stays a no-op for graph contents even
        // though it now runs the targeted scrub (which probes the
        // pseudo-key unconditionally).
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("foo", SymbolKind::Function, "/a.cpp")],
            vec![
                call_edge("/a.cpp:foo", "/a.cpp:bar", "/a.cpp", 1),
                inherit_edge("Derived", "Base", "/a.cpp"),
            ],
        ));
        let before = g.stats();

        g.remove_file(Path::new("/nowhere.cpp"));

        assert_eq!(g.stats(), before);
        assert!(g.adj().contains_key("/a.cpp:foo"));
        assert!(g.adj().contains_key("Derived"));
        assert!(g.files().contains_path(PathBuf::from("/a.cpp")));
    }

    #[test]
    fn remove_files_under_scrubs_type_name_keys_per_dropped_file() {
        // `remove_files_under` shares the same targeted scrub, driven by
        // the `FileEntry` each `remove_subtree` pair hands back.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a/x.cpp",
            Language::Cpp,
            vec![sym("InA", SymbolKind::Class, "/a/x.cpp")],
            vec![inherit_edge("InA", "Base", "/a/x.cpp")],
        ));
        g.merge_file_graph(make_fg(
            "/b/z.cpp",
            Language::Cpp,
            vec![sym("InB", SymbolKind::Class, "/b/z.cpp")],
            vec![inherit_edge("InB", "Base", "/b/z.cpp")],
        ));

        let removed = g.remove_files_under(Path::new("/a"));

        assert!(removed.contains("/a/x.cpp:InA"));
        assert!(
            !g.adj().contains_key("InA"),
            "dropped file's type-name key must be scrubbed"
        );
        // Out-of-subtree file keeps its edge and its share of the shared
        // reverse key.
        assert!(g.adj().contains_key("InB"));
        let base_in = g.radj().get("Base").expect("B's inherits edge survives");
        assert_eq!(base_in.len(), 1);
        assert_eq!(base_in[0].target, "InB");
    }

    #[test]
    fn remove_files_under_drops_full_subtree() {
        // /a/x.cpp and /a/sub/y.cpp both under /a; /b/z.cpp under /b.
        // remove_files_under("/a") should drop both /a-files and leave
        // /b alone. Returned symbol-id set must include the dropped
        // symbols so the caller's prune_dangling_edges pass scrubs
        // cross-file edges that targeted them.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a/x.cpp",
            Language::Cpp,
            vec![sym("foo", SymbolKind::Function, "/a/x.cpp")],
            vec![include_edge("/a/x.cpp", "/utils.h", "/a/x.cpp")],
        ));
        g.merge_file_graph(make_fg(
            "/a/sub/y.cpp",
            Language::Cpp,
            vec![sym("bar", SymbolKind::Function, "/a/sub/y.cpp")],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            "/b/z.cpp",
            Language::Cpp,
            // /b.cpp's `baz` calls /a/x.cpp's `foo` — gives us a
            // cross-subtree edge that should be scrubbed.
            vec![sym("baz", SymbolKind::Function, "/b/z.cpp")],
            vec![call_edge("/b/z.cpp:baz", "/a/x.cpp:foo", "/b/z.cpp", 1)],
        ));

        let removed = g.remove_files_under(Path::new("/a"));

        // Both /a-files dropped from the file index.
        assert!(!g.files().contains_path(PathBuf::from("/a/x.cpp")));
        assert!(!g.files().contains_path(PathBuf::from("/a/sub/y.cpp")));
        // /b stays.
        assert!(g.files().contains_path(PathBuf::from("/b/z.cpp")));

        // Symbols from /a-files are gone from nodes.
        assert!(!g.nodes().contains_key("/a/x.cpp:foo"));
        assert!(!g.nodes().contains_key("/a/sub/y.cpp:bar"));
        // /b's symbol stays.
        assert!(g.nodes().contains_key("/b/z.cpp:baz"));

        // The returned id set covers both dropped symbols.
        assert!(removed.contains("/a/x.cpp:foo"));
        assert!(removed.contains("/a/sub/y.cpp:bar"));
        assert_eq!(removed.len(), 2);

        // Caller-side step: prune dangling edges. After this, /b's
        // `baz → /a/x.cpp:foo` edge must be gone too.
        g.prune_dangling_edges(&removed);
        let baz_edges = g.adj().get("/b/z.cpp:baz").cloned().unwrap_or_default();
        assert!(
            baz_edges.iter().all(|e| e.target != "/a/x.cpp:foo"),
            "cross-subtree edge to dropped symbol must be pruned"
        );

        // Includes table also cleaned: `/a/x.cpp` had an include entry.
        assert!(!g.includes().contains_path(PathBuf::from("/a/x.cpp")));
    }

    #[test]
    fn remove_files_under_unknown_prefix_is_noop() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("foo", SymbolKind::Function, "/a.cpp")],
            vec![],
        ));
        let before = g.stats();
        let removed = g.remove_files_under(Path::new("/nowhere"));
        assert!(removed.is_empty());
        assert_eq!(g.stats(), before);
    }

    #[test]
    fn clear_resets_to_empty() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("foo", SymbolKind::Function, "/a.cpp")],
            vec![include_edge("/a.cpp", "/utils.h", "/a.cpp")],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("bar", SymbolKind::Function, "/b.cpp")],
            vec![],
        ));
        assert_ne!(
            g.stats(),
            GraphStats {
                nodes: 0,
                edges: 0,
                files: 0
            }
        );

        g.clear();

        assert_eq!(
            g.stats(),
            GraphStats {
                nodes: 0,
                edges: 0,
                files: 0,
            }
        );
        assert!(g.nodes().is_empty());
        assert!(g.adj().is_empty());
        assert!(g.radj().is_empty());
        assert!(g.files().is_empty());
        assert!(g.includes().is_empty());
    }

    #[test]
    fn sparse_resolver_metadata_follows_merge_remove_subtree_and_clear() {
        let mut g = Graph::new();
        let a = PathBuf::from("/a/x.go");
        let b = PathBuf::from("/b/y.go");
        let metadata = || ResolverMetadata::Go {
            declared_package: "example".to_string(),
            package_value_bindings: vec!["Value".to_string()],
        };

        g.merge_file_graph(make_fg(
            "/a/x.go",
            Language::Go,
            vec![sym("X", SymbolKind::Function, "/a/x.go")],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            "/b/y.go",
            Language::Go,
            vec![sym("Y", SymbolKind::Function, "/b/y.go")],
            vec![],
        ));
        g.set_resolver_metadata(a.clone(), metadata());
        g.set_resolver_metadata(b.clone(), metadata());
        assert_eq!(g.resolver_metadata_snapshot().len(), 2);

        // Fresh file data invalidates its prior parse metadata.
        g.merge_file_graph(make_fg(
            "/a/x.go",
            Language::Go,
            vec![sym("X", SymbolKind::Function, "/a/x.go")],
            vec![],
        ));
        assert!(!g.resolver_metadata().contains_path(&a));
        assert!(g.resolver_metadata().contains_path(&b));

        g.set_resolver_metadata(a.clone(), metadata());
        let _ = g.remove_files_under(Path::new("/a"));
        assert!(!g.resolver_metadata().contains_path(&a));
        assert!(g.resolver_metadata().contains_path(&b));

        g.remove_file(&b);
        assert!(g.resolver_metadata().is_empty());

        g.set_resolver_metadata(a, metadata());
        g.clear();
        assert!(g.resolver_metadata().is_empty());
    }

    #[test]
    fn resolver_metadata_accepts_only_indexed_go_files() {
        let mut g = Graph::new();
        let go_path = PathBuf::from("/a.go");
        let cpp_path = PathBuf::from("/b.cpp");
        let unknown_path = PathBuf::from("/missing.go");
        let metadata = || ResolverMetadata::Go {
            declared_package: "example".to_string(),
            package_value_bindings: vec!["Value".to_string()],
        };

        g.merge_file_graph(make_fg(
            "/a.go",
            Language::Go,
            vec![sym("A", SymbolKind::Function, "/a.go")],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("B", SymbolKind::Function, "/b.cpp")],
            vec![],
        ));

        g.set_resolver_metadata(go_path.clone(), metadata());
        g.set_resolver_metadata(cpp_path.clone(), metadata());
        g.set_resolver_metadata(unknown_path.clone(), metadata());

        assert!(g.resolver_metadata().contains_path(&go_path));
        assert!(!g.resolver_metadata().contains_path(&cpp_path));
        assert!(!g.resolver_metadata().contains_path(&unknown_path));
    }

    #[test]
    fn merge_routes_inherits_edges_to_adj_radj() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("Base", SymbolKind::Class, "/a.cpp"),
                sym("Derived", SymbolKind::Class, "/a.cpp"),
            ],
            // Inherits edges use bare (non-generic) derived/base names.
            vec![inherit_edge("Derived", "Base", "/a.cpp")],
        ));

        let adj = g.adj();
        let radj = g.radj();

        let derived_out = adj.get("Derived").expect("Derived has adj entry");
        assert_eq!(derived_out.len(), 1);
        assert_eq!(derived_out[0].target, "Base");
        assert_eq!(derived_out[0].kind, EdgeKind::Inherits);

        let base_in = radj.get("Base").expect("Base has radj entry");
        assert_eq!(base_in.len(), 1);
        assert_eq!(base_in[0].target, "Derived");
        assert_eq!(base_in[0].kind, EdgeKind::Inherits);
    }

    #[test]
    fn merge_routes_includes_only_to_includes_map() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![],
            vec![include_edge("/a.cpp", "/utils.h", "/a.cpp")],
        ));

        let key = PathBuf::from("/a.cpp");
        // This test pins the *routing* invariant; the `include_edge`
        // fixture carries `line: 0`, which flows through unchanged.
        assert_eq!(
            *g.includes().get(&key).unwrap(),
            vec![IncludeEntry {
                path: PathBuf::from("/utils.h"),
                line: 0,
            }],
        );
        // Must NOT leak into adj/radj.
        assert!(g.adj().is_empty());
        assert!(g.radj().is_empty());
    }

    #[test]
    fn file_entry_records_language() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("foo", SymbolKind::Function, "/a.cpp")],
            vec![],
        ));
        let entry = g.files().get(PathBuf::from("/a.cpp")).unwrap();
        assert_eq!(entry.language, Language::Cpp);
        assert_eq!(entry.symbol_ids, vec!["/a.cpp:foo".to_string()]);
    }

    #[test]
    fn file_graphs_snapshot_returns_one_entry_per_file_with_no_edges() {
        // The watch-mode snapshot helper must reconstruct one FileGraph per
        // stored file with the file's symbols (in insertion order) and an
        // empty `edges` Vec — edges are merged into adj/radj/includes at
        // merge time and aren't recoverable from internal storage in
        // FileGraph form, but the watch path only needs symbols+language
        // for index construction.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("foo", SymbolKind::Function, "/a.cpp"),
                sym("bar", SymbolKind::Function, "/a.cpp"),
            ],
            vec![call_edge("/a.cpp:foo", "/a.cpp:bar", "/a.cpp", 1)],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("baz", SymbolKind::Function, "/b.cpp")],
            vec![],
        ));

        let snapshot = g.file_graphs_snapshot();
        assert_eq!(snapshot.len(), 2, "one FileGraph per stored file");
        for fg in &snapshot {
            assert!(
                fg.edges.is_empty(),
                "snapshot leaves edges empty (merged into adj/radj/includes already)"
            );
            assert_eq!(fg.language, Language::Cpp);
        }
        // Find /a.cpp's snapshot — order is HashMap-defined. Snapshot paths
        // are trie-reconstructed with the native separator, so normalize the
        // Unix-style fixture before comparing.
        let a = snapshot
            .iter()
            .find(|fg| fg.path.replace('\\', "/") == "/a.cpp")
            .expect("/a.cpp present");
        assert_eq!(a.symbols.len(), 2);
        assert_eq!(a.symbols[0].name, "foo");
        assert_eq!(a.symbols[1].name, "bar");
    }

    #[test]
    fn stats_after_re_merge() {
        let mut g = Graph::new();
        let fg = make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("foo", SymbolKind::Function, "/a.cpp"),
                sym("bar", SymbolKind::Function, "/a.cpp"),
                sym("baz", SymbolKind::Function, "/a.cpp"),
            ],
            vec![
                call_edge("/a.cpp:foo", "/a.cpp:bar", "/a.cpp", 1),
                include_edge("/a.cpp", "/utils.h", "/a.cpp"),
            ],
        );

        g.merge_file_graph(fg.clone());
        let first = g.stats();
        assert_eq!(first.nodes, 3);
        assert_eq!(first.edges, 2);
        assert_eq!(first.files, 1);

        // Re-merge the SAME FileGraph — counts must NOT double.
        g.merge_file_graph(fg);
        let second = g.stats();
        assert_eq!(first, second);
    }

    #[test]
    fn prune_dangling_edges_drops_cross_file_targets_to_removed_symbols() {
        // Watch-mode rename scenario: A defines old_fn; B calls old_fn.
        // After A is reindexed without old_fn, the cross-file edge from
        // B is left dangling (its `file` is B, not A). prune_dangling_edges
        // — given the truly-removed ID set — must scrub it.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![sym("old_fn", SymbolKind::Function, "/a.cpp")],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("caller", SymbolKind::Function, "/b.cpp")],
            // file=B for the cross-file edge — that's the bug-trigger shape.
            vec![call_edge("/b.cpp:caller", "/a.cpp:old_fn", "/b.cpp", 7)],
        ));

        // Sanity: pre-prune, B's caller targets old_fn.
        assert_eq!(g.adj()["/b.cpp:caller"][0].target, "/a.cpp:old_fn");
        assert!(g.radj().contains_key("/a.cpp:old_fn"));

        // Simulate the "old_fn was truly removed" set the watch path computes.
        let mut removed = HashSet::new();
        removed.insert("/a.cpp:old_fn".to_string());
        g.prune_dangling_edges(&removed);

        // The dangling edge is gone, so `adj["/b.cpp:caller"]` either has
        // no entries left (key dropped) or no entry targeting old_fn.
        assert!(
            g.adj()
                .get("/b.cpp:caller")
                .is_none_or(|v| v.iter().all(|e| e.target != "/a.cpp:old_fn")),
            "dangling forward edge to removed symbol must be gone"
        );
        // radj key for old_fn fully cleared — the symbol no longer exists.
        assert!(!g.radj().contains_key("/a.cpp:old_fn"));
    }

    #[test]
    fn prune_dangling_edges_empty_set_is_noop() {
        // Routine reindex (no rename) should produce an empty removed set;
        // the method must be a true no-op so the watch hot path costs
        // nothing on the common case.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("foo", SymbolKind::Function, "/a.cpp"),
                sym("bar", SymbolKind::Function, "/a.cpp"),
            ],
            vec![call_edge("/a.cpp:foo", "/a.cpp:bar", "/a.cpp", 1)],
        ));
        let before = g.stats();
        g.prune_dangling_edges(&HashSet::new());
        assert_eq!(g.stats(), before);
        assert_eq!(g.adj()["/a.cpp:foo"][0].target, "/a.cpp:bar");
    }

    #[test]
    fn prune_dangling_edges_preserves_unrelated_edges() {
        // Only the entry whose `target ∈ removed_ids` should be scrubbed —
        // other entries on the same key, and entries to non-removed
        // symbols, must survive.
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/a.cpp",
            Language::Cpp,
            vec![
                sym("old_fn", SymbolKind::Function, "/a.cpp"),
                sym("kept_fn", SymbolKind::Function, "/a.cpp"),
            ],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            "/b.cpp",
            Language::Cpp,
            vec![sym("caller", SymbolKind::Function, "/b.cpp")],
            vec![
                call_edge("/b.cpp:caller", "/a.cpp:old_fn", "/b.cpp", 1),
                call_edge("/b.cpp:caller", "/a.cpp:kept_fn", "/b.cpp", 2),
            ],
        ));

        let mut removed = HashSet::new();
        removed.insert("/a.cpp:old_fn".to_string());
        g.prune_dangling_edges(&removed);

        let entries = g.adj().get("/b.cpp:caller").expect("caller key kept");
        assert_eq!(entries.len(), 1, "exactly the unrelated edge survives");
        assert_eq!(entries[0].target, "/a.cpp:kept_fn");
        assert!(g.radj().contains_key("/a.cpp:kept_fn"));
        assert!(!g.radj().contains_key("/a.cpp:old_fn"));
    }

    // --- Scope-aware methods: drop_files_in_scope / evict_missing_in_scope /
    // sweep_missing_out_of_scope --------------------------------------------

    /// `drop_files_in_scope` removes every file whose path starts with the
    /// scope path, returns the removed list, and leaves out-of-scope
    /// files intact. The canonical use is the scoped-`force=true` case:
    /// invalidate a subtree without touching the rest of the project.
    #[test]
    fn drop_files_in_scope_removes_only_in_scope_files() {
        let mut g = Graph::new();
        // Two files inside scope, one outside.
        g.merge_file_graph(make_fg(
            "/proj/sub/a.cpp",
            Language::Cpp,
            vec![sym("a", SymbolKind::Function, "/proj/sub/a.cpp")],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            "/proj/sub/b.cpp",
            Language::Cpp,
            vec![sym("b", SymbolKind::Function, "/proj/sub/b.cpp")],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            "/proj/other/c.cpp",
            Language::Cpp,
            vec![sym("c", SymbolKind::Function, "/proj/other/c.cpp")],
            vec![],
        ));
        assert_eq!(g.stats().files, 3);

        let removed = g.drop_files_in_scope(Path::new("/proj/sub"));
        assert_eq!(removed.len(), 2, "exactly the two in-scope files removed");
        assert!(removed.iter().any(|p| p == Path::new("/proj/sub/a.cpp")));
        assert!(removed.iter().any(|p| p == Path::new("/proj/sub/b.cpp")));

        let stats = g.stats();
        assert_eq!(stats.files, 1, "out-of-scope file survives");
        assert_eq!(stats.nodes, 1, "only the out-of-scope symbol remains");
        assert!(g.files().contains_path(PathBuf::from("/proj/other/c.cpp")));
        assert!(!g.files().contains_path(PathBuf::from("/proj/sub/a.cpp")));
    }

    /// `drop_files_in_scope` with no in-scope files is a no-op and
    /// returns an empty Vec.
    #[test]
    fn drop_files_in_scope_no_matches_is_noop() {
        let mut g = Graph::new();
        g.merge_file_graph(make_fg(
            "/proj/other/c.cpp",
            Language::Cpp,
            vec![sym("c", SymbolKind::Function, "/proj/other/c.cpp")],
            vec![],
        ));
        let before = g.stats();
        let removed = g.drop_files_in_scope(Path::new("/proj/sub"));
        assert!(removed.is_empty(), "nothing in scope, nothing removed");
        assert_eq!(g.stats(), before, "graph unchanged");
    }

    /// `evict_missing_in_scope` stats each in-scope cached path; drops
    /// the ones that aren't on disk; leaves the ones that are. Uses a
    /// real tempdir + tempfile so the `Path::exists()` calls are
    /// against a known filesystem state.
    #[test]
    fn evict_missing_in_scope_drops_only_disappeared_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let kept = dir.path().join("kept.cpp");
        let gone = dir.path().join("gone.cpp");
        std::fs::write(&kept, b"// kept\n").unwrap();
        // Deliberately do NOT create `gone` on disk.

        let mut g = Graph::new();
        let kept_str = kept.to_string_lossy().into_owned();
        let gone_str = gone.to_string_lossy().into_owned();
        g.merge_file_graph(make_fg(
            &kept_str,
            Language::Cpp,
            vec![sym("k", SymbolKind::Function, &kept_str)],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            &gone_str,
            Language::Cpp,
            vec![sym("g", SymbolKind::Function, &gone_str)],
            vec![],
        ));
        assert_eq!(g.stats().files, 2);

        let removed = g.evict_missing_in_scope(dir.path());
        assert_eq!(removed.len(), 1, "exactly the missing file removed");
        assert_eq!(removed[0], gone);

        let stats = g.stats();
        assert_eq!(stats.files, 1, "kept.cpp survives");
        assert!(g.files().contains_path(&kept));
        assert!(!g.files().contains_path(&gone));
    }

    /// `evict_missing_in_scope` ignores files OUTSIDE the scope, even
    /// when they're also missing on disk. Out-of-scope hygiene is the
    /// sweep's job, not this method's.
    #[test]
    fn evict_missing_in_scope_ignores_out_of_scope_missing_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let scope = dir.path().join("scope");
        let other = dir.path().join("other");
        std::fs::create_dir(&scope).unwrap();
        std::fs::create_dir(&other).unwrap();
        // Both files cached but neither on disk; only the in-scope one
        // should be dropped.
        let in_scope = scope.join("a.cpp");
        let out_of_scope = other.join("b.cpp");

        let mut g = Graph::new();
        let in_str = in_scope.to_string_lossy().into_owned();
        let out_str = out_of_scope.to_string_lossy().into_owned();
        g.merge_file_graph(make_fg(
            &in_str,
            Language::Cpp,
            vec![sym("a", SymbolKind::Function, &in_str)],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            &out_str,
            Language::Cpp,
            vec![sym("b", SymbolKind::Function, &out_str)],
            vec![],
        ));

        let removed = g.evict_missing_in_scope(&scope);
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0], in_scope);
        assert!(
            g.files().contains_path(&out_of_scope),
            "out-of-scope missing file is preserved"
        );
    }

    /// `sweep_missing_out_of_scope` is the mirror of
    /// `evict_missing_in_scope`: stats every OUT-of-scope cached path
    /// and drops the ones that aren't on disk. In-scope files are
    /// untouched even if they're missing — the eviction pass handles
    /// those.
    #[test]
    fn sweep_missing_out_of_scope_drops_only_out_of_scope_missing() {
        let dir = tempfile::TempDir::new().unwrap();
        let scope = dir.path().join("scope");
        let other = dir.path().join("other");
        std::fs::create_dir(&scope).unwrap();
        std::fs::create_dir(&other).unwrap();
        // In-scope file missing, out-of-scope file also missing.
        let in_scope = scope.join("a.cpp");
        let out_of_scope = other.join("b.cpp");

        let mut g = Graph::new();
        let in_str = in_scope.to_string_lossy().into_owned();
        let out_str = out_of_scope.to_string_lossy().into_owned();
        g.merge_file_graph(make_fg(
            &in_str,
            Language::Cpp,
            vec![sym("a", SymbolKind::Function, &in_str)],
            vec![],
        ));
        g.merge_file_graph(make_fg(
            &out_str,
            Language::Cpp,
            vec![sym("b", SymbolKind::Function, &out_str)],
            vec![],
        ));

        let removed = g.sweep_missing_out_of_scope(&scope);
        assert_eq!(
            removed.len(),
            1,
            "only the out-of-scope missing file is dropped"
        );
        assert_eq!(removed[0], out_of_scope);
        assert!(
            g.files().contains_path(&in_scope),
            "in-scope missing file is preserved for evict_missing_in_scope"
        );
    }
}
